"""KiriKiri Unicode TFT v1 inspection and explicit BMP glyph supplementation.

``index(data)`` returns codepoint -> Glyph; ``bitmap(data, glyph)`` returns
row-major 0..64 grayscale. ``supplement(data, codes, renderer)`` returns bytes,
using ``renderer.glyph(code) -> (Glyph, pixels)`` only for missing codepoints.
It retains every original byte except the count/index offsets in header bytes
24..35. Existing glyph metrics, bitmaps and reserved fields remain untouched.

The format and RLE follow krkrz/krkrz commit
fd5c4baa6a2ef5978db1bd043634351f48667daf, visual/PrerenderedFont.{h,cpp}:
https://github.com/krkrz/krkrz/tree/fd5c4baa6a2ef5978db1bd043634351f48667daf/visual
Version 0, non-Unicode files, and non-BMP supplementation are unsupported.

GdiRenderer is an optional Windows build-time renderer. Supply the font file,
face and pixel size explicitly and use its context manager to release private
font registrations. This module neither installs fonts nor changes runtime
font selection. The JSON stdin/stdout CLI supports read-only ``probe`` only;
matching the file format does not establish in-game rendering compatibility.
"""
import json
import os
from pathlib import Path
import stat
import struct
import sys
from typing import NamedTuple


FORMAT = "kirikiri-unicode-tft-v1"
MAGIC = b"TVP pre-rendered font\x1a"
RECORD = struct.Struct("<IHHhhhhhH")
MAX_FILE_BYTES = 64 << 20
MAX_GLYPH_PIXELS = 1 << 20
MAX_TOTAL_PIXELS = 64 << 20


class Glyph(NamedTuple):
    offset: int
    width: int
    height: int
    origin_x: int
    origin_y: int
    inc_x: int
    inc_y: int
    advance: int
    reserved: int


def _header(data):
    if not isinstance(data, (bytes, bytearray)):
        raise ValueError("TFT data must be bytes or bytearray")
    if not 36 <= len(data) <= MAX_FILE_BYTES:
        raise ValueError("TFT file must contain 36 bytes..64 MiB")
    if data[:22] != MAGIC or data[22:24] != b"\x01\x02":
        raise ValueError("Only KiriKiri Unicode TFT version 1 is supported")
    count, chars, records = struct.unpack_from("<III", data, 24)
    if not 1 <= count <= 65536:
        raise ValueError("TFT glyph count must be in 1..65536")
    ranges = ((chars, chars + count * 2), (records, records + count * RECORD.size))
    if any(start < 36 or end > len(data) for start, end in ranges):
        raise ValueError("TFT index lies outside the file")
    if max(ranges[0][0], ranges[1][0]) < min(ranges[0][1], ranges[1][1]):
        raise ValueError("TFT character and glyph indexes overlap")
    return count, chars, records, ranges


def _glyph(item):
    try:
        item = Glyph(*item)
        if any(type(value) is not int for value in item):
            raise ValueError("TFT glyph fields must be integers")
        RECORD.pack(*item)
    except (TypeError, struct.error) as error:
        raise ValueError("TFT glyph must contain the nine valid record fields") from error
    if item.width * item.height > MAX_GLYPH_PIXELS:
        raise ValueError("TFT glyph exceeds the 1 Mi-pixel limit")
    return item


def _bitmap(data, item, ranges, *, pixels):
    """Stop at the declared pixel count and before any active index table."""
    at = item.offset
    size = item.width * item.height
    if at > len(data):
        raise ValueError("TFT glyph offset lies outside the file")
    if not size:
        return b"" if pixels else None
    if at < 36 or any(start <= at < end for start, end in ranges):
        raise ValueError("TFT bitmap overlaps the header or an index")
    limit = min([len(data)] + [start for start, _ in ranges if start > at])
    output = bytearray() if pixels else None
    produced = 0
    last = None
    while produced < size:
        if at >= limit:
            raise ValueError("TFT bitmap is truncated or crosses an index")
        value = data[at]
        at += 1
        if value >= 0x41:
            length = value - 0x40
            if last is None or produced + length > size:
                raise ValueError("Invalid TFT v1 bitmap repeat")
        else:
            length = 1
            last = value
        if pixels:
            output.extend(bytes((last,)) * length)
        produced += length
    return bytes(output) if pixels else None


