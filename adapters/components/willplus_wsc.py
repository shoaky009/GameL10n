"""Rotated WSC members with the eight-byte FileEnd operand (WSC v2 subset)."""
from bisect import bisect_right
from collections import Counter
from dataclasses import dataclass
from pathlib import Path
import re
import struct


# Total instruction sizes and distances to NUL-terminated operands.
FIXED = {
    0x01: 11, 0x03: 8, 0x04: 1, 0x05: 3, 0x06: 6, 0x0A: 2, 0x0B: 3, 0x0D: 8,
    0x22: 5, 0x26: 3, 0x28: 6, 0x29: 5, 0x30: 5, 0x45: 5, 0x47: 3,
    0x49: 4, 0x4A: 7, 0x4B: 21, 0x4C: 9, 0x4D: 14, 0x4E: 5, 0x4F: 5,
    0x51: 6, 0x52: 3, 0x55: 2, 0x62: 2, 0x63: 4, 0x64: 9, 0x65: 6,
    0x66: 23, 0x68: 10, 0x69: 3, 0x70: 9, 0x72: 2, 0x74: 3, 0x82: 4,
    0x83: 2, 0x85: 3, 0x89: 2, 0x8A: 2, 0x8B: 2, 0x8C: 4, 0x8E: 2,
    0xAB: 2, 0xB1: 6, 0xB8: 4, 0xB9: 4, 0xBC: 5, 0xBD: 3, 0xE2: 2, 0xE4: 3,
}
STRINGS = {
    0x07: (1,), 0x09: (1,), 0x21: (11,), 0x23: (10,), 0x25: (12,),
    0x41: (5,), 0x42: (6, 0), 0x43: (7,), 0x46: (10,), 0x48: (12,),
    0x50: (1,), 0x54: (1,), 0x61: (2,), 0x73: (10,),
}
DECODE = bytes((b >> 2) | ((b << 6) & 255) for b in range(256))
ENCODE = bytes(((b << 2) & 255) | (b >> 6) for b in range(256))
CONTROLS = re.compile(r'\\n|%[KP]')
MAX_SIZE = 8 << 20


def read_index(stream, *, max_entries, max_index_size, record_offsets=None):
    from python.archives import willplus
    index = willplus.read_index(stream, version=1, name_size=13, max_entries=max_entries,
                                max_index_size=max_index_size)
    if record_offsets is not None:
        stream.seek(0)
        groups, = struct.unpack("<I", stream.read(4))
        for _, count, directory in struct.iter_unpack("<4sII", stream.read(groups * 12)):
            record_offsets.extend(directory + row * 21 + 13 for row in range(count))
    return index


def read_member(stream, index, entry, maximum):
    from python.archives import willplus
    return willplus.read_member(stream, index, entry, max_stored_size=maximum)


def patch_record(header, record, offset, entry, replacement):
    size = entry.size if replacement is None else len(replacement)
    struct.pack_into("<II", header, record, size, offset)
    return replacement


@dataclass(frozen=True)
class Field:
    path: str
    kind: str
    text: str
    start: int
    end: int


