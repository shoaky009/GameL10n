"""Read-only GDI font coverage/metrics probe. JSON stdin/stdout; no font installation.

Run in a short-lived Python process. Private font registrations are removed before exit.
This measures the requested GDI font, not a game's renderer or its fallback chain.
"""
import ctypes as c
from ctypes import wintypes as w
import json
import os
from pathlib import Path
import sys


class LOGFONT(c.Structure):
    _fields_ = [(name, w.LONG) for name in ("height", "width", "escapement", "orientation", "weight")] + [
        (name, w.BYTE) for name in ("italic", "underline", "strikeOut", "charSet", "outPrecision",
                                   "clipPrecision", "quality", "pitchAndFamily")
    ] + [("faceName", w.WCHAR * 32)]


class SIZE(c.Structure):
    _fields_ = [("cx", w.LONG), ("cy", w.LONG)]


class POINT(c.Structure):
    _fields_ = [("x", w.LONG), ("y", w.LONG)]


class GLYPHMETRICS(c.Structure):
    _fields_ = [("blackBoxX", w.UINT), ("blackBoxY", w.UINT), ("origin", POINT),
                ("cellIncX", c.c_short), ("cellIncY", c.c_short)]


class FIXED(c.Structure):
    _fields_ = [("fract", w.WORD), ("value", c.c_short)]


class MAT2(c.Structure):
    _fields_ = [(name, FIXED) for name in ("eM11", "eM12", "eM21", "eM22")]


def gdi_api():
    gdi = c.WinDLL("gdi32", use_last_error=True)
    signatures = {
        "AddFontResourceExW": ([w.LPCWSTR, w.DWORD, c.c_void_p], c.c_int),
        "RemoveFontResourceExW": ([w.LPCWSTR, w.DWORD, c.c_void_p], w.BOOL),
        "CreateCompatibleDC": ([c.c_void_p], c.c_void_p),
        "DeleteDC": ([c.c_void_p], w.BOOL),
        "CreateFontIndirectW": ([c.POINTER(LOGFONT)], c.c_void_p),
        "SelectObject": ([c.c_void_p, c.c_void_p], c.c_void_p),
        "DeleteObject": ([c.c_void_p], w.BOOL),
        "GetTextFaceW": ([c.c_void_p, c.c_int, w.LPWSTR], c.c_int),
        "GetGlyphIndicesW": ([c.c_void_p, w.LPCWSTR, c.c_int, c.POINTER(w.WORD), w.DWORD], w.DWORD),
        "GetTextExtentPoint32W": ([c.c_void_p, w.LPCWSTR, c.c_int, c.POINTER(SIZE)], w.BOOL),
        "GetGlyphOutlineW": ([c.c_void_p, w.UINT, w.UINT, c.POINTER(GLYPHMETRICS),
                               w.DWORD, c.c_void_p, c.POINTER(MAT2)], w.DWORD),
    }
    for name, (args, result) in signatures.items():
        fn = getattr(gdi, name)
        fn.argtypes, fn.restype = args, result
    return gdi


def integer(spec, key, default, minimum, maximum):
    value = spec.get(key, default)
    if type(value) is not int or not minimum <= value <= maximum:
        raise ValueError(f"{key} must be an integer in {minimum}..{maximum}")
    return value


