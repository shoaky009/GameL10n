"""SMIL/SmileCrypt KiriKiri profile over shared XP3, SCN and KAG components."""
from contextlib import nullcontext
import json
from pathlib import Path
import sys
import traceback

from kirikiri_text import Script, escape, scan_ks, scan_static_tjs
from kirikiri_scn_text import SceneText, tokens
from gamel10n_host import validation_context
from python.engines.kirikiri_psb import Psb
from python.engines import kirikiri_scn as scn
from xp3_smil import SmileFilter, UnsupportedArchive, decode_member, read_index, read_member, rebuild

ENGINE = 'engine.kirikiri-smil-smile'
VERSION = '1'
FILTER = SmileFilter(0xd0f0ba7b, 0x7b, 0x5e)
EXTENSIONS = ('.scn', '.ks', '.tjs')
COVERAGE = '覆盖 PSB SCN 正文、显示姓名、选项与场景标题、KS 直接正文及 TJS caption/hint/title 静态字面量；编译 TJS、其他动态界面及图片和视频文字尚未确认。'


class Missing(Exception):
    def __init__(self, offset, count):
        self.offset, self.count = offset, count


class ProbeView:
    def __init__(self, request, file):
        self.size, self.position = file['length'], 0
        self.ranges = [(0, resource_path(request['sourceDirectory'], file['path']).read_bytes())]
        self.ranges.extend((part['offset'], Path(part['dataPath']).read_bytes())
                           for part in request.get('probeRanges', []) if part['path'] == file['path'])

    def tell(self):
        return self.position

    def seek(self, offset, whence=0):
        self.position = offset + (self.position if whence == 1 else self.size if whence == 2 else 0)
        return self.position

    def read(self, count=-1):
        if self.position < 0:
            raise ValueError('XP3 读取位置无效。')
        count = min(self.size - self.position, count if count >= 0 else self.size - self.position)
        output = bytearray()
        while len(output) < count:
            block = next(((start, data) for start, data in self.ranges
                          if start <= self.position < start + len(data)), None)
            if block is None:
                following = min((start for start, _ in self.ranges if start > self.position), default=self.size)
                raise Missing(self.position, min(1 << 20, following - self.position,
                                                 max(65536, count - len(output))))
            start, data = block
            part = data[self.position - start:self.position - start + count - len(output)]
            output.extend(part)
            self.position += len(part)
        return bytes(output)


def resource_path(root, relative):
    normalized = relative.replace('\\', '/')
    if (':' in normalized or normalized.startswith('/')
            or any(part in ('', '.', '..') for part in normalized.split('/'))):
        raise ValueError('资源路径无效。')
    path = Path(root).resolve() / normalized
    if not path.resolve().is_relative_to(Path(root).resolve()):
        raise ValueError('资源路径超出目录。')
    return path


def resources(request):
    files = request.get('resourceFiles')
    if files is None:
        root = Path(request['sourceDirectory'])
        files = [path.relative_to(root).as_posix() for path in root.rglob('*')
                 if path.is_file() and path.suffix.lower() == '.xp3']
    return sorted((file for file in files if file.lower().endswith('.xp3')), key=str.lower)


