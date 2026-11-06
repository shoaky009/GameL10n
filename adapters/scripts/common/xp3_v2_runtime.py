"""KiriKiri v2: later archives own slots; changed scripts use UTF-16LE."""
import json
from pathlib import Path
import sys
import traceback

from kirikiri_v2_text import Document, blank, scan_ks, scan_tjs
from xp3_container import Archive
from xp3_probe import Missing, View
from xp3_adler import MAGIC, read_index
from xp3_runtime import apply, path

ENGINE = 'engine.kirikiri-xp3-v2'
DESCRIPTION = 'Kirikiri 2 XP3：扫描 .ks/.tjs 文本单元，按条目重建封包；含改动的脚本改写为 UTF-16LE(BOM)，运行时按 Unicode 读取，不依赖 ACP 代码页'
COVERAGE = 'TJS 中除 new KAGMenuItem 菜单项以外的界面字面量尚未确认显示消费者，暂未提取；.ks 中标签参数与脚本代码不计入文本。'


def order(files):
    return sorted(files, key=lambda file: (0 if Path(file).name.lower() == 'data.xp3' else 1, Path(file).name.lower()))


def candidates(request):
    root = Path(request['sourceDirectory'])
    files = list(request.get('resourceFiles') or [])
    for file in root.rglob('*.xp3'):
        relative = file.relative_to(root).as_posix()
        if relative.lower() not in [item.lower() for item in files]:
            files.append(relative)
    return order([file for file in files if file.lower().endswith('.xp3')])


def probe(request):
    files = request.get('probeFiles') or [dict(path=file, length=path(request['sourceDirectory'], file).stat().st_size) for file in candidates(request)]
    reads, selected, warnings = [], [], []
    for file in files:
        try:
            view = View(request, file)
            index = read_index(view)
            if any(entry.name.lower().endswith(('.ks', '.tjs')) for entry in index.entries):
                selected.append(file['path'])
        except Missing as missing:
            reads.append(dict(path=file['path'], offset=missing.offset, length=missing.count))
        except Exception as error:
            warnings.append(file['path'] + '：' + str(error))
    if reads:
        return dict(probeReads=reads)
    if not selected:
        raise ValueError('XP3 索引中没有发现 .ks/.tjs 脚本条目')
    return dict(probe=dict(engineId=ENGINE, adapterId=ENGINE, adapterVersion='2026.1', description=DESCRIPTION,
                           requiredFiles=order(selected), originalExecutable=None, warnings=warnings + [COVERAGE]))


def translations(request):
    result = {}
    try:
        value = request.get('translations')
        if request.get('translationsFile'):
            file = Path(request['translationsFile'])
            if not file.is_absolute() and (Path(request['outputDirectory']) / file).exists():
                file = Path(request['outputDirectory']) / file
            if not file.exists():
                return result
            value = json.loads(file.read_text('utf-8-sig'))
    except (OSError, ValueError):
        return result

    def item(value):
        if not isinstance(value, dict) or not value.get('id'):
            return
        for key in ('targetText', 'text', 'translation', 'target', 'zh'):
            if isinstance(value.get(key), str):
                result[value['id']] = value[key]
                return

    def collect(value):
        if isinstance(value, list):
            for record in value:
                item(record)
        elif isinstance(value, dict):
            if isinstance(value.get('units'), list):
                for record in value['units']:
                    item(record)
                return
            if isinstance(value.get('translations'), dict):
                collect(value['translations'])
                return
            is_map = True
            for key, text in value.items():
                if isinstance(text, str):
                    result[key] = text
                else:
                    is_map = False
            if not is_map:
                for child in value.values():
                    collect(child)
    collect(value)
    return result


def execute(request):
    operation = request['operation'].lower()
    if operation == 'probe':
        return probe(request)
    warnings, loaded, owners = [], [], {}
    for resource in candidates(request):
        try:
            archive = Archive(path(request['sourceDirectory'], resource), 'v2')
            scripts = [entry for entry in archive.entries if entry.name.lower().endswith(('.ks', '.tjs'))]
            if not scripts:
                continue
            loaded.append((resource, archive, scripts))
            for entry in scripts:
                owners[entry.name.lower()] = resource
        except Exception as error:
            warnings.append(resource + ': XP3 打开失败（' + str(error) + '）')
    units, changes, ids = [], {}, set()
    trans = translations(request) if operation == 'build' else {}
    parsed_entries = 0
    for resource, archive, entries in loaded:
        replaced = changes[resource] = {}
        for entry in entries:
            if owners[entry.name.lower()] != resource:
                continue
            try:
                data = archive.read(entry)
                document = Document(data)
                name = entry.name.replace('\\', '/')
                local = scan_tjs(name, document.text) if name.lower().endswith('.tjs') else scan_ks(name, document.text)
            except Exception as error:
                warnings.append(resource + '/' + entry.name + ': 脚本解析失败（' + str(error) + '）')
                continue
            parsed_entries += 1
            edits = []
            for unit in local:
                if blank(unit.text):
                    continue
                identifier = resource + '|' + unit.locator
                if identifier in ids:
                    duplicate = 1
                    while identifier + '#' + str(duplicate) in ids:
                        duplicate += 1
                    identifier += '#' + str(duplicate)
                ids.add(identifier)
                units.append(dict(id=identifier, resourcePath=resource, locator=unit.locator, sourceText=unit.text,
                                  context=unit.context, speaker=None, kind=unit.kind))
                text = trans.get(resource + '|' + unit.locator)
                if text is not None and text.strip() and text != unit.text:
                    edits.append((unit.start, unit.end, text.replace('\r', ' ').replace('\n', ' ')))
            if edits:
                if not document.lossless:
                    warnings.append(resource + '/' + entry.name + ': 脚本含当前窄编码无法无损解码的字节，整条目保留原文以免破坏正文')
                else:
                    replaced[entry.name] = b'\xff\xfe' + apply(document.text, edits).encode('utf-16le', errors='surrogatepass')
    if operation == 'extract':
        return dict(extraction=dict(units=units, warnings=warnings[:80] + [COVERAGE]))
    if operation == 'validate':
        issues = []
        if not parsed_entries:
            issues.append(dict(code='no-script', message='未找到可解析的 .ks/.tjs 脚本条目', unitId=None))
        elif not units:
            issues.append(dict(code='no-text', message='脚本条目中未提取到任何文本单元', unitId=None))
        return dict(validation=dict(passed=not issues, issues=issues, runtimeStatus='not-tested'))
    if operation != 'build':
        raise ValueError('未知操作: ' + operation)
    source, output = Path(request['sourceDirectory']).resolve(), Path(request['outputDirectory']).resolve()
    if source.is_relative_to(output) or output.is_relative_to(source):
        raise ValueError('输出目录必须独立于原资源目录。')
    files = []
    for resource, archive, entries in loaded:
        archive.build(path(output, resource), changes[resource])
        files.append(resource)
    return dict(build=dict(files=files, warnings=warnings[:80] + [COVERAGE], launchExecutable=None))


def main():
    request = {}
    try:
        request = json.load(sys.stdin)
        response = dict(protocolVersion=1, success=True, error=None, **execute(request))
    except Exception as error:
        traceback.print_exc(file=sys.stderr)
        response = dict(protocolVersion=1, success=False, error=str(error))
    text = json.dumps(response, ensure_ascii=False)
    if request.get('responseFile'):
        Path(request['responseFile']).write_text(text, 'utf-8')
    else:
        print(text)
