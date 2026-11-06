"""The two historical QLIE text contracts, retaining their exact unit IDs."""
from dataclasses import dataclass
import re


@dataclass
class Segment:
    id: str
    kind: str
    text: str
    start: int
    end: int
    identity: str | None = None
    insert_identity: bool = False
    speaker: str | None = None


class Encoding:
    def __init__(self, aliases=None):
        self.aliases = {int(k): int(v) for k, v in (aliases or {}).items()}
        self.reverse = {v: k for k, v in self.aliases.items()}
        if len(self.reverse) != len(self.aliases):
            raise ValueError('字形映射包含重复字符。')

    def decode(self, data):
        return ''.join(chr(self.aliases.get(ord(c), ord(c))) for c in data.decode('cp932'))

    def encode(self, text):
        return ''.join(chr(self.reverse.get(ord(c), ord(c))) for c in text).encode('cp932')

    def add_text(self, text, reserved):
        for c in text:
            if ord(c) in self.reverse:
                continue
            try:
                c.encode('cp932')
                continue
            except UnicodeEncodeError:
                pass
            slot = next((n for n in range(0xe000, 0xe758) if n not in self.aliases and n not in reserved), None)
            if slot is None:
                raise ValueError('新增字符超过可用字库容量（最多1880字）。')
            self.aliases[slot] = ord(c)
            self.reverse[ord(c)] = slot


def decode(data):
    if data.startswith(b'\xff\xfe'):
        return data[2:].decode('utf-16le')
    if data.startswith(b'\xfe\xff'):
        raise ValueError('不支持大端脚本。')
    return data.decode('cp932')


def encode_unicode(text):
    return b'\xff\xfe' + text.encode('utf-16le')


def same_encoding(original, text):
    return encode_unicode(text) if original.startswith(b'\xff\xfe') else text.encode('cp932')


def body(text, modern):
    if modern:
        text = text.lstrip(' \t')
        return bool(text and text[0] not in '\\@^％【' and text.strip())
    return bool(text and not text[0].isspace() and text[0] not in '\\@^％')