def index(data):
    """Validate indexes and every bitmap; return sorted codepoint -> Glyph.

    Zero-area glyphs are allowed. Bitmap expansion is bounded to 1 Mi pixels
    per glyph and 64 Mi pixels for the complete font, including shared data.
    """
    count, chars, records, ranges = _header(data)
    entries = {}
    previous = -1
    total = 0
    for i in range(count):
        code = struct.unpack_from("<H", data, chars + i * 2)[0]
        if code <= previous:
            raise ValueError("TFT character index must be strictly increasing")
        previous = code
        item = _glyph(RECORD.unpack_from(data, records + i * RECORD.size))
        total += item.width * item.height
        if total > MAX_TOTAL_PIXELS:
            raise ValueError("TFT font exceeds the 64 Mi-pixel expansion limit")
        _bitmap(data, item, ranges, pixels=False)
        entries[code] = item
    return entries


def bitmap(data, item):
    """Decode one Glyph, preserving the engine's native 0..64 grayscale."""
    _, _, _, ranges = _header(data)
    return _bitmap(data, _glyph(item), ranges, pixels=True)


def _codepoints(codes):
    result = set()
    for code in codes:
        if type(code) is not int or not 0 <= code <= 0xFFFF or 0xD800 <= code <= 0xDFFF:
            raise ValueError("Supplementation requires Unicode BMP scalar codepoints")
        result.add(code)
    return result


def supplement(data, codes, renderer):
    """Append only missing BMP glyphs and merge the sorted character index.

    No file is written. With no missing codepoints the original bytes are
    returned and the renderer is not called. Newly rendered pixels are stored
    as valid uncompressed TFT v1 literals; the renderer's offset is ignored.
    Empty glyphs are allowed (for example a space with a nonzero advance).
    """
    entries = index(data)
    missing = sorted(_codepoints(codes) - entries.keys())
    if not missing:
        return bytes(data)
    output = bytearray(data)
    table_size = (len(entries) + len(missing)) * (2 + RECORD.size)
    total_pixels = sum(item.width * item.height for item in entries.values())
    for code in missing:
        item, pixels = renderer.glyph(code)
        item = _glyph(item)
        if not isinstance(pixels, (bytes, bytearray)) or len(pixels) != item.width * item.height:
            raise ValueError(f"Rendered bitmap size does not match U+{code:04X}")
        if any(value > 64 for value in pixels):
            raise ValueError(f"Rendered bitmap is not 0..64 grayscale: U+{code:04X}")
        total_pixels += len(pixels)
        if total_pixels > MAX_TOTAL_PIXELS or len(output) + len(pixels) + table_size > MAX_FILE_BYTES:
            raise ValueError("Supplemented TFT exceeds the file or pixel limit")
        entries[code] = item._replace(offset=len(output))
        output.extend(pixels)
    ordered = sorted(entries)
    char_offset = len(output)
    output.extend(struct.pack("<" + "H" * len(ordered), *ordered))
    record_offset = len(output)
    for code in ordered:
        output.extend(RECORD.pack(*entries[code]))
    struct.pack_into("<III", output, 24, len(ordered), char_offset, record_offset)
    return bytes(output)