def probe(request):
    files = request.get('probeFiles')
    if files is None:
        files = [dict(path=file, length=resource_path(request['sourceDirectory'], file).stat().st_size)
                 for file in resources(request)]
    reads, selected = [], []
    for file in files:
        if not file['path'].lower().endswith('.xp3'):
            continue
        source = (nullcontext(ProbeView(request, file)) if request.get('probeFiles') is not None
                  else resource_path(request['sourceDirectory'], file['path']).open('rb'))
        try:
            with source as stream:
                index = read_index(stream)
                candidates = sorted((entry for entry in index.entries
                                     if entry.name.lower().endswith(EXTENSIONS)
                                     and 0 < max(entry.raw_size, entry.packed_size) <= 524288
                                     and entry.alias != 'startup.tjs'), key=lambda entry: entry.packed_size)
                verified = 0
                for entry in candidates[:2]:
                    data, filtered = decode_member(stream, entry, FILTER, max_size=524288)
                    if entry.name.lower().endswith('.scn'):
                        Psb(data)
                    elif not data.startswith(b'TJS2100'):
                        Script(data)
                    verified += int(filtered)
                if verified >= 2:
                    selected.append(file['path'])
        except Missing as missing:
            reads.append(dict(path=file['path'], offset=missing.offset, length=missing.count))
        except (ValueError, EOFError):
            continue
    if reads:
        return dict(probeReads=reads)
    if not selected:
        return dict(probe=None)
    return dict(probe=dict(engineId=ENGINE, adapterId=ENGINE, adapterVersion=VERSION,
                           description='KiriKiri SMIL 文件名映射 / SmileCrypt / PSB SCN、KS 与静态 TJS 界面',
                           requiredFiles=selected, originalExecutable=None,
                           warnings=[COVERAGE, '中文按 Unicode 回填并保留原封包结构；实际启动、字体及排版仍需游戏内验证。']))


def translations(request):
    if request.get('translationsFile'):
        value = json.loads(Path(request['translationsFile']).read_text('utf-8-sig'))
    else:
        value = request.get('translations') or {}
    if not isinstance(value, dict) or any(not isinstance(k, str) or not isinstance(v, str) for k, v in value.items()):
        raise ValueError('译文必须是文本 ID 到字符串的映射。')
    return value


def document(entry, data):
    if entry.name.lower().endswith('.scn'):
        parsed = Psb(data)
        if 'scenes' not in scn.fields(parsed, parsed.root, ()):
            return None, [], 0
        scene = SceneText(data, speaker_name=False, allow_partial=True)
        fields = [(path, text, kind, tokens(text) if kind in ('dialogue', 'choice') else [])
                  for path, text, kind, _ in scene.fields]
        return scene, fields, len(scene.skipped_fields)
    if data.startswith(b'TJS2100'):
        return None, [], 1
    source = Script(data)
    spans = scan_ks(source.text) if entry.name.lower().endswith('.ks') else scan_static_tjs(source.text)
    source.spans = {}
    for span in spans:
        locator = 'L' + str(span.line) + span.suffix
        if locator in source.spans:
            raise ValueError('脚本槽位重复。')
        source.spans[locator] = span
    return source, [(path, span.text, span.kind, span.tokens) for path, span in source.spans.items()], 0


def changed_document(doc, changes):
    if isinstance(doc, SceneText):
        return doc.build(changes)
    edits = []
    for path, value in changes.items():
        span = doc.spans[path]
        if '\0' in value or span.syntax != 'literal' and ('\r' in value or '\n' in value):
            raise ValueError('脚本文本槽不接受 NUL，正文行不接受实际换行。')
        if span.syntax == 'literal':
            value = escape(value)
            if doc.text[span.start - 1] == "'":
                value = value.replace("'", "\\'")
        edits.append((span.start, span.end, value))
    text = doc.text
    for start, end, value in sorted(edits, reverse=True):
        text = text[:start] + value + text[end:]
    return doc.encode(text, fallback='utf-16le')


