"""Reusable loose-script/RPA3/XP3 adapter over the fixed native parser."""
import hashlib
import json
from pathlib import Path
import shutil
import sys
import tempfile
from . import engine_archive as archive
from .gamel10n_host import parser, validation_context, write_response


def selected(root, request, config):
    files = request.get('resourceFiles')
    if files is None:
        import fnmatch
        patterns = config['resourcePatterns']
        files = [p.relative_to(root).as_posix() for p in root.rglob('*') if p.is_file()
                 and any(fnmatch.fnmatch(p.relative_to(root).as_posix().lower(), pattern.lower())
                         or pattern.startswith('**/') and fnmatch.fnmatch(p.relative_to(root).as_posix().lower(), pattern[3:].lower())
                         for pattern in patterns)]
    for relative in files:
        archive.member_name(relative)
    return files


def containers(root, files, config, scratch):
    result = []
    packed = config.get('archiveComponent')
    for index, resource in enumerate(files):
        source = archive.checked_path(root / resource)
        if packed:
            destination = scratch / str(index)
            report = archive.execute(dict(protocolVersion=1, component=packed, operation='extract',
                                          archive=str(source), outputDirectory=str(destination),
                                          filter=config.get('filter'), extensions=config['scriptExtensions']))
            scripts = report.get('extractedFiles', [])
            result.append((resource, destination, scripts))
        else:
            result.append((None, root, [resource]))
    return result


def extract(request, items, config):
    units, warnings = [], []
    for resource, directory, files in items:
        if not files:
            continue
        result = parser(request, config['format'], operation='extract', sourceDirectory=str(directory),
                        resourceFiles=files).get('extraction') or {}
        warnings += result.get('warnings', [])
        for unit in result.get('units', []):
            original_id = unit['id']
            original_resource = unit['resourcePath']
            unit = dict(unit)
            if resource:
                unit['id'] = hashlib.sha256((resource + '\0' + original_id).encode('utf-8')).hexdigest()
                unit['resourcePath'] = resource
                unit['locator'] = original_resource + '!' + unit['locator']
            units.append((unit, resource, original_id, original_resource))
    return units, warnings


def validate_units(original, modified, translations):
    after = {unit['id']: unit for unit in modified}
    issues = []
    if len(after) != len(original):
        issues.append(dict(code='count', message='回填改变了文本数量。'))
    for unit in original:
        candidate = after.get(unit['id'])
        if not candidate or candidate['sourceText'] != translations.get(unit['id'], unit['sourceText']):
            issues.append(dict(code='text', message='回填文本与预期不一致。', unitId=unit['id']))
    return dict(validation=dict(passed=not issues, issues=issues, runtimeStatus='not-tested'))


def execute(request):
    package = Path(request.get('adapterDirectory') or Path(__file__).resolve().parent.parent)
    config = json.loads((package / 'tools/adapter-template.json').read_text(encoding='utf-8-sig'))
    if config.get('schemaVersion') != 1 or config.get('templateVersion') != 1:
        raise ValueError('不支持的资源组合模板版本。')
    root = Path(request['sourceDirectory']).resolve()
    files = selected(root, request, config)
    if not files:
        return dict(probe=None) if request['operation'] == 'probe' else (_ for _ in ()).throw(ValueError('没有可处理资源。'))
    if request['operation'] == 'probe':
        # Only bounded signatures: complete parsing belongs to extract/build.
        packed = config.get('archiveComponent')
        matched = []
        for resource in files:
            with (root / resource).open('rb') as stream:
                header = stream.read(64)
            if packed == 'archive.rpa3':
                valid = header.startswith(b'RPA-3.0 ')
            elif packed == 'archive.xp3':
                valid = header.startswith(b'XP3\r\n \n\x1a\x8bg\x01')
            else:
                valid = Path(resource).suffix.lower() in config['scriptExtensions']
            if valid:
                matched.append(resource)
        if not matched:
            return dict(probe=None)
        return dict(probe=dict(engineId=config['format'], adapterId='template.' + config['templateId'],
                               adapterVersion='1', description='固定容器与脚本组合模板', requiredFiles=matched,
                               warnings=['仅覆盖解析器已支持的显示字段；游戏启动、字体和中文显示尚未测试。']))
    if request['operation'] == 'validate':
        context = validation_context(request)
        if context is not None:
            return validate_units(context['originalUnits'], context['outputUnits'], context['translations'])
    with tempfile.TemporaryDirectory(prefix='gamel10n-template-') as directory:
        scratch = Path(directory)
        items = containers(root, files, config, scratch / 'original')
        units, warnings = extract(request, items, config)
        if request['operation'] == 'extract':
            return dict(extraction=dict(units=[unit for unit, *_ in units], warnings=warnings),
                        validationContextVersion=1)
        output = Path(request['outputDirectory']).resolve()
        translations = request.get('translations') or {}
        if request.get('translationsFile'):
            translations = json.loads(Path(request['translationsFile']).read_text(encoding='utf-8-sig'))
        if request['operation'] == 'validate':
            changed = containers(output, files, config, scratch / 'changed')
            actual, _ = extract(request, changed, config)
            return validate_units([unit for unit, *_ in units], [unit for unit, *_ in actual], translations)
        if request['operation'] != 'build':
            raise ValueError('不支持的适配操作。')
        source_texts = {unit['id']: unit['sourceText'] for unit, *_ in units}
        if not isinstance(translations, dict) or translations.keys() - source_texts.keys():
            raise ValueError('部分译文 ID 无法对应当前脚本，请重新提取后再构建补丁。')
        translations = {identity: text for identity, text in translations.items() if text != source_texts[identity]}
        outputs = []
        for index, (resource, source, scripts) in enumerate(items):
            script_paths = set(scripts)
            translated = {original: translations[unit['id']] for unit, owner, original, path in units
                          if owner == resource and path in script_paths and unit['id'] in translations}
            if not translated:
                for member in [resource] if resource else scripts:
                    original = archive.checked_path(root / archive.member_name(member))
                    destination = archive.checked_path(output / archive.member_name(member))
                    archive.separate(original, destination)
                    destination.parent.mkdir(parents=True, exist_ok=True)
                    with original.open('rb') as source_stream, destination.open('xb') as target_stream:
                        shutil.copyfileobj(source_stream, target_stream, 1 << 20)
                    outputs.append(member)
                continue
            target = scratch / 'build' / str(index)
            target.mkdir(parents=True, exist_ok=True)
            result = parser(request, config['format'], operation='build', sourceDirectory=str(source),
                            resourceFiles=scripts, outputDirectory=str(target), translations=translated)
            built = result['build']['files']
            if resource:
                destination = archive.checked_path(output / archive.member_name(resource))
                destination.parent.mkdir(parents=True, exist_ok=True)
                replacements = {member: str(target / member) for member in built}
                archive.execute(dict(protocolVersion=1, component=config['archiveComponent'], operation='rebuild',
                                     archive=str(root / resource), outputFile=str(destination), replacements=replacements,
                                     filter=config.get('filter')))
                outputs.append(resource)
            else:
                for member in built:
                    destination = archive.checked_path(output / archive.member_name(member))
                    destination.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(target / member, destination)
                    outputs.append(member)
        return dict(build=dict(files=outputs, warnings=warnings))


def main():
    request = json.load(sys.stdin)
    try:
        response = dict(protocolVersion=1, success=True, **execute(request))
    except Exception as error:
        response = dict(protocolVersion=1, success=False, error=f'资源组合模板 {type(error).__name__}: {error}')
    write_response(request, response)
    return 0 if response['success'] else 1
