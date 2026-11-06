"""VNTextProxy v0.0.41-compatible SJIS tunnel (MIT, arcusmaximus).

Tunnel(raw=b'') loads the ordered UTF-16LE sjis_ext.bin mapping. encode(text)
appends new BMP characters without reassigning existing slots; decode(bytes)
and serialize() read the same wire format. Persist serialize() explicitly in
the adapter when producing a patch. A failed encode leaves the mapping intact.

The JSON stdin CLI accepts operation="check" (or "verify"), texts=[str], and
optional mappingInput=<file>. It reads at most 8 MiB of JSON and never writes
files or returns text, encoded bytes, or the mapping. neededSlots counts new
unique mapped characters; remainingSlots is the unused capacity after the
sample (zero on exhaustion). This only checks encoding feasibility in memory.
It cannot validate engine character traversal, fonts, or DLL side-loading.
"""

import json
from pathlib import Path
import sys


AVOID = (9, 10, 13, 32, 44)
SLOTS = 59 * 58
MAX_REQUEST_BYTES = 8 * 1024 * 1024
LIMIT = ("3422 non-NUL BMP tunnel characters. Encoding round-trip only; engine "
         "character traversal, font rendering and DLL side-loading are unverified.")


def pair(index):
    """Return the VNTextProxy byte pair for a zero-based tunnel slot."""
    if type(index) is not int or not 0 <= index < SLOTS:
        raise ValueError("SJIS tunnel slot must be in 0..3421")
    hi, low = divmod(index, 58)
    high = 0x81 + hi if hi < 31 else 0xE0 + hi - 31
    low += 1
    for blocked in AVOID:
        if low >= blocked:
            low += 1
    return bytes((high, low))


def _validate_char(char):
    code = ord(char)
    if not code or code > 0xFFFF or 0xD800 <= code <= 0xDFFF:
        raise ValueError("VNTextProxy supports only non-NUL BMP characters without surrogates")


def _native_bytes(char):
    try:
        encoded = char.encode("cp932")
        # CP932 aliases (e.g. yen and wave dash) must not silently change text.
        # VNTextProxy reserves the F0+ range, including CP932 private-use glyphs.
        if encoded[0] < 0xF0 and encoded.decode("cp932") == char:
            return encoded
    except UnicodeError:
        pass
    return None


class Tunnel:
    def __init__(self, raw=b''):
        if not isinstance(raw, (bytes, bytearray)):
            raise TypeError("SJIS tunnel mapping must be bytes")
        if len(raw) % 2 or len(raw) > SLOTS * 2:
            raise ValueError("Invalid SJIS tunnel mapping length (maximum 6844 bytes)")
        try:
            chars = list(raw.decode("utf-16-le"))
        except UnicodeError:
            raise ValueError("Invalid UTF-16LE SJIS tunnel mapping") from None
        for char in chars:
            _validate_char(char)
        if len(set(chars)) != len(chars):
            raise ValueError("Duplicate characters in SJIS tunnel mapping")
        self.chars = chars
        self.reverse = {char: index for index, char in enumerate(chars)}

    def decode(self, raw):
        if not isinstance(raw, (bytes, bytearray)):
            raise TypeError("SJIS input must be bytes")
        text = []
        position = 0
        while position < len(raw):
            high = raw[position]
            width = 2 if 0x81 <= high < 0xA0 or 0xE0 <= high < 0xFD else 1
            chunk = raw[position:position + width]
            if len(chunk) != width:
                raise ValueError("Truncated SJIS character")
            if 0 in chunk:
                raise ValueError("NUL is not supported in SJIS tunnel text")
            if width == 2 and chunk[1] < 0x40:
                low = chunk[1]
                if low in AVOID:
                    raise ValueError("Control or delimiter byte conflicts with SJIS tunnel trail byte")
                hi = high - 0x81 if high < 0xA0 else 31 + high - 0xE0
                index = hi * 58 + low - 1 - sum(blocked < low for blocked in AVOID)
                if index >= len(self.chars):
                    raise ValueError("Missing or incomplete sjis_ext.bin tunnel mapping")
                text.append(self.chars[index])
            else:
                try:
                    text.append(chunk.decode("cp932"))
                except UnicodeError:
                    raise ValueError("Invalid CP932 byte sequence") from None
            position += width
        return "".join(text)

    def encode(self, text):
        if not isinstance(text, str):
            raise TypeError("SJIS tunnel text must be a string")
        out = bytearray()
        added = {}
        for char in text:
            _validate_char(char)
            encoded = _native_bytes(char)
            if encoded is None:
                index = self.reverse.get(char)
                if index is None:
                    index = added.get(char)
                if index is None:
                    index = len(self.chars) + len(added)
                    if index >= SLOTS:
                        raise ValueError("SJIS tunnel capacity exceeded (maximum 3422 extended characters)")
                    added[char] = index
                encoded = pair(index)
            out.extend(encoded)
        self.chars.extend(added)
        self.reverse.update(added)
        return bytes(out)

    def serialize(self):
        return "".join(self.chars).encode("utf-16-le")


def _report():
    return dict(success=False, status="rejected",
                checks=dict(mapping=None, characters=None, capacity=None, roundTrip=None),
                neededSlots=None, remainingSlots=None, runtimeTested=False, limit=LIMIT)


def execute(request):
    """Check text feasibility without persisting the in-memory tunnel mapping."""
    result = _report()
    checks = result["checks"]
    try:
        if not isinstance(request, dict) or request.get("operation") not in ("check", "verify"):
            raise ValueError("operation must be check or verify")
        texts = request.get("texts")
        if not isinstance(texts, list) or not all(isinstance(text, str) for text in texts):
            raise ValueError("texts must be an array of strings")
        checks["mapping"] = False
        mapping_input = request.get("mappingInput")
        raw = b''
        if mapping_input is not None:
            if not isinstance(mapping_input, str) or not mapping_input or "\0" in mapping_input:
                raise ValueError("mappingInput must be a file path")
            with Path(mapping_input).open("rb") as source:
                raw = source.read(SLOTS * 2 + 1)
        tunnel = Tunnel(raw)
        checks["mapping"] = True
        result["remainingSlots"] = SLOTS - len(tunnel.chars)
        checks["characters"] = False
        characters = set("".join(texts))
        for char in characters:
            _validate_char(char)
        checks["characters"] = True
        needed = sum(char not in tunnel.reverse and _native_bytes(char) is None for char in characters)
        available = SLOTS - len(tunnel.chars)
        result.update(neededSlots=needed, remainingSlots=max(0, available - needed))
        checks["capacity"] = needed <= available
        if not checks["capacity"]:
            raise ValueError(f"SJIS tunnel capacity exceeded: {needed} new slots required, {available} available")
        checks["roundTrip"] = False
        for text in texts:
            if tunnel.decode(tunnel.encode(text)) != text:
                raise ValueError("SJIS tunnel encoding round-trip failed")
        checks["roundTrip"] = True
        result.update(success=True, status="ok")
    except (TypeError, ValueError, OSError) as error:
        result["error"] = str(error)
    return result


def main():
    try:
        data = sys.stdin.buffer.read(MAX_REQUEST_BYTES + 1)
        if len(data) > MAX_REQUEST_BYTES:
            raise ValueError("request exceeds 8 MiB")
        result = execute(json.loads(data.decode("utf-8-sig")))
    except (UnicodeError, ValueError) as error:
        result = _report()
        result["error"] = "Invalid UTF-8 JSON request" if isinstance(error, UnicodeError) else str(error)
    print(json.dumps(result, ensure_ascii=True))
    return 0 if result["success"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
