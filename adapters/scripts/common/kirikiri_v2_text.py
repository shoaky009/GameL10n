"""KAG3 run IDs and legacy mixed CP932/CP936 trailer decoding."""
import base64
from dataclasses import dataclass
import json
from pathlib import Path
import re
import zlib

EXTRA = {codepage: {bytes.fromhex(key): value for key, value in values.items()}
         for codepage, values in json.loads(Path(__file__).with_name('narrow-extra.json').read_text('utf-8')).items()}
VALID = {key: base64.b64decode(value) for key, value in
         json.loads(Path(__file__).with_name('narrow-valid.json').read_text('utf-8')).items()}


@dataclass
class Unit:
    locator: str
    text: str
    kind: str
    context: str
    start: int
    end: int


class Document:
    def __init__(self, data):
        self.raw = data
        self.lossless = True
        self.warning = None
        if len(data) >= 4 and data[:2] in (b'\xff\xfe', b'\xfe\xff'):
            # Read whole UTF-16 code units; the original format ignores an odd tail.
            self.text = data[2:2 + (len(data) - 2) // 2 * 2].decode('utf-16le' if data[:2] == b'\xff\xfe' else 'utf-16be', errors='surrogatepass')
            return
        content, regions, alt = markers(data)
        result = []
        at, region = 0, 0
        while at < len(content):
            while region < len(regions) and at >= sum(regions[region]):
                region += 1
            cp936 = region < len(regions) and regions[region][0] <= at < sum(regions[region])
            codepage = '936' if cp936 else '932'
            first = content[at]
            width = 1 if first < 0x80 or not cp936 and 0xa1 <= first <= 0xdf or at + 1 >= len(content) else 2
            raw = content[at:at + width]
            text = EXTRA[codepage].get(raw)
            if text is None:
                text = raw.decode('cp' + codepage, errors='replace')
            bit = raw[0] if len(raw) == 1 else 256 + int.from_bytes(raw, 'big')
            if not VALID[codepage][bit // 8] & (1 << (bit % 8)):
                self.lossless = False
            if cp936:
                logical = ''.join(alt[ord(char) - 0xe000] if 0xe000 <= ord(char) < 0xe000 + len(alt) else char for char in text)
                if logical != text:
                    self.lossless = False
                text = logical
            result.append(text)
            at += width
        self.text = ''.join(result)


def markers(data):
    scan_from, end = max(0, len(data) - 65536), len(data)
    first, saw_encoding, regions, alt = len(data), False, [], ''
    for _ in range(8):
        while end > scan_from and data[end - 1] in (10, 13):
            end -= 1
        if end <= scan_from:
            break
        start = data.rfind(b'\n', scan_from, end) + 1
        start = max(start, scan_from)
        line = data[start:end].decode('ascii', errors='replace').lstrip(' \t')
        if not line:
            end = start
            continue
        if not line.startswith((';', '//')):
            break
        if 'chrp-enc:v1:' in line and not saw_encoding:
            try:
                raw = zlib.decompress(base64.b64decode(line.split('chrp-enc:v1:', 1)[1].strip()))
                cursor, current = 0, 0
                def varint():
                    nonlocal cursor
                    value, shift = 0, 0
                    while cursor < len(raw):
                        byte = raw[cursor]
                        cursor += 1
                        value |= (byte & 0x7f) << shift
                        if not byte & 0x80:
                            return value
                        shift += 7
                        if shift > 63:
                            raise ValueError('varint 过长')
                    raise ValueError('varint 截断')
                while cursor < len(raw):
                    begin = current + varint()
                    length = varint()
                    if not length or begin + length > len(data):
                        raise ValueError('区间越界')
                    regions.append((begin, length))
                    current = begin + length
                if not regions:
                    raise ValueError('空区间')
            except Exception:
                return data, [], ''
            saw_encoding, first = True, start
        elif 'chrp-alt:v1:' in line and not alt:
            try:
                raw = base64.b64decode(line.split('chrp-alt:v1:', 1)[1].strip())
                if not raw or len(raw) % 2:
                    raise ValueError('长度非法')
                alt = raw.decode('utf-16le', errors='surrogatepass')
            except Exception:
                alt = ''
            first = start
        else:
            break
        end = start
    return (data[:first], sorted(regions), alt) if saw_encoding else (data, [], '')


def blank(text):
    return not text.strip(' \t\u3000\r\n')


def scan_ks(name, text):
    units = []
    in_script = in_link = False
    offset = 0
    for line_number, raw in enumerate(text.split('\n'), 1):
        row = raw[:-1] if raw.endswith('\r') else raw
        trim = row.lstrip(' \t\u3000')
        if in_script:
            if trim.lower().startswith(('@endscript', '[endscript')):
                in_script = False
            offset += len(raw) + 1
            continue
        if not trim or trim.startswith((';', '*', '//')):
            offset += len(raw) + 1
            continue
        if trim.lower().startswith('@iscript'):
            in_script = True
        if trim.startswith('@'):
            offset += len(raw) + 1
            continue
        run_index = attr_index = run_start = 0
        at, in_tag, tag_start, saw_script = 0, False, 0, False

        def flush(start, end):
            nonlocal run_index
            run_index += 1
            if end > start and row[end - 1] == '\\':
                end -= 1
            value = row[start:end]
            if end > start and not blank(value):
                units.append(Unit(f'{name}#L{line_number}/T{run_index}', value, 'choice' if in_link else 'dialogue',
                                  'ks-choice' if in_link else 'ks-dialogue', offset + start, offset + end))

        def handle(tag_raw):
            nonlocal in_script, in_link, attr_index, saw_script
            tag = tag_raw.strip()
            if not tag:
                return
            command = re.split('[ \t=]', tag, 1)[0].lower()
            if command == 'iscript':
                saw_script = True
                return
            if command == 'endscript':
                in_script = False
                return
            if command in ('link', 'endlink'):
                in_link = command == 'link'
                return
            for key, context, kind in (('name', 'ks-window-title' if command == 'title' else 'ks-name', 'ui' if command == 'title' else 'name'),
                                       ('hint', 'ks-link', 'ui')):
                count, cursor = 0, 0
                while cursor < len(tag):
                    char = tag[cursor]
                    if char in ('"', "'"):
                        end = tag.find(char, cursor + 1)
                        cursor = end + 1 if end >= 0 else len(tag)
                        continue
                    if not char.isalpha() and char != '_':
                        cursor += 1
                        continue
                    word = re.match(r'\w+', tag[cursor:])[0]
                    end_word = cursor + len(word)
                    if word.lower() != key:
                        cursor = end_word
                        continue
                    cursor = end_word
                    while cursor < len(tag) and tag[cursor] in ' \t':
                        cursor += 1
                    if cursor >= len(tag) or tag[cursor] != '=':
                        continue
                    cursor += 1
                    while cursor < len(tag) and tag[cursor] in ' \t':
                        cursor += 1
                    if cursor >= len(tag):
                        break
                    if tag[cursor] in ('"', "'"):
                        quote = tag[cursor]
                        start = cursor + 1
                        end = tag.find(quote, start)
                        end = end if end >= 0 else len(tag)
                        cursor = min(end + 1, len(tag))
                    else:
                        start = cursor
                        while cursor < len(tag) and tag[cursor] not in ' \t':
                            cursor += 1
                        end = cursor
                    count += 1
                    value = tag[start:end]
                    if value and not blank(value):
                        # The legacy parser intentionally anchors trimmed tags at tagStart.
                        units.append(Unit(f'{name}#L{line_number}/A{attr_index + count}', value, kind, context,
                                          offset + tag_start + start, offset + tag_start + end))
                attr_index += count

        while at < len(row):
            char = row[at]
            if not in_tag:
                if char == '[':
                    flush(run_start, at)
                    in_tag, tag_start = True, at + 1
                at += 1
                continue
            if char in ('"', "'"):
                ending = row.find(char, at + 1)
                at = ending + 1 if ending >= 0 else len(row)
            elif char == ']':
                handle(row[tag_start:at])
                run_start, in_tag = at + 1, False
                at += 1
            else:
                at += 1
        flush(len(row) if in_tag else run_start, len(row))
        if saw_script:
            in_script = True
        offset += len(raw) + 1
    return units


def skip_literal(text, start, end):
    quote, cursor = text[start], start + 1
    escaped = False
    while cursor < end and text[cursor] != quote:
        if text[cursor] == '\\':
            escaped = True
            cursor += 1
        cursor += 1
    return min(cursor, end), escaped


def scan_tjs(name, text):
    units, pos, ordinal = [], 0, 0
    while True:
        match = re.search('new KAGMenuItem', text[pos:], re.I)
        if match is None:
            break
        at = pos + match.start()
        opening = text.find('(', at)
        if opening < 0:
            break
        depth, cursor, ending = 0, opening, -1
        while cursor < len(text):
            char = text[cursor]
            if char in ('"', "'"):
                cursor, _ = skip_literal(text, cursor, len(text))
            elif char == '(':
                depth += 1
            elif char == ')':
                depth -= 1
                if depth == 0:
                    ending = cursor
                    break
            cursor += 1
        if ending < 0:
            break
        args, start, depth, cursor = [], opening + 1, 0, opening + 1
        while cursor < ending:
            char = text[cursor]
            if char in ('"', "'"):
                cursor, _ = skip_literal(text, cursor, ending)
            elif char in '([{':
                depth += 1
            elif char in ')]}':
                depth -= 1
            elif char == ',' and depth == 0:
                args.append((start, cursor))
                start = cursor + 1
            cursor += 1
        args.append((start, ending))
        if len(args) >= 2:
            start, end = args[1]
            cursor = start
            while cursor < end:
                if text[cursor] in ('"', "'"):
                    literal_end, escaped = skip_literal(text, cursor, end)
                    value = text[cursor + 1:literal_end]
                    if not escaped and any(ord(char) >= 0x80 for char in value):
                        ordinal += 1
                        units.append(Unit(f'{name}#L{text[:at].count(chr(10)) + 1}/M{ordinal}', value, 'ui', 'tjs-menu', cursor + 1, literal_end))
                    cursor = literal_end
                cursor += 1
        pos = ending + 1
    return units
