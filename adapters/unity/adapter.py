"""JSON CLI v1 for Unity SerializedFile adapter variants."""
import hashlib
import json
from pathlib import Path
import struct
import sys

from il2cpp_text import Metadata
from serialized_file import UnitySet
from text_fields import collect
from tmp_fallback import prepare

ADAPTER = 'engine.unity-textasset-dialogue-json'


def safe(root, relative):
    root = Path(root).resolve()
    path = (root / relative).resolve()
    if path == root or not path.is_relative_to(root):
        raise ValueError('资源路径越界')
    return path


def metadata_path(path):
    return Path(path).name.casefold() == 'global-metadata.dat'


def probe(request, config):
    root = request['sourceDirectory']
    files, reads = [], []
    warnings = list(config['warnings'])
    for path in request.get('resourceFiles') or []:
        candidate = safe(root, path)
        if metadata_path(path):
            if not config['metadata']: continue
            with candidate.open('rb') as stream: header = stream.read(256)
            if len(header) != 256 or header[:8] != struct.pack('<II', 0xFAB11BAF, 31) or struct.unpack_from('<i', header, 12)[0] != 17876 * 8:
                warnings.append('此版本的程序内置文案尚未适配；元数据文件未纳入补丁。')
                continue
            offset = struct.unpack_from('<i', header, 160)[0]
            size = next((p['length'] for p in request.get('probeFiles', []) if p['path'] == path), candidate.stat().st_size)
            if offset < 256 or offset + 65536 > size: raise ValueError('IL2CPP 类型表范围无效')
            sample = None
            if offset + 65536 <= candidate.stat().st_size:
                with candidate.open('rb') as stream:
                    stream.seek(offset); sample = stream.read(65536)
            else:
                for item in request.get('probeRanges', []):
                    if item['path'] == path and item['offset'] == offset and item['length'] == 65536:
                        sample = Path(item['dataPath']).read_bytes()
            if sample is None:
                reads.append(dict(path=path, offset=offset, length=65536))
            elif hashlib.sha256(sample).hexdigest() == '19a16edf8bf85629aaf0b35ed67345d80983386caafe55d00a81ecdb23205b20':
                files.append(path)
            else:
                warnings.append('此版本的程序内置文案尚未适配；元数据文件未纳入补丁。')
        else:
            if config['assetsOnly'] and not path.lower().endswith('.assets'): continue
            with candidate.open('rb') as stream: header = stream.read(64)
            if len(header) == 64 and struct.unpack_from('>I', header, 8)[0] == 22 and header[16] == 0 and header[48:].startswith(b'2022.'):
                files.append(path)
    if reads: return 'probeReads', reads
    if not any(not metadata_path(p) for p in files): return 'probe', None
    return 'probe', dict(engineId='unity-textasset-dialogue-json', adapterId=ADAPTER,
                        adapterVersion=config['legacyVersion'], description='Unity 2022 / 已验证的序列化文字与 Unicode 字体回退',
                        requiredFiles=files, warnings=warnings, originalExecutable=None)


def load(root, paths, config):
    for path in paths: safe(root, path)
    unity = UnitySet(root, [p for p in paths if not metadata_path(p)])
    fields = collect(unity, config)
    metadata = {p: Metadata(safe(root, p).read_bytes(), config['metadata']) for p in paths if metadata_path(p)}
    units = [token[2] for field in fields.values() for token in field.tokens if token[2]['sourceText'].strip()]
    for path, text in metadata.items(): units.extend(text.units(path, config['legacyVersion']))
    if not units: raise ValueError('未发现受支持的可见文字字段')
    if len({u['id'] for u in units}) != len(units): raise ValueError('文本 ID 冲突')
    return unity, fields, metadata, units


def plan(unity, fields, units, translations):
    known = {u['id']: u['sourceText'] for u in units}
    # Blank dialogue entries can occur in source tables but are not extracted.
    for field in fields.values():
        known.update((u['id'], u['sourceText']) for _, _, u in field.tokens)
    if translations.keys() - known.keys(): raise ValueError('译文引用了不存在的单元')
    if any(not isinstance(t, str) for t in translations.values()): raise ValueError('译文必须是字符串')
    changed = [value for key, value in translations.items() if value != known[key]]
    patches = {key: field.rebuild(translations) for key, field in fields.items()}
    patches.update(prepare(unity, changed))
    return patches


def run(request, config):
    if request.get('protocolVersion', 1) != 1: raise ValueError('不支持的协议版本')
    operation = request['operation']
    if operation == 'probe': return probe(request, config)
    paths, root = request['resourceFiles'], request['sourceDirectory']
    unity, fields, metadata, units = load(root, paths, config)
    if operation == 'extract': return 'extraction', dict(units=units, warnings=config['warnings'])
    translations = request.get('translations') or {}
    if request.get('translationsFile'):
        translations = json.loads(Path(request['translationsFile']).read_text(encoding='utf-8-sig'))
    output = request.get('outputDirectory') or root
    if operation == 'build':
        if Path(output).resolve() == Path(root).resolve(): raise ValueError('输出不能覆盖原始资源')
        patches = plan(unity, fields, units, translations)
        for path in paths:
            destination = safe(output, path)
            destination.parent.mkdir(parents=True, exist_ok=True)
            value = metadata[path].rebuild(path, translations) if path in metadata else unity.assets[path].rebuild({pid: data for (p, pid), data in patches.items() if p == path})
            destination.write_bytes(value)
        return 'build', dict(files=paths, warnings=config['warnings'], launchExecutable=None)
    if operation == 'validate':
        issues = []
        try:
            built, _, _, actual_units = load(output, paths, config)
            current = {u['id']: u['sourceText'] for u in actual_units}
            current.update(translations)
            patches = plan(unity, fields, units, current)
            for path in paths:
                if path in metadata:
                    if metadata[path].rebuild(path, current) != safe(output, path).read_bytes():
                        raise ValueError('IL2CPP 元数据与预期回填不符：' + path)
                    continue
                original, target = unity.assets[path], built.assets[path]
                if original.objects.keys() != target.objects.keys() or original.externals != target.externals:
                    raise ValueError('Unity 对象或依赖发生变化：' + path)
                for pid, obj in original.objects.items():
                    if target.objects[pid].class_id != obj.class_id or target.raw(pid) != patches.get((path, pid), original.raw(pid)):
                        raise ValueError(f'资源对象与预期回填不符：{path}:{pid}')
        except Exception as error:
            issues.append(dict(code='resource-mismatch', message=str(error), unitId=None))
        return 'validation', dict(passed=not issues, issues=issues, runtimeStatus='not-tested')
    raise ValueError('未知操作：' + operation)


def main():
    request = {}
    try:
        request = json.load(sys.stdin)
        config = json.loads(Path(__file__).with_name('variant.json').read_text(encoding='utf-8'))
        key, value = run(request, config)
        response = dict(protocolVersion=1, success=True, **{key: value})
    except Exception as error:
        response = dict(protocolVersion=1, success=False, error='Unity 资源处理失败：' + str(error))
    encoded = json.dumps(response, ensure_ascii=False)
    if request.get('responseFile'):
        Path(request['responseFile']).write_text(encoded, encoding='utf-8')
    else: print(encoded)


if __name__ == '__main__': main()