def extract_or_build(request):
    building = request['operation'] == 'build'
    trans = translations(request) if building else {}
    by_member = {}
    for identifier in trans:
        parts = identifier.split('|', 2)
        if len(parts) != 3:
            raise ValueError('译文文本 ID 格式无效。')
        by_member.setdefault((parts[0], parts[1]), set()).add(parts[2])
    units, known_ids, warnings, output_files = [], set(), [], []
    skipped, compiled = 0, 0
    if building:
        source, output = Path(request['sourceDirectory']).resolve(), Path(request['outputDirectory']).resolve()
        if source.is_relative_to(output) or output.is_relative_to(source):
            raise ValueError('输出目录必须独立于原资源。')
    for resource in resources(request):
        path = resource_path(request['sourceDirectory'], resource)
        replacements = {}
        with path.open('rb') as stream:
            try:
                index = read_index(stream)
            except (ValueError, EOFError) as error:
                raise UnsupportedArchive(str(error)) from error
            for entry in index.entries:
                if not entry.name.lower().endswith(EXTENSIONS):
                    continue
                if building and (resource, entry.alias) not in by_member:
                    continue
                data = read_member(stream, entry, FILTER)
                try:
                    doc, fields, unsupported = document(entry, data)
                except ValueError as error:
                    raise UnsupportedArchive(f'{entry.name}：{error}') from error
                if data.startswith(b'TJS2100'):
                    compiled += 1
                else:
                    skipped += unsupported
                    if unsupported:
                        warnings.append(f'{entry.name}：{unsupported} 个字段的控制符或派生数据不在已确认范围，保留原文。')
                changes = {}
                for locator, text, kind, protected in fields:
                    if not text.strip():
                        continue
                    identifier = resource + '|' + entry.alias + '|' + locator
                    if identifier in known_ids:
                        raise ValueError('文本 ID 重复。')
                    known_ids.add(identifier)
                    if not building:
                        unit = dict(id=identifier, resourcePath=resource, locator=entry.name + '|' + locator,
                                    sourceText=text, context=entry.name, speaker=None, kind=kind)
                        if protected:
                            unit['protectedTokens'] = protected
                        units.append(unit)
                    elif identifier in trans and trans[identifier] != text:
                        changes[locator] = trans[identifier]
                if changes:
                    replacements[entry.alias] = changed_document(doc, changes)
        if building:
            rebuild(path, resource_path(request['outputDirectory'], resource), index, replacements, FILTER)
            output_files.append(resource)
    if building and set(trans) - known_ids:
        raise ValueError('译文包含当前资源中不存在的文本 ID。')
    if compiled:
        warnings.append(f'保留 {compiled} 个编译 TJS 成员，未将常量池字符串直接作为显示文本。')
    if skipped:
        warnings.append(f'共 {skipped} 个未确认字段保留原文，其余字段可独立回填。')
    warnings += [COVERAGE]
    if building:
        return dict(build=dict(files=output_files, warnings=warnings, launchExecutable=None))
    return dict(extraction=dict(units=units, warnings=warnings))


def validate(request):
    context = validation_context(request)
    if context is not None:
        before, after = context['originalUnits'], context['outputUnits']
    else:
        before = extract_or_build(dict(request, operation='extract'))['extraction']['units']
        after = extract_or_build(dict(request, operation='extract', sourceDirectory=request['outputDirectory']))['extraction']['units']
    expected = {unit['id']: unit['sourceText'] for unit in before}
    trans = translations(request)
    expected.update({key: text for key, text in trans.items() if key in expected})
    actual = {unit['id']: unit['sourceText'] for unit in after}
    passed = expected == actual and len(actual) == len(after) and not set(trans) - expected.keys()
    issues = [] if passed else [dict(code='text-readback', message='回填后的文本、身份或未修改项与预期不一致。', unitId=None)]
    return dict(validation=dict(passed=passed, issues=issues, runtimeStatus='not-tested'))


def execute(request):
    operation = request['operation']
    if operation == 'probe':
        return probe(request)
    if operation == 'validate':
        return validate(request)
    if operation in ('extract', 'build'):
        return extract_or_build(request)
    raise ValueError('不支持的适配器操作。')


def main():
    request = {}
    try:
        request = json.load(sys.stdin)
        result = dict(protocolVersion=1, success=True, validationContextVersion=1, **execute(request))
    except Exception as error:
        traceback.print_exc(file=sys.stderr)
        result = dict(protocolVersion=1, success=False, error=str(error))
        if isinstance(error, UnsupportedArchive):
            result['errorKind'] = 'incompatible'
    if request.get('responseFile'):
        Path(request['responseFile']).write_text(json.dumps(result, ensure_ascii=False), 'utf-8')
    else:
        print(json.dumps(result, ensure_ascii=False))


if __name__ == '__main__':
    main()
