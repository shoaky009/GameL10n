"""Dictionary-compressed AI5V/SILV MES text runs."""

from dataclasses import dataclass
import json
from pathlib import Path
import struct
import sys


MAX_SCRIPT = 32 << 20
DICTIONARY_BASE = 0x80
DICTIONARY_CAPACITY = 256 - DICTIONARY_BASE
SET_VARIABLE = {0x0C, 0x0D, 0x0E}
OPERATORS = range(0x20, 0x30)
INLINE_CONSTANTS = range(0x30, 0x40)
VARIABLES = range(0x40, 0x5B)


@dataclass(frozen=True)
class Field:
    path: str
    text: str
    start: int
    end: int
    kind: str = "dialogue"


def decode_pair(pair):
    if (len(pair) != 2 or not (0x81 <= pair[0] <= 0x9F or 0xE0 <= pair[0] <= 0xFC)
            or not 0x40 <= pair[1] <= 0xFC or pair[1] == 0x7F):
        raise ValueError("AI5V character is not a double-byte CP932 pair")
    return pair.decode("cp932")


class ScriptText:
    def __init__(self, data):
        if not 3 <= len(data) <= MAX_SCRIPT:
            raise ValueError("AI5V script size is outside the budget")
        code_start, = struct.unpack_from("<H", data)
        if (code_start % 2 or not 2 <= code_start < len(data)
                or code_start > 2 + 2 * DICTIONARY_CAPACITY):
            raise ValueError("invalid AI5V character table boundary")
        self.data = data
        self.code_start = code_start
        self.dictionary = tuple(data[i:i + 2] for i in range(2, code_start, 2))
        for pair in self.dictionary:
            decode_pair(pair)
        self.fields = []
        position, depth, run = code_start, 0, 0
        while position < len(data):
            opcode = data[position]
            if opcode >= 0x60:
                start = position
                characters = bytearray()
                while position < len(data) and data[position] >= 0x60:
                    code = data[position]
                    if code >= DICTIONARY_BASE:
                        idx = code - DICTIONARY_BASE
                        if idx >= len(self.dictionary):
                            raise ValueError("AI5V character table reference is out of range")
                        characters.extend(self.dictionary[idx])
                        position += 1
                    else:
                        if position + 2 > len(data):
                            raise ValueError("truncated AI5V inline character")
                        if code == 0x60 or not 0x40 <= data[position + 1] <= 0xFC or data[position + 1] == 0x7F:
                            raise ValueError("invalid AI5V inline CP932 character")
                        characters.extend((code + 0x20, data[position + 1]))
                        position += 2
                value = characters.decode("cp932")
                if value.strip():
                    self.fields.append(Field(f"text/{run}", value, start, position))
                run += 1
                continue
            position += 1
            if opcode == 0:
                if depth == 0:
                    if any(data[position:]):
                        raise ValueError("unexpected bytecode after AI5V message end")
                    break
                depth -= 1
            elif opcode == 1:
                depth += 1
            elif opcode == 4:
                if position >= len(data) or not 0x10 <= data[position] <= 0x27:
                    raise ValueError("unsupported or truncated AI5V system command")
                position += 1
            elif opcode == 6:
                end = data.find(b"\x06", position)
                if end < 0 or any(value != 9 and not 0x20 <= value <= 0x7E and not 0xA1 <= value <= 0xDF
                                  for value in data[position:end]):
                    raise ValueError("invalid AI5V parameter string")
                position = end + 1
            elif 0x07 <= opcode <= 0x09:
                end = position + opcode - 6
                if end > len(data) or any(value & 3 != 3 for value in data[position:end]):
                    raise ValueError("invalid AI5V constant operand")
                position = end
            elif opcode in SET_VARIABLE:
                if position >= len(data) or data[position] not in VARIABLES:
                    raise ValueError("invalid AI5V variable operand")
                position += 1
            elif not (opcode in (0x02, 0x03, 0x0A, 0x0B, 0x0F)
                      or opcode in range(0x10, 0x1B)
                      or opcode in OPERATORS or opcode in INLINE_CONSTANTS or opcode in VARIABLES):
                raise ValueError(f"unsupported AI5V opcode 0x{opcode:02x} at {position - 1:#x}")
        if depth:
            raise ValueError("unterminated AI5V bytecode block")

    def build(self, translations):
        if not isinstance(translations, dict) or translations.keys() - {field.path for field in self.fields}:
            raise ValueError("unknown AI5V text field")
        table = list(self.dictionary)
        lookup = {pair: index for index, pair in enumerate(table)}
        pieces, cursor = [], self.code_start
        output_size = len(self.data)
        for field in self.fields:
            value = translations.get(field.path, field.text)
            if value == field.text:
                continue
            if not isinstance(value, str) or len(value) > MAX_SCRIPT or not value.strip():
                raise ValueError("AI5V translation must be a nonblank string")
            encoded = bytearray()
            for character in value:
                pair = character.encode("cp932")
                if decode_pair(pair) != character:
                    raise ValueError("AI5V translation cannot round trip in CP932")
                if pair in lookup:
                    encoded.append(DICTIONARY_BASE + lookup[pair])
                elif pair[0] < 0xA0:
                    encoded.extend((pair[0] - 0x20, pair[1]))
                else:
                    if len(table) == DICTIONARY_CAPACITY:
                        raise ValueError("AI5V character table is full")
                    lookup[pair] = len(table)
                    encoded.append(DICTIONARY_BASE + len(table))
                    table.append(pair)
            pieces.extend((self.data[cursor:field.start], encoded))
            cursor = field.end
            output_size += len(encoded) - (field.end - field.start)
            if output_size + 2 * (len(table) - len(self.dictionary)) > MAX_SCRIPT:
                raise ValueError("rebuilt AI5V script exceeds the byte budget")
        if not pieces:
            return self.data
        pieces.append(self.data[cursor:])
        result = struct.pack("<H", 2 + len(table) * 2) + b"".join(table) + b"".join(pieces)
        after = ScriptText(result)
        expected = {field.path: translations.get(field.path, field.text) for field in self.fields}
        if {field.path: field.text for field in after.fields} != expected:
            raise ValueError("AI5V rebuild changed the identity or content of text runs")
        return result


