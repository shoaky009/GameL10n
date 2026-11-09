"""XP3 protocol glue retaining the original line IDs and archive precedence."""
import json
from pathlib import Path
import sys
import traceback

from kirikiri_text import Script, escape, scan_ks, scan_ma
from xp3_container import Archive, MAGIC


def translations(request):
    value = request.get('translations') or {}
    if request.get('translationsFile') and Path(request['translationsFile']).exists():
        value = json.loads(Path(request['translationsFile']).read_text('utf-8-sig'))
    result = {}

    def collect(value):
        if isinstance(value, list):
            for item in value:
                collect(item)
        elif isinstance(value, dict):
            if isinstance(value.get('id'), str):
                text = next((value[key] for key in ('text', 'target', 'translation', 'translatedText') if isinstance(value.get(key), str)), None)
                if text is not None:
                    result[value['id']] = text
                return
            for key, item in value.items():
                if key in ('units', 'translations', 'items'):
                    collect(item)
                elif isinstance(item, str):
                    result[key] = item
                elif isinstance(item, dict):
                    text = next((item[key] for key in ('text', 'target', 'translation', 'translatedText') if isinstance(item.get(key), str)), None)
                    if text is not None:
                        result[key] = text
    collect(value)
    return result


def path(root, relative):
    normalized = relative.replace('\\', '/')
    if Path(normalized).is_absolute() or '..' in normalized.split('/'):
        raise ValueError('资源路径不安全。')
    target = Path(root).resolve() / normalized
    if not target.resolve().is_relative_to(Path(root).resolve()):
        raise ValueError('资源路径超出目录。')
    return target


def priority(name):
    name = Path(name).name.lower()
    return (0 if name == 'patch.xp3' else 1 if name.startswith('patch') and name.endswith('.xp3') else 2)


def archives(request, probe=False):
    root = request['sourceDirectory']
    files = [item['path'] for item in request.get('probeFiles', [])] if probe else request.get('resourceFiles', [])
    if not files:
        files = request.get('resourceFiles', [])
    result = []
    for file in files:
        file = file.replace('\\', '/')
        if not file.lower().endswith('.xp3'):
            continue
        with path(root, file).open('rb') as stream:
            if stream.read(len(MAGIC)) == MAGIC and file.lower() not in [item.lower() for item in result]:
                result.append(file)
    if not result and not probe:
        result = [file.name for file in Path(root).glob('*.xp3') if file.open('rb').read(len(MAGIC)) == MAGIC]
    if not result:
        raise ValueError('未找到 KiriKiri XP3 归档。')
    return sorted(result, key=lambda value: (priority(value), value.lower()))


def apply(text, replacements):
    cursor = 0
    result = []
    for start, end, replacement in sorted(replacements, key=lambda item: item[0]):
        if start < cursor:
            continue
        result.extend((text[cursor:start], replacement))
        cursor = end
    result.append(text[cursor:])
    return ''.join(result)


