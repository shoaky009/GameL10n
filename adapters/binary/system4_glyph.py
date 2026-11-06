"""Windows GDI bitmap renderer for the bundled Noto FNL variant."""
import base64
import ctypes as c
from ctypes import wintypes as w


class Point(c.Structure):
    _fields_ = [('x', w.LONG), ('y', w.LONG)]


class Metrics(c.Structure):
    _fields_ = [('black_x', w.UINT), ('black_y', w.UINT), ('origin', Point),
                ('inc_x', c.c_short), ('inc_y', c.c_short)]


class Matrix(c.Structure):
    _fields_ = [(name, c.c_short) for name in 'abcdefgh']


def render_noto(jobs, font_path):
    gdi = c.WinDLL('gdi32', use_last_error=True)
    signatures = {
        'AddFontResourceExW': ([w.LPCWSTR, w.DWORD, w.LPVOID], c.c_int),
        'RemoveFontResourceExW': ([w.LPCWSTR, w.DWORD, w.LPVOID], w.BOOL),
        'CreateCompatibleDC': ([w.HDC], w.HDC), 'DeleteDC': ([w.HDC], w.BOOL),
        'SelectObject': ([w.HDC, w.HANDLE], w.HANDLE), 'DeleteObject': ([w.HANDLE], w.BOOL),
        'CreateFontW': ([c.c_int]*5+[w.DWORD]*8+[w.LPCWSTR], w.HANDLE),
        'GetGlyphIndicesW': ([w.HDC, w.LPCWSTR, c.c_int, c.POINTER(w.WORD), w.DWORD], w.DWORD),
        'GetGlyphOutlineW': ([w.HDC, w.UINT, w.UINT, c.POINTER(Metrics), w.DWORD,
                              w.LPVOID, c.POINTER(Matrix)], w.DWORD)}
    for name, (args, result) in signatures.items():
        method = getattr(gdi, name)
        method.argtypes, method.restype = args, result
    if not font_path.is_file() or not gdi.AddFontResourceExW(str(font_path), 0x10, None):
        raise ValueError('Cannot load packaged Noto font')
    dc = gdi.CreateCompatibleDC(None)
    try:
        if not dc:
            raise ValueError('GDI DC failed')
        result = []
        for job in jobs:
            width, height, char = job['width'], job['height'], job['character']
            stride = (width+31)//32*4
            reference = base64.b64decode(job['reference'])
            pixels = [(x, y) for y in range(height) for x in range(width)
                      if reference[(height-1-y)*stride+x//8] & (128>>(x%8))]
            if not pixels:
                raise ValueError('Empty reference CJK glyph')
            min_x, max_x = min(x for x, _ in pixels), max(x for x, _ in pixels)
            min_y, max_y = min(y for _, y in pixels), max(y for _, y in pixels)
            font = gdi.CreateFontW(-width, 0, 0, 0, 400, 0, 0, 0, 1, 0, 0, 3, 0, 'Noto Sans SC')
            if not font:
                raise ValueError('CreateFont failed')
            previous = gdi.SelectObject(dc, font)
            try:
                index = w.WORD()
                if gdi.GetGlyphIndicesW(dc, char, 1, c.byref(index), 1) == 0xffffffff or index.value in (0, 0xffff):
                    raise ValueError(f'Packaged font lacks U+{ord(char):04X}')
                matrix, metrics = Matrix(b=1, h=1), Metrics()
                size = gdi.GetGlyphOutlineW(dc, ord(char), 1, c.byref(metrics), 0, None, c.byref(matrix))
                if size in (0, 0xffffffff) or not metrics.black_x or not metrics.black_y:
                    raise ValueError(f'Cannot rasterize U+{ord(char):04X}')
                bits = c.create_string_buffer(size)
                if gdi.GetGlyphOutlineW(dc, ord(char), 1, c.byref(metrics), size, bits, c.byref(matrix)) == 0xffffffff:
                    raise ValueError('Glyph bitmap failed')
                bits = bits.raw
                source_stride = (metrics.black_x+31)//32*4
                scale = min((max_x-min_x+1)/metrics.black_x, (max_y-min_y+1)/metrics.black_y)
                target_w, target_h = max(1, round(metrics.black_x*scale)), max(1, round(metrics.black_y*scale))
                origin_x = min_x + (max_x-min_x+1-target_w)//2
                origin_y = min_y + (max_y-min_y+1-target_h)//2
                output = bytearray(stride*height)
                for y in range(target_h):
                    for x in range(target_w):
                        sx, sy = x*metrics.black_x//target_w, y*metrics.black_y//target_h
                        if bits[sy*source_stride+sx//8] & (128>>(sx%8)):
                            output[(height-1-origin_y-y)*stride+(origin_x+x)//8] |= 128>>((origin_x+x)%8)
                if not any(output):
                    raise ValueError('Empty generated glyph')
                result.append(bytes(output))
            finally:
                gdi.SelectObject(dc, previous)
                gdi.DeleteObject(font)
        return result
    finally:
        if dc:
            gdi.DeleteDC(dc)
        gdi.RemoveFontResourceExW(str(font_path), 0x10, None)