class GdiRenderer:
    """Explicit font_file/face and em size in pixels, with GDI weight/italic.

    Windows only; use ``with GdiRenderer(path, face, size) as renderer``.
    Native resources are opened on the first glyph request, so a supplement
    that needs no new glyphs does not load the font or create a GDI object.
    The face must match GetTextFaceW after selection; a substituted face or
    absent glyph is an error. Style synthesis and suitability for a specific
    engine remain the caller's responsibility. Resources are private to this
    process and are released by close(), including on initialization failure.
    """
    def __init__(self, font_file, face, size, *, weight=400, italic=False):
        if os.name != "nt":
            raise ValueError("GdiRenderer requires Windows GDI")
        if (not isinstance(face, str) or not face or "\0" in face
                or len(face.encode("utf-16-le")) > 62):
            raise ValueError("face must contain 1..31 UTF-16 code units")
        if type(size) is not int or not 1 <= size <= 512:
            raise ValueError("size must be an integer in 1..512 em pixels")
        if type(weight) is not int or not 0 <= weight <= 1000 or type(italic) is not bool:
            raise ValueError("weight must be in 0..1000 and italic must be boolean")
        path = Path(font_file).resolve(strict=True)
        if not path.is_file() or path.suffix.lower() not in (".ttf", ".ttc", ".otf"):
            raise ValueError("font_file must be a TTF, TTC or OTF file")
        if path.stat().st_size > MAX_FILE_BYTES:
            raise ValueError("font_file exceeds 64 MiB")
        self.path = str(path)
        self.face, self.size, self.weight, self.italic = face, size, weight, italic
        self.dc = self.font = self.previous = None
        self.registered = False
        self._closed = False

    def _open(self):
        if self._closed:
            raise ValueError("GdiRenderer is closed")
        if self.dc:
            return
        import ctypes as c
        from ctypes import wintypes as w

        class Point(c.Structure):
            _fields_ = [("x", w.LONG), ("y", w.LONG)]

        class Metrics(c.Structure):
            _fields_ = [("width", w.UINT), ("height", w.UINT), ("origin", Point),
                        ("advance_x", c.c_short), ("advance_y", c.c_short)]

        class Fixed(c.Structure):
            _fields_ = [("fract", w.WORD), ("value", c.c_short)]

        class Matrix(c.Structure):
            _fields_ = [(name, Fixed) for name in ("m11", "m12", "m21", "m22")]

        self.c = c
        self.Metrics = Metrics
        self.matrix = Matrix(Fixed(0, 1), Fixed(), Fixed(), Fixed(0, 1))
        self.gdi = gdi = c.WinDLL("gdi32", use_last_error=True)
        signatures = {
            "AddFontResourceExW": ([w.LPCWSTR, w.DWORD, c.c_void_p], c.c_int),
            "RemoveFontResourceExW": ([w.LPCWSTR, w.DWORD, c.c_void_p], w.BOOL),
            "CreateCompatibleDC": ([c.c_void_p], c.c_void_p),
            "DeleteDC": ([c.c_void_p], w.BOOL),
            "CreateFontW": ([c.c_int] * 5 + [w.DWORD] * 8 + [w.LPCWSTR], c.c_void_p),
            "DeleteObject": ([c.c_void_p], w.BOOL),
            "SelectObject": ([c.c_void_p, c.c_void_p], c.c_void_p),
            "GetTextFaceW": ([c.c_void_p, c.c_int, w.LPWSTR], c.c_int),
            "GetGlyphIndicesW": ([c.c_void_p, w.LPCWSTR, c.c_int, c.POINTER(w.WORD), w.DWORD], w.DWORD),
            "GetGlyphOutlineW": ([c.c_void_p, w.UINT, w.UINT, c.POINTER(Metrics),
                                   w.DWORD, c.c_void_p, c.POINTER(Matrix)], w.DWORD),
        }
        for name, (args, restype) in signatures.items():
            function = getattr(gdi, name)
            function.argtypes, function.restype = args, restype
        try:
            if not gdi.AddFontResourceExW(self.path, 0x30, None):
                raise OSError("GDI could not privately load font_file")
            self.registered = True  # FR_PRIVATE | FR_NOT_ENUM; never installed.
            self.dc = gdi.CreateCompatibleDC(None)
            if not self.dc:
                raise OSError("GDI could not create a font DC")
            self.font = gdi.CreateFontW(-self.size, 0, 0, 0, self.weight, int(self.italic), 0, 0,
                                        1, 0, 0, 4, 0, self.face)
            if not self.font:
                raise OSError("GDI could not create the requested font")
            previous = gdi.SelectObject(self.dc, self.font)
            if not previous or previous == c.c_void_p(-1).value:
                raise OSError("GDI could not select the requested font")
            self.previous = previous
            selected = c.create_unicode_buffer(128)
            if not gdi.GetTextFaceW(self.dc, len(selected), selected):
                raise OSError("GDI could not read the selected font face")
            if selected.value.casefold() != self.face.casefold():
                raise ValueError(f"GDI substituted face {self.face!r} with {selected.value!r}")
        except BaseException:
            self.close()
            raise

    def __enter__(self):
        if self._closed:
            raise ValueError("GdiRenderer is closed")
        return self

    def __exit__(self, *_):
        self.close()

    def close(self):
        if self.previous and self.dc:
            self.gdi.SelectObject(self.dc, self.previous)
            self.previous = None
        if self.font:
            self.gdi.DeleteObject(self.font)
            self.font = None
        if self.dc:
            self.gdi.DeleteDC(self.dc)
            self.dc = None
        if self.registered:
            self.gdi.RemoveFontResourceExW(self.path, 0x30, None)
            self.registered = False
        self._closed = True

    def glyph(self, code):
        _codepoints((code,))
        self._open()
        c = self.c
        glyph_index = c.c_ushort()
        status = self.gdi.GetGlyphIndicesW(self.dc, chr(code), 1, c.byref(glyph_index), 1)
        if status == 0xFFFFFFFF or glyph_index.value == 0xFFFF:
            raise ValueError(f"Selected font does not cover U+{code:04X}")
        metrics = self.Metrics()
        # GGO_GRAY8_BITMAP | GGO_GLYPH_INDEX: use the checked glyph, no fallback.
        flags = 6 | 0x80
        length = self.gdi.GetGlyphOutlineW(self.dc, glyph_index.value, flags, c.byref(metrics),
                                          0, None, c.byref(self.matrix))
        if length == 0xFFFFFFFF or length > MAX_GLYPH_PIXELS + 3 * 65535:
            raise ValueError(f"GDI could not read a bounded bitmap for U+{code:04X}")
        # GDI can report a 1x1 placeholder for a space while requiring zero
        # bitmap bytes. Represent its absent ink with a zero-area TFT glyph.
        width, height = (metrics.width, metrics.height) if length else (0, 0)
        item = _glyph(Glyph(0, width, height, metrics.origin.x, metrics.origin.y,
                            metrics.advance_x, metrics.advance_y, metrics.advance_x, 0))
        pitch = (item.width + 3) & ~3
        if length != pitch * item.height:
            raise ValueError(f"GDI bitmap pitch/length mismatch for U+{code:04X}")
        buffer = c.create_string_buffer(length)
        if length:
            status = self.gdi.GetGlyphOutlineW(self.dc, glyph_index.value, flags, c.byref(metrics),
                                               length, buffer, c.byref(self.matrix))
            if status != length:
                raise ValueError(f"GDI bitmap read was incomplete for U+{code:04X}")
        pixels = b"".join(buffer.raw[y * pitch:y * pitch + item.width] for y in range(item.height))
        if any(value > 64 for value in pixels):
            raise ValueError(f"GDI returned invalid grayscale for U+{code:04X}")
        return item, pixels


