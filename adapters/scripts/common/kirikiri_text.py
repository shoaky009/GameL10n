"""Legacy KAG line slots and testo hint literals, with source character spans."""
from dataclasses import dataclass, field
import re


@dataclass
class Span:
    line: int
    start: int
    end: int
    text: str
    kind: str = 'dialogue'
    syntax: str = 'ks'
    tokens: list = field(default_factory=list)
    suffix: str = ''
    ruby: object = None
    key_insert: int = -1
    key_attribute: str = ''


def unescape(raw):
    result = []
    at = 0
    controls = dict(n='\n', r='\r', t='\t', b='\b', f='\f', v='\v', **{'0': '\0'})
    while at < len(raw):
        char = raw[at]
        at += 1
        if char == '\\' and at < len(raw):
            char = raw[at]
            at += 1
            if char == 'x' and at + 2 <= len(raw) and re.fullmatch('[0-9a-fA-F]{2}', raw[at:at + 2]):
                char = chr(int(raw[at:at + 2], 16))
                at += 2
            else:
                char = controls.get(char, char)
        result.append(char)
    return ''.join(result)


def escape(text):
    return ''.join({'\\': '\\\\', '"': '\\"', '\n': '\\n', '\r': '\\r', '\t': '\\t'}.get(char,
                   f'\\x{ord(char):02x}' if ord(char) < 32 else char) for char in text)


def tag_name(content, start):
    match = re.match(r'[\w]*', content[start + 1:])
    return match[0].lower()


def tag_end(content, start):
    quote = False
    for at in range(start + 1, len(content)):
        char = content[at]
        if char == '"':
            quote = not quote
        elif not quote:
            if char == ']':
                return at
            if char == '[':
                break
    return -1


def scan_ks(text):
    result = []
    block = False
    offset = 0
    for line, raw in enumerate(text.split('\n'), 1):
        row = raw[:-1] if raw.endswith('\r') else raw
        begin = len(row) - len(row.lstrip(' \t'))
        trim = row[begin:]
        if not trim or trim[0] in ';*':
            offset += len(raw) + 1
            continue
        if block:
            if trim.lower().startswith('@endscript'):
                block = False
            offset += len(raw) + 1
            continue
        if trim[0] == '@':
            if re.match(r'@iscript(?:[^\w]|$)', trim, re.I):
                block = True
            offset += len(raw) + 1
            continue
        if trim[0] == '[' and tag_name(row, begin) in ('iscript', 'endscript'):
            block = tag_name(row, begin) == 'iscript'
            offset += len(raw) + 1
            continue
        tags = []
        first = last = -1
        at = begin
        while at < len(row):
            if row[at] == '[':
                close = tag_end(row, at)
                if close >= 0:
                    tags.append((at, close + 1, tag_name(row, at)))
                    at = close + 1
                    continue
            if not row[at].isspace():
                if first < 0:
                    first = at
                last = at + 1
            at += 1
        if first >= 0 and last > first:
            while True:
                preceding = [tag for tag in tags if tag[1] == first and tag[2] == 'ruby']
                if not preceding:
                    break
                first = preceding[0][0]
            tokens = [row[start:end] for start, end, _ in tags if start >= first and end <= last and end - start <= 256][:64]
            visible = row[first:last]
            for token in tokens:
                visible = visible.replace(token, '')
            kind = 'dialogue'
            if '【' in visible and visible.rfind('】') > visible.find('【'):
                kind = 'name'
                for bracket in ('【', '】'):
                    if bracket not in tokens and len(tokens) < 64:
                        tokens.append(bracket)
            result.append(Span(line, offset + first, offset + last, row[first:last], kind, tokens=tokens))
        offset += len(raw) + 1
    return result


def scan_ma(text):
    result = []
    offset = 0
    for line, raw in enumerate(text.split('\n'), 1):
        row = raw.rstrip('\r')
        if row.lstrip(' \t').startswith(';'):
            offset += len(raw) + 1
            continue
        pattern = r'(?<![\w])hint[ \t]*=[ \t]*"((?:\\.|[^"\\])*)"'
        for match in re.finditer(pattern, row):
            value = unescape(match[1])
            if value.strip():
                result.append(Span(line, offset + match.start(1), offset + match.end(1), value, 'ui', 'literal'))
        offset += len(raw) + 1
    return result


def scan_tjs(text):
    result = []
    at, line = 0, 1
    while at < len(text):
        char = text[at]
        if char == '\n':
            line += 1
            at += 1
        elif text.startswith('//', at):
            end = text.find('\n', at)
            at = end if end >= 0 else len(text)
        elif text.startswith('/*', at):
            end = text.find('*/', at + 2)
            end = end + 2 if end >= 0 else len(text)
            line += text[at:end].count('\n')
            at = end
        elif char in ('"', "'"):
            start = at + 1
            at = start
            while at < len(text):
                if text[at] == '\\':
                    at += 2
                elif text[at] == char or text[at] == '\n':
                    break
                else:
                    at += 1
            if at < len(text) and text[at] == char:
                value = unescape(text[start:at])
                if value.strip():
                    result.append(Span(line, start, at, value, 'ui', 'literal'))
            at += 1
        else:
            at += 1
    return result


class Script:
    def __init__(self, data):
        self.raw = data
        self.bom = b''
        for bom, encoding in ((b'\xef\xbb\xbf', 'utf-8'), (b'\xff\xfe', 'utf-16le')):
            if data.startswith(bom):
                self.bom, self.encoding = bom, encoding
                self.text = data[len(bom):].decode(encoding)
                return
        for encoding in ('utf-8', 'cp932'):
            try:
                self.text = data.decode(encoding)
                self.encoding = encoding
                return
            except UnicodeError:
                pass
        raise ValueError('脚本编码无法识别。')

    def encode(self, text, utf16=False, fallback='utf-8'):
        if utf16:
            return b'\xff\xfe' + text.encode('utf-16le')
        try:
            return self.bom + text.encode(self.encoding)
        except UnicodeError:
            if fallback == 'utf-16le':
                return b'\xff\xfe' + text.encode('utf-16le')
            return b'\xef\xbb\xbf' + text.encode('utf-8')