class Script:
    def __init__(self, data, codec=None, modern=False, unicode=False):
        self.original = data
        self.codec = codec or Encoding()
        self.modern = modern
        self.unicode = unicode
        self.text = decode(data) if unicode else self.codec.decode(data)
        self.segments = []
        self._scan_unicode() if unicode else self._scan_legacy()

    def _scan_legacy(self):
        pos, line = 0, 0
        while pos < len(self.text):
            line += 1
            end = self.text.find('\n', pos)
            if end < 0:
                end = len(self.text)
            stop = end-1 if end > pos and self.text[end-1] == '\r' else end
            text = self.text[pos:stop]
            indent = len(text)-len(text.lstrip(' \t')) if self.modern else 0
            command = text[indent:]
            def add(kind, start, end, field=0):
                if end > start:
                    self.segments.append(Segment(f'L{line:06d}:{kind}:{field}', kind,
                                                  self.text[start:end], start, end))
            if command.startswith('^select,'):
                at, field = indent+8, 0
                while at <= len(text):
                    stop = text.find(',', at)
                    if stop < 0:
                        stop = len(text)
                    field += 1
                    add('choice', pos+at, pos+stop, field)
                    if stop == len(text):
                        break
                    at = stop+1
            elif len(command) >= 2 and command.startswith('【') and command.endswith('】'):
                add('name', pos+indent+1, pos+len(text)-1)
            elif body(text, self.modern):
                add('text', pos+indent, pos+len(text))
            pos = end+1 if end < len(self.text) else end

    def _scan_unicode(self):
        # VNTextPatch's QLIE dialect ranges are intentionally retained here:
        # stripping only ASCII indentation, comma records, and trailing spaces.
        ordinal, speaker, pos = 0, None, 0
        for found in re.finditer(r'[^\r\n]*(?:\r\n|\r|\n|$)', self.text):
            line = found[0]
            if not line:
                continue
            raw_line = line.rstrip('\r\n')
            indent = len(raw_line)-len(raw_line.lstrip(' \t'))
            text = raw_line[indent:].rstrip(' \t')
            start = pos+indent
            ranges = []
            if text.startswith('^select,'):
                at = 8
                for arg in text[8:].split(','):
                    ranges.append(('choice', start+at, len(arg)))
                    at += len(arg)+1
            elif not text or text[0] in '@^\\％':
                pass
            elif text.startswith('【') and text.endswith('】'):
                ranges.append(('name', start+1, len(text)-2))
            elif (match := re.match(r'^\w+,([^,]+),(.+)', text)) is not None:
                ranges.extend([('name', start+match.start(1), len(match[1])),
                               ('dialogue', start+match.start(2), len(match[2]))])
            else:
                ranges.append(('dialogue', start, len(text)))
            for kind, at, length in ranges:
                if not length:
                    continue
                raw = self.text[at:at+length]
                def add(kind, a, z):
                    nonlocal ordinal
                    if z > a and self.text[a:z].strip():
                        ordinal += 1
                        self.segments.append(Segment(f'U{ordinal:07d}', kind, self.text[a:z], a, z, speaker=speaker))
                if kind == 'name':
                    split = raw.find('＠')
                    identity = raw[:split] if split >= 0 else raw
                    speaker = identity
                    offset = at+split+1 if split >= 0 else at
                    if offset < at+length:
                        ordinal += 1
                        self.segments.append(Segment(f'U{ordinal:07d}', 'name', self.text[offset:at+length],
                                                      offset, at+length, identity, split < 0, identity))
                else:
                    cursor = at
                    for match in re.finditer(r'\[pc,([^\]\r\n]*)\]', raw):
                        add(kind, cursor, at+match.start())
                        add('choice', at+match.start(1), at+match.end(1))
                        cursor = at+match.end()
                    add(kind, cursor, at+length)
            pos += len(line)

    def build(self, edits):
        known = {s.id: s for s in self.segments}
        for key, value in edits.items():
            if key not in known:
                raise ValueError('未知脚本文本标识：' + key)
            s = known[key]
            if not isinstance(value, str):
                raise ValueError('译文不能为空值。')
            if value == s.text:
                continue
            if any(c in value for c in '\r\n\0'):
                raise ValueError('译文不能包含物理换行或空字符。')
            forbidden = ',[]' if self.unicode else ','
            if s.kind == 'choice' and any(c in value for c in forbidden):
                raise ValueError('选项译文包含命令分隔符。')
            if s.kind == 'name' and any(c in value for c in ('＠【】,' if self.unicode else '【】')):
                raise ValueError('姓名译文包含姓名分隔符。')
            if re.findall(r'\[[^\]\r\n]*\]', s.text) != re.findall(r'\[[^\]\r\n]*\]', value):
                raise ValueError('请保留译文中的格式控制标记。')
            if s.kind in ('text', 'dialogue'):
                valid = body(value, self.modern) if not self.unicode else bool(value.strip() and value.lstrip(' \t')[0] not in '@^\\％【')
                # Historical build validation also disallows a leading name marker.
                if not valid or (not self.modern and not self.unicode and value.startswith('【')):
                    raise ValueError('正文译文不能变为控制行或空行。')
        if not any(v != known[k].text for k, v in edits.items()):
            return self.original
        parts, at, byte_at = [], 0, 0
        for s in self.segments:
            value = edits.get(s.id, s.text)
            if self.unicode:
                parts.append(self.text[at:s.start])
                parts.append((s.identity+'＠' if s.insert_identity and value != s.text else '')+value)
            else:
                begin = byte_at+len(self.codec.encode(self.text[at:s.start]))
                end = begin+len(self.codec.encode(self.text[s.start:s.end]))
                parts.append(self.original[byte_at:begin])
                parts.append(self.codec.encode(value) if value != s.text else self.original[begin:end])
                byte_at = end
            at = s.end
        if self.unicode:
            return encode_unicode(''.join(parts)+self.text[at:])
        return b''.join(parts)+self.original[byte_at:]
