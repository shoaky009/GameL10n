"""Length-prefixed text fields in NekoSDK ADVSCRIPT2 records."""

from dataclasses import dataclass
import json
from pathlib import Path
import struct
import sys


MAGIC = b"NEKOSDK_ADVSCRIPT2\0"
MAX_SCRIPT = 32 << 20
RECORD_HEADER = 212
STRING_COUNT = 33
DISPLAY = "[テキスト表示]".encode("cp932")
LOG = "[ログ追加]".encode("cp932")
U32 = struct.Struct("<I")


@dataclass(frozen=True)
class Field:
    path: str
    text: str
    kind: str
    length_offset: int
    end: int


class ScriptText:
    def __init__(self, data):
        if not data.startswith(MAGIC) or not len(MAGIC) + 4 <= len(data) <= MAX_SCRIPT:
            raise ValueError("invalid ADVSCRIPT2 header or script size")
        count, = U32.unpack_from(data, len(MAGIC))
        position = len(MAGIC) + 4
        if count > (len(data) - position) // (RECORD_HEADER + STRING_COUNT * 4):
            raise ValueError("ADVSCRIPT2 record count exceeds the script")
        self.data = data
        self.fields = []
        for record in range(count):
            if position + RECORD_HEADER > len(data):
                raise ValueError("truncated ADVSCRIPT2 record")
            opcode, = U32.unpack_from(data, position + 12)
            position += RECORD_HEADER
            is_text = False
            for slot in range(STRING_COUNT):
                if position + 4 > len(data):
                    raise ValueError("truncated ADVSCRIPT2 string length")
                size, = U32.unpack_from(data, position)
                start, end = position + 4, position + 4 + size
                if end > len(data):
                    raise ValueError("ADVSCRIPT2 string exceeds the script")
                if slot == 0:
                    is_text = data.startswith(LOG, start, end)
                    if opcode == 5:
                        if not data.startswith(DISPLAY, start, end):
                            raise ValueError("unexpected ADVSCRIPT2 display command")
                        is_text = True
                elif is_text and slot in (1, 2):
                    if not size or data[end - 1] != 0 or data.find(b"\0", start, end - 1) >= 0:
                        raise ValueError("ADVSCRIPT2 text field is not a NUL-terminated string")
                    value = data[start:end - 1].decode("cp932")
                    if value.strip():
                        kind = "name" if slot == 1 else "dialogue"
                        self.fields.append(Field(f"{record}:{slot}", value, kind, position, end))
                position = end
        if position != len(data):
            raise ValueError("unexpected data after ADVSCRIPT2 records")

    def build(self, translations):
        fields = {field.path: field for field in self.fields}
        if not isinstance(translations, dict) or translations.keys() - fields.keys():
            raise ValueError("unknown ADVSCRIPT2 text field")
        edits = []
        for path, value in translations.items():
            field = fields[path]
            if not isinstance(value, str) or len(value) > MAX_SCRIPT or not value.strip() or "\0" in value:
                raise ValueError("ADVSCRIPT2 translation must be a nonblank string without NUL")
            if value != field.text:
                encoded = value.encode("cp932")
                if encoded.decode("cp932") != value:
                    raise ValueError("ADVSCRIPT2 translation cannot round trip in CP932")
                edits.append((field.length_offset, field.end, U32.pack(len(encoded) + 1) + encoded + b"\0"))
        if not edits:
            return self.data
        pieces, position = [], 0
        for start, end, replacement in sorted(edits):
            pieces.extend((self.data[position:start], replacement))
            position = end
        pieces.append(self.data[position:])
        result = b"".join(pieces)
        if len(result) > MAX_SCRIPT:
            raise ValueError("rebuilt ADVSCRIPT2 exceeds the script budget")
        return result


def main(request=None):
    request = json.load(sys.stdin) if request is None else request
    result = dict(success=False, parseStatus="failed", roundtripStatus="not-run",
                  translationStatus="not-run", fieldCount=0, runtimeStatus="not-tested")
    try:
        with Path(request["input"]).open("rb") as source:
            data = source.read(MAX_SCRIPT + 1)
        script = ScriptText(data)
        result.update(success=True, parseStatus="passed", fieldCount=len(script.fields),
                      profile=dict(format="ADVSCRIPT2", encoding="cp932"))
        if request.get("operation") == "verify":
            if request.get("checkRoundtrip", True):
                originals = {field.path: field.text for field in script.fields}
                if script.build(originals) != data:
                    raise ValueError("original-byte round trip differs")
                result["roundtripStatus"] = "passed"
            if request.get("checkTranslation", True) and script.fields:
                field = next((field for field in script.fields if field.kind == "dialogue"), script.fields[0])
                replacement = field.text + "翻訳確認"
                rebuilt = ScriptText(script.build({field.path: replacement}))
                if {f.path: f.text for f in rebuilt.fields}.get(field.path) != replacement:
                    raise ValueError("replaced text did not round trip")
                result["changedTextReadback"] = "passed"
                try:
                    script.build({field.path: field.text + "汉化验证"})
                except UnicodeEncodeError as error:
                    result.update(translationStatus="failed", diagnostic=dict(errorType=type(error).__name__),
                                  error="CP932 cannot represent the Chinese sample")
                else:
                    result["translationStatus"] = "passed"
    except (ValueError, OSError) as error:
        result.update(success=False, diagnostic=dict(errorType=type(error).__name__), error=str(error)[:1000])
    return result


if __name__ == "__main__":
    report = main()
    print(json.dumps(report, ensure_ascii=True))
    sys.exit(0 if report["success"] else 1)