def _read_file(path):
    with Path(path).open("rb") as stream:
        if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
            raise ValueError("TFT input must be a regular file")
        data = stream.read(MAX_FILE_BYTES + 1)
    if len(data) > MAX_FILE_BYTES:
        raise ValueError("TFT input exceeds 64 MiB")
    return data


def probe(input_path, text="", sample_limit=2):
    """Read-only file/coverage/metrics probe; never renders or writes glyphs."""
    result = dict(status="rejected", format=FORMAT, runtimeTested=False, samples=[])
    try:
        if type(sample_limit) is not int or not 0 <= sample_limit <= 2:
            raise ValueError("sampleLimit must be an integer in 0..2")
        if not isinstance(text, str) or len(text) > 4096:
            raise ValueError("text must contain at most 4096 characters")
        codes = _codepoints(ord(char) for char in text)
        if not input_path:
            result.update(status="needs-evidence", reason="A TFT input path is required")
            return result
        data = _read_file(input_path)
        entries = index(data)
        present = codes & entries.keys()
        missing = sorted(codes - entries.keys())
        result.update(status="matched", version=1, codeUnitBytes=2,
                      fileBytes=len(data), glyphCount=len(entries),
                      coverage=dict(requestedCount=len(codes), presentCount=len(present),
                                    missingCount=len(missing),
                                    missingCodepoints=[f"U+{code:04X}" for code in missing[:16]]))
        # Requested covered glyphs first; otherwise sample the sorted index.
        ordered = sorted(present) if codes else list(entries)
        for code in ordered[:sample_limit]:
            item = entries[code]
            result["samples"].append(dict(codepoint=f"U+{code:04X}", width=item.width,
                                          height=item.height, originX=item.origin_x, originY=item.origin_y,
                                          incX=item.inc_x, incY=item.inc_y, advance=item.advance))
    except FileNotFoundError:
        result.update(status="needs-evidence", reason="The TFT input file is missing")
    except (OSError, ValueError, TypeError) as error:
        result["reason"] = str(error)[:300]
    return result


def main():
    try:
        raw = sys.stdin.read(16_385)
        if len(raw) > 16_384:
            raise ValueError("Probe request exceeds 16 KiB")
        request = json.loads(raw)
        if not isinstance(request, dict) or request.get("operation") != "probe":
            raise ValueError("operation must be probe")
        result = probe(request.get("input"), request.get("text", ""), request.get("sampleLimit", 2))
    except (ValueError, TypeError) as error:
        result = dict(status="rejected", format=FORMAT, reason=str(error)[:300], runtimeTested=False)
    print(json.dumps(result, ensure_ascii=True))
    return 0 if result["status"] == "matched" else 1


if __name__ == "__main__":
    raise SystemExit(main())