def inspect_font(gdi, spec, characters):
    face = spec.get("face")
    if not isinstance(face, str) or not face or "\0" in face or len(face.encode("utf-16-le")) > 62:
        raise ValueError("face must contain 1..31 UTF-16 code units")
    lf = LOGFONT()
    lf.faceName = face
    lf.height = integer(spec, "height", -24, -512, 512)
    if not lf.height:
        raise ValueError("height must not be zero; use the engine's signed height request")
    lf.width = integer(spec, "width", 0, 0, 512)
    lf.weight = integer(spec, "weight", 400, 0, 1000)
    lf.charSet = integer(spec, "charset", 1, 0, 255)
    lf.pitchAndFamily = integer(spec, "pitchAndFamily", 0, 0, 255)
    lf.italic = int(bool(spec.get("italic", False)))
    font_path = spec.get("file")
    registered = False
    dc = font = previous = None
    try:
        if font_path:
            font_path = str(Path(font_path).resolve(strict=True))
            if Path(font_path).suffix.lower() not in (".ttf", ".ttc", ".otf"):
                raise ValueError("file must be a staged TTF/TTC/OTF font")
            if Path(font_path).stat().st_size > 64 << 20:
                raise ValueError("font file exceeds 64 MiB")
            # FR_PRIVATE | FR_NOT_ENUM: this child process only, never the installed-font list.
            if not gdi.AddFontResourceExW(font_path, 0x30, None):
                raise OSError("GDI could not load the font file")
            registered = True
        dc = gdi.CreateCompatibleDC(None)
        font = gdi.CreateFontIndirectW(c.byref(lf))
        if not dc or not font:
            raise OSError("GDI could not create the probe DC/font")
        previous = gdi.SelectObject(dc, font)
        if not previous or previous == c.c_void_p(-1).value:
            raise OSError("GDI could not select the probe font")
        selected = c.create_unicode_buffer(128)
        if not gdi.GetTextFaceW(dc, len(selected), selected):
            raise OSError("GDI could not read the selected font face")
        transform = MAT2(FIXED(0, 1), FIXED(), FIXED(), FIXED(0, 1))
        missing, unsupported, glyphs = [], [], []
        for ch in characters:
            code = ord(ch)
            if code > 0xFFFF or 0xD800 <= code <= 0xDFFF:
                unsupported.append(f"U+{code:04X}")
                continue
            glyph = w.WORD()
            if gdi.GetGlyphIndicesW(dc, ch, 1, c.byref(glyph), 1) == 0xFFFFFFFF:
                raise OSError("GetGlyphIndicesW failed")
            size = SIZE()
            if not gdi.GetTextExtentPoint32W(dc, ch, 1, c.byref(size)):
                raise OSError("GetTextExtentPoint32W failed")
            item = dict(character=ch, codepoint=f"U+{code:04X}", present=glyph.value != 0xFFFF,
                        measuredAdvance=size.cx)
            if glyph.value == 0xFFFF:
                missing.append(ch)
            else:
                metrics = GLYPHMETRICS()
                if gdi.GetGlyphOutlineW(dc, code, 0, c.byref(metrics), 0, None,
                                       c.byref(transform)) != 0xFFFFFFFF:
                    item.update(glyphAdvance=metrics.cellIncX, inkLeft=metrics.origin.x,
                                inkWidth=metrics.blackBoxX,
                                rightOverhang=max(0, metrics.origin.x + metrics.blackBoxX - size.cx))
            glyphs.append(item)
        overhangs = [g for g in glyphs if g.get("rightOverhang", 0) > 0]
        mismatches = [g for g in glyphs if "glyphAdvance" in g and g["glyphAdvance"] != g["measuredAdvance"]]
        covered = [g["measuredAdvance"] for g in glyphs if g["present"]]
        result = dict(requestedFace=face, selectedFace=selected.value,
                      faceNameDiffers=selected.value.casefold() != face.casefold(),
                      height=lf.height, width=lf.width, charset=lf.charSet,
                      characterCount=len(characters), missingCount=len(missing),
                      missingCharacters="".join(missing[:32]), unsupportedCount=len(unsupported),
                      unsupportedCodepoints=unsupported[:16],
                      coversSample=not missing and not unsupported,
                      coveredAdvanceRange=[min(covered), max(covered)] if covered else None,
                      overhangCount=len(overhangs), advanceMismatchCount=len(mismatches),
                      # Problem samples first, bounded even when the caller supplies a large character set.
                      samples=([g for g in glyphs if not g["present"]] +
                               [g for g in glyphs if g in overhangs or g in mismatches] +
                               [g for g in glyphs if g["present"] and g not in overhangs and g not in mismatches])[:8])
        return result
    finally:
        if previous and previous != c.c_void_p(-1).value and dc:
            gdi.SelectObject(dc, previous)
        if font:
            gdi.DeleteObject(font)
        if dc:
            gdi.DeleteDC(dc)
        if registered:
            gdi.RemoveFontResourceExW(font_path, 0x30, None)


def execute(request):
    if os.name != "nt":
        raise ValueError("This probe requires Windows GDI")
    fonts = request.get("fonts")
    text = request.get("text")
    if not isinstance(fonts, list) or not 1 <= len(fonts) <= 4 or not all(isinstance(f, dict) for f in fonts):
        raise ValueError("fonts must contain 1..4 font specifications")
    if not isinstance(text, str) or not 1 <= len(text) <= 512 or "\0" in text:
        raise ValueError("text must contain 1..512 characters without NUL")
    characters = list(dict.fromkeys(ch for ch in text if ch not in "\r\n\t"))
    if not characters:
        raise ValueError("text must contain glyph samples, not only line controls")
    gdi = gdi_api()
    return dict(success=True, fonts=[inspect_font(gdi, font, characters) for font in fonts],
                scope="Primary GDI font coverage and metrics only. Missing glyphs can trigger engine/font fallback; "
                      "their measured widths do not validate fallback rendering. Different face names may be aliases or substitution. "
                      "Ink overhang alone is not a layout error. Engine resets, prerendered glyphs, locale selection, outline/pitch "
                      "and actual in-game rendering require separate checks. No game or font installation was performed.")


def main():
    try:
        data = sys.stdin.buffer.read(32769)
        if len(data) > 32768:
            raise ValueError("request exceeds 32 KiB")
        result = execute(json.loads(data.decode("utf-8-sig")))
    except Exception as error:
        print(json.dumps(dict(success=False, error=str(error)), ensure_ascii=True))
        return 1
    print(json.dumps(result, ensure_ascii=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
