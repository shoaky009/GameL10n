"""Read-only GDI font metrics and optional GDI+ creation probe. JSON stdin/stdout.

Run in a short-lived Python process. Private font registrations are removed before exit.
This probes native APIs, not a game's renderer or its fallback chain. No font installation.
"""
import ctypes as c
from ctypes import wintypes as w
from contextlib import contextmanager, nullcontext
import json
import os
from pathlib import Path
from statistics import median
import sys


class LOGFONT(c.Structure):
    _fields_ = [(name, w.LONG) for name in ("height", "width", "escapement", "orientation", "weight")] + [
        (name, w.BYTE) for name in ("italic", "underline", "strikeOut", "charSet", "outPrecision",
                                   "clipPrecision", "quality", "pitchAndFamily")
    ] + [("faceName", w.WCHAR * 32)]


class LOGFONTA(c.Structure):
    _fields_ = LOGFONT._fields_[:-1] + [("faceName", c.c_char * 32)]


class GDIPLUS_STARTUP_INPUT(c.Structure):
    _fields_ = [("version", w.UINT), ("debugEventCallback", c.c_void_p),
                ("suppressBackgroundThread", w.BOOL), ("suppressExternalCodecs", w.BOOL)]


class SIZE(c.Structure):
    _fields_ = [("cx", w.LONG), ("cy", w.LONG)]


class POINT(c.Structure):
    _fields_ = [("x", w.LONG), ("y", w.LONG)]


class GLYPHMETRICS(c.Structure):
    _fields_ = [("blackBoxX", w.UINT), ("blackBoxY", w.UINT), ("origin", POINT),
                ("cellIncX", c.c_short), ("cellIncY", c.c_short)]


class TEXTMETRIC(c.Structure):
    _fields_ = [(name, w.LONG) for name in (
        "height", "ascent", "descent", "internalLeading", "externalLeading", "averageWidth",
        "maxWidth", "weight", "overhang", "digitizedAspectX", "digitizedAspectY"
    )] + [(name, w.WCHAR) for name in ("firstChar", "lastChar", "defaultChar", "breakChar")] + [
        (name, w.BYTE) for name in ("italic", "underlined", "struckOut", "pitchAndFamily", "charSet")
    ]


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
        "GetObjectA": ([c.c_void_p, c.c_int, c.c_void_p], c.c_int),
        "GetObjectW": ([c.c_void_p, c.c_int, c.c_void_p], c.c_int),
        "GetTextFaceW": ([c.c_void_p, c.c_int, w.LPWSTR], c.c_int),
        "GetTextMetricsW": ([c.c_void_p, c.POINTER(TEXTMETRIC)], w.BOOL),
        "GetGlyphIndicesW": ([c.c_void_p, w.LPCWSTR, c.c_int, c.POINTER(w.WORD), w.DWORD], w.DWORD),
        "GetTextExtentPoint32W": ([c.c_void_p, w.LPCWSTR, c.c_int, c.POINTER(SIZE)], w.BOOL),
        "GetGlyphOutlineW": ([c.c_void_p, w.UINT, w.UINT, c.POINTER(GLYPHMETRICS),
                               w.DWORD, c.c_void_p, c.POINTER(MAT2)], w.DWORD),
    }
    for name, (args, result) in signatures.items():
        fn = getattr(gdi, name)
        fn.argtypes, fn.restype = args, result
    return gdi


GDIPLUS_STATUS_NAMES = (
    "Ok", "GenericError", "InvalidParameter", "OutOfMemory", "ObjectBusy", "InsufficientBuffer",
    "NotImplemented", "Win32Error", "WrongState", "Aborted", "FileNotFound", "ValueOverflow",
    "AccessDenied", "UnknownImageFormat", "FontFamilyNotFound", "FontStyleNotFound", "NotTrueTypeFont",
    "UnsupportedGdiplusVersion", "GdiplusNotInitialized", "PropertyNotFound", "PropertyNotSupported",
    "ProfileNotFound",
)


def gdiplus_status(status, pointer=None):
    result = dict(status=status, statusName=GDIPLUS_STATUS_NAMES[status]
                  if 0 <= status < len(GDIPLUS_STATUS_NAMES) else "UnknownStatus")
    if pointer is not None:
        result["created"] = status == 0 and bool(pointer.value)
    return result


