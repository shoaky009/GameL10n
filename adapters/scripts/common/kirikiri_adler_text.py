"""Adler variant display attributes, ruby groups, static labels and tables."""
import re

from kirikiri_text import Span, escape, scan_ks, scan_ma, scan_static_tjs, unescape

NAME_COMMANDS = {'nm', 'set_title', 'speaker', 'Talk', 'talk', 'cn', 'name', '名前'}
MESSAGE_COMMANDS = {'sel01', 'sel02', 'sel03', 'sel04', 'AddSelect', 'ruby'}
SHORT = re.compile(r'''\[(?P<base>[^\[\]\s'"=]+)'(?P<reading>[^\[\]\s'"]+)\]''')
LONG = re.compile(r'''\[gltx_ruby'v1 txt="(?P<base>(?:\\.|[^"\\])*)" gltx_reading="(?P<reading>(?:\\.|[^"\\])*)"\]''')
HOOK_NEEDLE = 'var ruby = rb[1];'
HOOK_MARKER = 'if(tagname == "gltx_ruby\'v1" && elm.gltx_reading !== void)ruby = elm.gltx_reading;'


def hook_owner(text):
    return all(needle in text for needle in ('Conductor.onTag = function(elm)', 'var rb = tagname.split("\'")', HOOK_NEEDLE, 'if(elm.txt !== void)txt = elm.txt;'))


def add_hook(text):
    if HOOK_MARKER in text:
        return text
    if text.count(HOOK_NEEDLE) != 1:
        raise ValueError('注音绘制入口与已验证结构不符，已停止回填。')
    return text.replace(HOOK_NEEDLE, HOOK_NEEDLE + ' ' + HOOK_MARKER)


def write_ruby(base, reading):
    return '[gltx_ruby\'v1 txt="' + escape(base) + '" gltx_reading="' + escape(reading) + '"]'


def attributes(row):
    # The legacy command grammar separates attributes with ASCII spaces.
    commands = list(re.finditer(r'''\[(?P<command>[^\]' ]+)(?P<attrs>(?: +[^\]= ]+(?: *= *(?:"(?:\\"|[^"])*"|'(?:\\'|[^'])*'|[^\]"' ]*))?)* *)\]''', row))
    if row.lstrip().startswith('@'):
        match = re.match(r'''^\s*@(?P<command>[^ ]+)(?P<attrs>(?: +[^= ]+(?: *= *(?:"(?:\\"|[^"])*"|'(?:\\'|[^'])*'|[^"' ]*))?)*)''', row)
        commands = [match] if match else []
    for command in commands:
        name = command['command']
        if name not in NAME_COMMANDS | MESSAGE_COMMANDS:
            continue
        attrs = list(re.finditer(r''' +(?P<key>[^\]= ]+)(?: *= *(?P<value>"(?:\\"|[^"])*"|'(?:\\'|[^'])*'|[^\]"' ]*))?''', command['attrs']))
        if any(attr['value'] is None for attr in attrs):
            continue
        for attr in attrs:
            if attr['key'] not in ('t', 'text', 'caption'):
                continue
            value = attr['value']
            quoted = value.startswith(('"', "'"))
            skip = int(quoted)
            if value[skip:].startswith('&'):
                continue
            start = command.start('attrs') + attr.start('value')
            keep_key = name == 'nm' and attr['key'] == 't' and not any(item['key'] == 'rt' for item in attrs)
            yield start + skip, len(value) - skip * 2, 'name' if name in NAME_COMMANDS else 'choice', quoted, start + len(value) if keep_key else -1, ' rt=' + value if keep_key else ''


def embedded(text):
    result = []
    for begin in re.finditer(r'^[ \t]*(?:@iscript|\[iscript\])[ \t]*\r?$', text, re.M):
        end = re.search(r'^[ \t]*(?:@endscript|\[endscript\])[ \t]*\r?$', text[begin.end():], re.M)
        if end is None:
            continue
        block = text[begin.end():begin.end() + end.start()]
        line_offset = text[:begin.end()].count('\n')
        for span in scan_tjs(block):
            # Match the exact historical 80-character constructor prefix window.
            first = max(0, span.start - 80)
            length = min(79, span.start - 1)
            prefix = block[first:first + length]
            if not visible_tjs(block, span.start) and not ('extends WIN32DialogEX' in block and re.search(r'\b(?:RText|PushButton)\s*\(\s*$', prefix)):
                continue
            span.start += begin.end()
            span.end += begin.end()
            span.line += line_offset
            result.append(span)
    return result