class ScriptText:
    def __init__(self, stored, decode=None):
        if not 9 <= len(stored) <= MAX_SIZE or stored[-9] != 0xFF:
            raise ValueError("expected a WSC FileEnd with eight trailing bytes")
        self.data = data = stored.translate(DECODE)
        self.fields = []
        self.jumps = []
        limit = len(data) - 9
        boundaries = bytearray(limit + 1)
        pos = command = 0

        def string(start, role=None, kind=None):
            end = data.find(b'\0', start, limit)
            if end < 0:
                raise ValueError(f"unterminated WSC string at {start:#x}")
            if role is not None:
                value = data[start:end]
                text = decode(value) if decode else value.decode('cp932')
                if text.strip():
                    self.fields.append(Field(f'cmd:{command}:{role}', kind, text, start, end))
            return end + 1

        def jump(at, base, fmt):
            target = base + struct.unpack_from(fmt, data, at)[0]
            self.jumps.append((at, base, target, fmt))

        while pos < limit:
            boundaries[pos] = 1
            op = data[pos]
            if op in FIXED:
                if op == 0x01:
                    jump(pos + 6, pos + 11, '<i')
                elif op == 0x06:
                    jump(pos + 1, 0, '<I')
                pos += FIXED[op]
            elif op in STRINGS:
                for number, skip in enumerate(STRINGS[op]):
                    if op == 0x42 and number == 0:
                        pos = string(pos + skip, 'name', 'name')
                    elif op in (0x41, 0x42):
                        pos = string(pos + skip, 'text', 'dialogue')
                    else:
                        pos = string(pos + skip)
            elif op == 0x02:
                count, = struct.unpack_from('<H', data, pos + 1)
                pos += 3
                for index in range(count):
                    pos = string(pos + 2, f'choice:{index}', 'choice') + 3
                    if pos >= limit:
                        raise ValueError("truncated WSC choice action")
                    action = data[pos]
                    if action == 0x07:
                        pos = string(pos + 1)
                    elif action == 0x03:
                        pos += FIXED[action]
                    elif action == 0x06:
                        jump(pos + 1, 0, '<I')
                        pos += FIXED[action]
                    else:
                        raise ValueError(f"unsupported WSC choice action {action:#x}")
            else:
                raise ValueError(f"unsupported WSC opcode {op:#x} at {pos:#x}")
            command += 1
        boundaries[limit] = 1
        if pos != limit or any(not 0 <= t <= limit or not boundaries[t] for _, _, t, _ in self.jumps):
            raise ValueError("WSC instruction or jump does not align with a command boundary")

    def build(self, translations, encode=None):
        if translations.keys() - {field.path for field in self.fields}:
            raise ValueError("unknown WSC text locator")
        result = bytearray()
        ends, shifts = [], []
        cursor = 0
        for field in self.fields:
            text = translations.get(field.path, field.text)
            if text == field.text:
                continue
            if not text.strip() or any(c in text for c in '\0\r\n\t'):
                raise ValueError(f"invalid WSC translation at {field.path}")
            if Counter(CONTROLS.findall(field.text)) != Counter(CONTROLS.findall(text)):
                raise ValueError(f"WSC control sequence changed at {field.path}")
            value = encode(text) if encode else text.encode('cp932')
            if b'\0' in value or (encode is None and value.decode('cp932') != text):
                raise ValueError(f"WSC translation cannot be represented at {field.path}")
            result.extend(self.data[cursor:field.start])
            result.extend(value)
            cursor = field.end
            ends.append(cursor)
            shifts.append(len(result) - cursor)
        result.extend(self.data[cursor:])
        if len(result) > MAX_SIZE:
            raise ValueError("rebuilt WSC exceeds the script budget")

        def moved(offset):
            count = bisect_right(ends, offset)
            return offset + shifts[count - 1] if count else offset

        # Relative branches use the end of their own instruction; absolute jumps use file start.
        for at, base, target, fmt in self.jumps:
            struct.pack_into(fmt, result, moved(at), moved(target) - moved(base))
        return bytes(result.translate(ENCODE))


def main(request):
    report = dict(success=False, parseStatus="failed", fieldCount=0, roundtripStatus="not-run",
                  translationStatus="not-run", runtimeStatus="not-tested")
    try:
        with Path(request["input"]).open("rb") as stream:
            original = stream.read(MAX_SIZE + 1)
        script = ScriptText(original)
        report.update(parseStatus="passed", fieldCount=len(script.fields))
        if request.get("checkRoundtrip", True):
            if script.build({}) != original:
                raise ValueError("WSC source bytes differ after rebuild")
            report["roundtripStatus"] = "passed"
        if script.fields and request.get("checkTranslation", True):
            sample = script.fields[0]
            changed = sample.text + "確認"
            after = ScriptText(script.build({sample.path: changed}))
            if [(f.path, f.text) for f in after.fields] != [
                (f.path, changed if f.path == sample.path else f.text) for f in script.fields
            ]:
                raise ValueError("WSC edited field or locator differs after readback")
            report["changedTextReadback"] = "passed"
            try:
                script.build({sample.path: sample.text + "汉化验证"})
            except UnicodeEncodeError as error:
                report.update(translationStatus="failed", diagnostic=dict(errorType=type(error).__name__),
                              error="CP932 cannot encode the Chinese sample")
            else:
                report["translationStatus"] = "passed"
        report["success"] = True
    except (ValueError, OSError, struct.error) as error:
        report.update(error=str(error)[:300], diagnostic=dict(errorType=type(error).__name__))
    return report