def execute(request, variant):
    operation = request.get('operation', 'probe')
    if operation == 'probe':
        from xp3_probe import adler_probe, rot_probe
        return adler_probe(request) if variant == 'adler' else rot_probe(request)
    if operation == 'validate':
        return dict(validation=dict(passed=True, issues=[], runtimeStatus='not-tested'))
    selected = archives(request)
    seen, ids = set(), set()
    units, warnings, loaded = [], [], []
    trans = translations(request) if operation == 'build' else {}
    ruby_needed = False
    ruby_owners = []
    for resource in selected:
        archive = Archive(path(request['sourceDirectory'], resource), variant)
        changes = {}
        for entry in archive.entries:
            extensions = ('.ks', '.ma', '.tjs', '.csv') if variant == 'adler' else ('.ks', '.ma')
            if not entry.name.lower().endswith(extensions) or entry.name.lower() in seen:
                continue
            try:
                data = archive.read(entry)
            except Exception as error:
                warnings.append(f'{entry.name}: 读取失败（{error}）')
                continue
            seen.add(entry.name.lower())
            try:
                script = Script(data)
                if variant == 'adler':
                    from kirikiri_adler_text import scan, hook_owner
                    spans = scan(entry.name, script.text)
                    if hook_owner(script.text):
                        ruby_owners.append((resource, entry.name, script, changes))
                else:
                    spans = scan_ma(script.text) if entry.name.lower().endswith('.ma') else scan_ks(script.text)
            except (ValueError, UnicodeError) as error:
                warnings.append(f'{entry.name}: 编码无法识别（{error}）')
                continue
            edits, ruby_done = [], set()
            for span in spans:
                identifier = entry.name + '#L' + str(span.line) + span.suffix
                if operation == 'extract':
                    if not span.text.strip() or identifier in ids:
                        continue
                    ids.add(identifier)
                    unit = dict(id=identifier, resourcePath=resource, locator=identifier, sourceText=span.text,
                                context=f'{entry.name} 第{span.line}行', speaker=None, kind=span.kind)
                    if span.tokens:
                        unit['protectedTokens'] = list(dict.fromkeys(span.tokens))
                    units.append(unit)
                elif identifier in trans:
                    text = trans[identifier]
                    if text == span.text:
                        continue
                    if not text:
                        warnings.append(identifier + ': 译文为空或含换行，已跳过该条')
                        continue
                    if variant == 'adler':
                        if span.syntax != 'literal' and ('\n' in text or '\r' in text):
                            raise ValueError(identifier + ': 此脚本槽不接受实际换行，请保留引擎换行标签。')
                        if span.syntax == 'ruby':
                            if span.ruby['start'] not in ruby_done:
                                from kirikiri_adler_text import write_ruby
                                ruby_done.add(span.ruby['start'])
                                def value(part):
                                    key = entry.name + '#L' + str(part.line) + part.suffix
                                    target = trans.get(key)
                                    return target if target and target.strip() else part.text
                                edits.append((span.ruby['start'], span.ruby['end'], write_ruby(value(span.ruby['base']), value(span.ruby['reading']))))
                                ruby_needed = True
                            continue
                        if span.syntax in ('table', 'background') and '\t' in text:
                            raise ValueError(identifier + ': 表格文本不能包含制表符。')
                        if span.syntax == 'background' and any(char in text for char in '\"\'\\'):
                            raise ValueError(identifier + ': 背景列表显示名称不能包含引号或反斜线。')
                        written = escape(text) if span.syntax == 'literal' else text
                        if span.syntax == 'attribute' and any(char.isspace() or char in '[]\"\'=\\' for char in text):
                            written = '\"' + escape(text) + '\"'
                        edits.append((span.start, span.end, written))
                        if span.key_insert >= 0:
                            edits.append((span.key_insert, span.key_insert, span.key_attribute))
                    else:
                        if '\n' in text or '\r' in text:
                            warnings.append(identifier + ': 译文含换行，已跳过该条')
                            continue
                        edits.append((span.start, span.end, escape(text) if span.syntax == 'literal' else text))
            if edits:
                changes[entry.name] = script.encode(apply(script.text, edits), fallback='utf-16le' if variant == 'adler' else 'utf-8')
        loaded.append((resource, archive, changes))
    if operation == 'extract':
        return dict(extraction=dict(units=units, warnings=warnings + ['.tjs 脚本内 UI/系统消息与图片/视频内嵌文字未提取。']))
    if operation != 'build':
        raise ValueError('未知操作。')
    if ruby_needed:
        if len(ruby_owners) != 1:
            raise ValueError('未找到唯一的注音绘制入口，已停止回填。')
        from kirikiri_adler_text import add_hook
        resource, member, script, changes = ruby_owners[0]
        current = Script(changes[member]) if member in changes else script
        changes[member] = current.encode(add_hook(current.text), fallback='utf-16le')
    source, output = Path(request['sourceDirectory']).resolve(), Path(request['outputDirectory']).resolve()
    if source.is_relative_to(output) or output.is_relative_to(source):
        raise ValueError('输出目录必须独立于原资源目录。')
    for resource, archive, changes in loaded:
        archive.build(path(output, resource), changes)
    return dict(build=dict(files=selected, warnings=warnings, launchExecutable=None))


def main(variant='rot'):
    request = {}
    try:
        request = json.load(sys.stdin)
        result = dict(protocolVersion=1, success=True, error=None, **execute(request, variant))
    except Exception as error:
        traceback.print_exc(file=sys.stderr)
        result = dict(protocolVersion=1, success=False, error=str(error))
    text = json.dumps(result, ensure_ascii=False)
    if request.get('responseFile'):
        Path(request['responseFile']).write_text(text, 'utf-8')
    else:
        print(text)