@contextmanager
def gdiplus_session():
    gdip = c.WinDLL("gdiplus", use_last_error=True)
    pointer = c.c_void_p
    signatures = {
        "GdiplusStartup": ([c.POINTER(c.c_size_t), c.POINTER(GDIPLUS_STARTUP_INPUT), pointer], c.c_int),
        "GdiplusShutdown": ([c.c_size_t], None),
        "GdipCreateFontFromDC": ([pointer, c.POINTER(pointer)], c.c_int),
        "GdipCreateFontFromLogfontA": ([pointer, c.POINTER(LOGFONTA), c.POINTER(pointer)], c.c_int),
        "GdipCreateFontFromLogfontW": ([pointer, c.POINTER(LOGFONT), c.POINTER(pointer)], c.c_int),
        "GdipCreateFontFamilyFromName": ([w.LPCWSTR, pointer, c.POINTER(pointer)], c.c_int),
        "GdipCreateFont": ([pointer, c.c_float, c.c_int, c.c_int, c.POINTER(pointer)], c.c_int),
        "GdipDeleteFont": ([pointer], c.c_int),
        "GdipDeleteFontFamily": ([pointer], c.c_int),
        "GdipNewPrivateFontCollection": ([c.POINTER(pointer)], c.c_int),
        "GdipPrivateAddFontFile": ([pointer, w.LPCWSTR], c.c_int),
        "GdipDeletePrivateFontCollection": ([c.POINTER(pointer)], c.c_int),
    }
    for name, (args, result) in signatures.items():
        fn = getattr(gdip, name)
        fn.argtypes, fn.restype = args, result
    token = c.c_size_t()
    startup = GDIPLUS_STARTUP_INPUT(1, None, False, False)
    status = gdip.GdiplusStartup(c.byref(token), c.byref(startup), None)
    if status:
        raise OSError(f"GdiplusStartup failed: {status} ({gdiplus_status(status)['statusName']})")
    try:
        yield gdip
    finally:
        gdip.GdiplusShutdown(token)


def inspect_gdiplus(gdip, gdi, dc, font, lf, em_height, font_path):
    def create_font(function, *args):
        created = c.c_void_p()
        try:
            return gdiplus_status(function(*args, c.byref(created)), created)
        finally:
            if created.value:
                gdip.GdipDeleteFont(created)

    # GDI+ FontStyle flags and UnitPixel. Use the measured GDI em height for named fonts.
    style = (int(lf.weight >= 700) | (int(bool(lf.italic)) << 1)
             | (int(bool(lf.underline)) << 2) | (int(bool(lf.strikeOut)) << 3))
    em_size = max(1, em_height)

    def by_name(collection):
        family = c.c_void_p()
        try:
            status = gdip.GdipCreateFontFamilyFromName(lf.faceName, collection, c.byref(family))
            family_result = gdiplus_status(status, family)
            font_result = (create_font(gdip.GdipCreateFont, family, em_size, style, 2)
                           if family_result["created"] else
                           dict(attempted=False, reason="Font family creation failed"))
            return dict(family=family_result, font=font_result)
        finally:
            if family.value:
                gdip.GdipDeleteFontFamily(family)

    result = dict(fromDC=create_font(gdip.GdipCreateFontFromDC, dc))
    for suffix, structure in (("A", LOGFONTA), ("W", LOGFONT)):
        logical_font = structure()
        if getattr(gdi, "GetObject" + suffix)(font, c.sizeof(logical_font), c.byref(logical_font)) == c.sizeof(logical_font):
            result["fromLogfont" + suffix] = create_font(
                getattr(gdip, "GdipCreateFontFromLogfont" + suffix), dc, c.byref(logical_font))
        else:
            result["fromLogfont" + suffix] = dict(attempted=False, reason="GetObject" + suffix + " failed")
    result["byName"] = by_name(None)
    result["namedFontRequest"] = dict(face=lf.faceName, emSize=em_size, style=style, unit="Pixel")
    if not font_path:
        result["privateCollection"] = dict(attempted=False, reason="No font file supplied")
        return result
    collection = c.c_void_p()
    private = result["privateCollection"] = {}
    try:
        private["createCollection"] = gdiplus_status(
            gdip.GdipNewPrivateFontCollection(c.byref(collection)), collection)
        if private["createCollection"]["created"]:
            private["addFontFile"] = gdiplus_status(gdip.GdipPrivateAddFontFile(collection, font_path))
            if private["addFontFile"]["status"] == 0:
                private.update(by_name(collection))
    finally:
        if collection.value:
            gdip.GdipDeletePrivateFontCollection(c.byref(collection))
    return result


def integer(spec, key, default, minimum, maximum):
    value = spec.get(key, default)
    if type(value) is not int or not minimum <= value <= maximum:
        raise ValueError(f"{key} must be an integer in {minimum}..{maximum}")
    return value