def main(request=None):
    request = json.load(sys.stdin) if request is None else request
    report = dict(success=False, parseStatus="failed", roundtripStatus="not-run",
                  translationStatus="not-run", fieldCount=0, runtimeStatus="not-tested")
    try:
        with Path(request["input"]).open("rb") as stream:
            data = stream.read(MAX_SCRIPT + 1)
        script = ScriptText(data)
        report.update(success=True, parseStatus="passed", fieldCount=len(script.fields),
                      profile=dict(format="AI5V", encoding="cp932", dictionaryEntries=len(script.dictionary)))
        if request.get("operation") == "verify":
            if request.get("checkRoundtrip", True):
                if script.build({field.path: field.text for field in script.fields}) != data:
                    raise ValueError("AI5V original-byte round trip differs")
                report["roundtripStatus"] = "passed"
            if request.get("checkTranslation", True) and script.fields:
                sample = script.fields[0]
                script.build({sample.path: sample.text + "確認"})
                report["changedTextReadback"] = "passed"
                try:
                    script.build({sample.path: sample.text + "汉化验证"})
                except UnicodeEncodeError as error:
                    report.update(translationStatus="failed", diagnostic=dict(errorType=type(error).__name__),
                                  error="CP932 cannot represent the Chinese sample")
                else:
                    report["translationStatus"] = "passed"
    except (ValueError, OSError) as error:
        report.update(success=False, error=str(error)[:1000], diagnostic=dict(errorType=type(error).__name__))
    return report


if __name__ == "__main__":
    report = main()
    print(json.dumps(report, ensure_ascii=True))
    sys.exit(0 if report["success"] else 1)