def display_ks(text):
    result = []
    offset, block = 0, False
    for line, row in enumerate(text.split('\n'), 1):
        trim = row.strip()
        if re.match(r'^(?:@|\[)(?:iscript|macro)(?:\s|\]|$)', trim):
            block = True
        if not block and not trim.startswith((';', '*')):
            added, cuts = [], []
            for start, length, kind, quoted, insert, key in attributes(row.rstrip('\r')):
                if length <= 0:
                    continue
                value = unescape(row[start:start + length])
                if value.strip():
                    added.append(Span(line, offset + start, offset + start + length, value, kind,
                                      'literal' if quoted else 'attribute', key_insert=offset + insert if insert >= 0 else -1,
                                      key_attribute=key))
                opening = row.rfind('[', 0, start + 1)
                ending = row.find(']', start + length)
                if opening >= 0 and ending >= 0:
                    cuts.append((opening, ending + 1))
            if not trim.startswith('@'):
                for pattern, escaped in ((SHORT, False), (LONG, True)):
                    for match in pattern.finditer(row):
                        group = dict(start=offset + match.start(), end=offset + match.end())
                        spans = []
                        for key in ('base', 'reading'):
                            value = unescape(match[key]) if escaped else match[key]
                            span = Span(line, offset + match.start(key), offset + match.end(key), value, syntax='ruby', ruby=group)
                            spans.append(span)
                            group[key] = span
                        cuts.append((match.start(), match.end()))
                        added.extend(spans)
            for original in scan_ks(row):
                intersect = sorted(set(cut for cut in cuts if cut[0] < original.end and cut[1] > original.start))
                if not intersect:
                    original.start += offset
                    original.end += offset
                    original.line = line
                    result.append(original)
                    continue
                cursor = original.start
                for start, end in intersect:
                    fragment(row, cursor, max(cursor, start), offset, line, result)
                    cursor = max(cursor, end)
                fragment(row, cursor, original.end, offset, line, result)
            result.extend(added)
        if re.match(r'^(?:@|\[)(?:endscript|endmacro)(?:\s|\]|$)', trim):
            block = False
        offset += len(row) + 1
    result.extend(embedded(text))
    return sorted(result, key=lambda span: span.start)


def fragment(row, start, end, offset, line, result):
    if end <= start:
        return
    for span in scan_ks(row[start:end]):
        span.start += offset + start
        span.end += offset + start
        span.line = line
        result.append(span)


def tables(name, text):
    result = []
    offset = 0
    rows = text.split('\n')
    if name.replace('\\', '/').rsplit('/', 1)[-1].lower() == '#stm_bg_list.csv':
        for line, raw in enumerate(rows, 1):
            cells = raw.rstrip('\r').split('\t')
            cursor = offset
            for column, cell in enumerate(cells):
                skip = -1
                if line == 1 and column >= 3 and ':' in cell:
                    skip = cell.find(':') + 1
                if line > 1 and column == 2 and len(cells) > 2 and cells[0]:
                    skip = 0
                if line > 1 and column >= 3 and ':' in cell:
                    skip = cell.find(':') + 1
                if skip >= 0 and cell[skip:].strip():
                    result.append(Span(line, cursor + skip, cursor + len(cell), cell[skip:], 'ui', 'background'))
                cursor += len(cell) + 1
            offset += len(raw) + 1
    else:
        if '\n' not in text:
            return result
        header = rows[0].rstrip('\r').split('\t')
        column = header.index('jp') if 'jp' in header else -1
        if column < 1 or 'en' not in header:
            return result
        offset = len(rows[0]) + 1
        for line, raw in enumerate(rows[1:], 2):
            row = raw.rstrip('\r')
            cells = row.split('\t')
            if not row.startswith('//') and len(cells) > column and cells[0].strip() and cells[column].strip():
                start = offset + sum(len(cell) + 1 for cell in cells[:column])
                value = cells[column]
                tokens = [match[0] for match in re.finditer(r'\\(?:[1-9][0-9]*|[nrt])|&[A-Za-z]', value)]
                result.append(Span(line, start, start + len(value), value, 'ui', 'table', tokens))
            offset += len(raw) + 1
    return result


def scan(name, text):
    extension = name.lower().rsplit('.', 1)[-1]
    if extension == 'tjs':
        return scan_static_tjs(text)
    spans = tables(name, text) if extension == 'csv' else display_ks(text) if extension == 'ks' else scan_ma(text)
    counts = {}
    for span in sorted(spans, key=lambda item: item.start):
        count = counts.get(span.line, 0)
        span.suffix = ':' + str(count) if count else ''
        counts[span.line] = count + 1
    return spans