def inspect_font(gdi, spec, characters, gdip=None):
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
        tm = TEXTMETRIC()
        if not gdi.GetTextMetricsW(dc, c.byref(tm)):
            raise OSError("GetTextMetricsW failed")
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
                                # Baseline is y=0; negative coordinates lie above it.
                                inkTop=-metrics.origin.y,
                                inkBottom=metrics.blackBoxY - metrics.origin.y,
                                inkHeight=metrics.blackBoxY,
                                rightOverhang=max(0, metrics.origin.x + metrics.blackBoxX - size.cx))
            glyphs.append(item)
        overhangs = [g for g in glyphs if g.get("rightOverhang", 0) > 0]
        mismatches = [g for g in glyphs if "glyphAdvance" in g and g["glyphAdvance"] != g["measuredAdvance"]]
        covered = [g["measuredAdvance"] for g in glyphs if g["present"]]
        ink_heights = [g["inkHeight"] for g in glyphs
                       if not g["character"].isspace() and g.get("inkHeight", 0) > 0]
        result = dict(requestedFace=face, selectedFace=selected.value,
                      faceNameDiffers=selected.value.casefold() != face.casefold(),
                      height=lf.height, width=lf.width, charset=lf.charSet,
                      characterCount=len(characters), missingCount=len(missing),
                      missingCharacters="".join(missing[:32]), unsupportedCount=len(unsupported),
                      unsupportedCodepoints=unsupported[:16],
                      coversSample=not missing and not unsupported,
                      coveredAdvanceRange=[min(covered), max(covered)] if covered else None,
                      coveredInkHeightRange=[min(ink_heights), max(ink_heights)] if ink_heights else None,
                      textMetrics=dict(height=tm.height, ascent=tm.ascent, descent=tm.descent,
                                       internalLeading=tm.internalLeading, externalLeading=tm.externalLeading,
                                       emHeight=tm.height - tm.internalLeading,
                                       averageWidth=tm.averageWidth, maxWidth=tm.maxWidth,
                                       weight=tm.weight, italic=bool(tm.italic),
                                       pitchAndFamily=tm.pitchAndFamily, charset=tm.charSet),
                      overhangCount=len(overhangs), advanceMismatchCount=len(mismatches),
                      # Problem samples first, bounded even when the caller supplies a large character set.
                      samples=([g for g in glyphs if not g["present"]] +
                               [g for g in glyphs if g in overhangs or g in mismatches] +
                               [g for g in glyphs if g["present"] and g not in overhangs and g not in mismatches])[:8])
        if gdip is not None:
            result["gdiplus"] = inspect_gdiplus(gdip, gdi, dc, font, lf,
                                               tm.height - tm.internalLeading, font_path)
        return result, glyphs
    finally:
        if previous and previous != c.c_void_p(-1).value and dc:
            gdi.SelectObject(dc, previous)
        if font:
            gdi.DeleteObject(font)
        if dc:
            gdi.DeleteDC(dc)
        if registered:
            gdi.RemoveFontResourceExW(font_path, 0x30, None)


def compare_fonts(reference, candidate, index):
    original, original_glyphs = reference
    replacement, replacement_glyphs = candidate
    covered = {g["character"]: g for g in original_glyphs if g["present"]}
    shared = [(covered[g["character"]], g) for g in replacement_glyphs
              if g["present"] and g["character"] in covered]
    outlined = [(a, b) for a, b in shared
                if not a["character"].isspace()
                and all(g.get("inkWidth", 0) > 0 and g.get("inkHeight", 0) > 0 for g in (a, b))]

    def ratio(pairs, field):
        values = [b[field] / a[field] for a, b in pairs if a.get(field, 0) > 0 and b.get(field, 0) > 0]
        return round(median(values), 4) if values else None

    a, b = original["textMetrics"], replacement["textMetrics"]
    return dict(referenceIndex=0, candidateIndex=index, sharedGlyphCount=len(shared),
                sharedOutlineGlyphCount=len(outlined), advanceRatio=ratio(shared, "measuredAdvance"),
                inkWidthRatio=ratio(outlined, "inkWidth"), inkHeightRatio=ratio(outlined, "inkHeight"),
                emHeightRatio=round(b["emHeight"] / a["emHeight"], 4) if a["emHeight"] > 0 else None,
                baselineDelta=b["ascent"] - a["ascent"], cellHeightDelta=b["height"] - a["height"])


def execute(request):
    if os.name != "nt":
        raise ValueError("This probe requires Windows GDI")
    renderer = request.get("renderer", "gdi")
    if renderer not in ("gdi", "gdiplus"):
        raise ValueError("renderer must be gdi or gdiplus")
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
    with gdiplus_session() if renderer == "gdiplus" else nullcontext(None) as gdip:
        measurements = [inspect_font(gdi, font, characters, gdip) for font in fonts]
    result = dict(success=True, fonts=[result for result, _ in measurements],
                comparisons=[compare_fonts(measurements[0], candidate, index)
                             for index, candidate in enumerate(measurements[1:], 1)],
                scope="Primary GDI font coverage and metrics only. Missing glyphs can trigger engine/font fallback; "
                      "their measured widths do not validate fallback rendering. Different face names may be aliases or substitution. "
                      "Comparisons use font 0 and shared covered glyphs; ratios are candidate/reference medians, not scale prescriptions. "
                      "Ink overhang alone is not a layout error. Engine resets, prerendered glyphs, locale selection, outline/pitch "
                      "and actual in-game rendering require separate checks. No game or font installation was performed.")
    if renderer == "gdiplus":
        result["renderer"] = renderer
        result["scope"] += (" GDI+ results report font creation from the selected DC, ANSI/Unicode LOGFONTs read from its HFONT, "
                            "name lookup in the installed collection, and a separate private collection loaded from file. "
                            "Private collection success does not prove a game uses that collection. Native statuses are API outcomes, "
                            "not root causes; NotTrueTypeFont (16) alone does not diagnose CFF/OTF incompatibility. "
                            "These calls do not validate GDI+ drawing or fallback rendering.")
    return result


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
