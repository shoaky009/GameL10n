"""FKDS Unicode-indexed G00 glyph pages and Windows font coverage checks."""
import ctypes as c
from ctypes import wintypes as w
from collections import defaultdict
import re
import struct

from siglus_ssu import g00
from siglus_ssu.native_ops import lzss_pack

PREFIXES = ('_moji_AN48_', '_moji_SE48_')
PAGE_SIZE = 441
CELL = 48


def fkds_template(sources):
    body = next((text for name, text in sources.items() if name.lower() == '_00_fkds.ss'), '')
    compact = re.sub(r'\s+', '', body).lower()
    required = ('command$fkds_sys_text(', 'property$text:str', '$text.get_code($cnt)',
                'l[01]=l[00]/$font_cnt[$size_set]', 'l[02]=l[00]%$font_cnt[$size_set]',
                'math.tostr_zero(l[01],4)', '"_moji_an48_"', '"_moji_se48_"')
    counts = [int(x) for x in re.findall(r'#define\s+@G00_MOJI_CNT_[012]\s+(\d+)', body)]
    return all(x in compact for x in required) and counts == [PAGE_SIZE] * 3


class Glyphs:
    """Use the actual selected Windows face, not silent font substitution."""
    def __init__(self, size=CELL - 2):
        self.gdi = c.WinDLL('gdi32', use_last_error=True)
        g = self.gdi
        signatures = {
            'CreateCompatibleDC': ([w.HDC], w.HDC),
            'CreateFontW': ([c.c_int]*5 + [w.DWORD]*8 + [w.LPCWSTR], w.HANDLE),
            'SelectObject': ([w.HDC, w.HANDLE], w.HANDLE),
            'DeleteObject': ([w.HANDLE], w.BOOL), 'DeleteDC': ([w.HDC], w.BOOL),
            'GetTextFaceW': ([w.HDC, c.c_int, w.LPWSTR], c.c_int),
            'GetGlyphIndicesW': ([w.HDC, w.LPCWSTR, c.c_int, c.POINTER(w.WORD), w.DWORD], w.DWORD),
            'GetGlyphOutlineW': ([w.HDC, w.UINT, w.UINT, c.c_void_p, w.DWORD, c.c_void_p, c.c_void_p], w.DWORD),
        }
        for name, (args, ret) in signatures.items():
            fn = getattr(g, name); fn.argtypes = args; fn.restype = ret
        self.dc = g.CreateCompatibleDC(None)
        self.font = g.CreateFontW(-size, 0, 0, 0, 400, 0, 0, 0, 134, 0, 0, 4, 0, 'SimSun')
        if not self.dc or not self.font:
            self.close()
            raise ValueError('无法创建宋体字形，请确认 Windows 已安装 SimSun（宋体）。')
        self.old = g.SelectObject(self.dc, self.font)
        face = c.create_unicode_buffer(128)
        g.GetTextFaceW(self.dc, len(face), face)
        if face.value.casefold() not in ('simsun', '宋体'):
            self.close()
            raise ValueError('系统替换了请求的宋体，不能保证中文字形；请安装 SimSun。')

    def close(self):
        if getattr(self, 'old', None): self.gdi.SelectObject(self.dc, self.old)
        if getattr(self, 'font', None): self.gdi.DeleteObject(self.font)
        if getattr(self, 'dc', None): self.gdi.DeleteDC(self.dc)
        self.font = self.dc = self.old = None

    def require(self, text):
        for ch in set(text):
            if ch.isspace(): continue
            if ord(ch) > 0xffff or 0xd800 <= ord(ch) <= 0xdfff:
                raise ValueError('当前 Siglus 字体方案不支持 BMP 以外的字符：U+%X' % ord(ch))
            index = w.WORD()
            result = self.gdi.GetGlyphIndicesW(self.dc, ch, 1, c.byref(index), 1)
            if result == 0xffffffff or index.value == 0xffff:
                raise ValueError('宋体缺少译文字符：U+%04X' % ord(ch))

    def render(self, ch):
        self.require(ch)
        class Metrics(c.Structure):
            _fields_ = [('width', w.UINT), ('height', w.UINT), ('origin', w.POINT),
                        ('advanceX', c.c_short), ('advanceY', c.c_short)]
        # MAT2: four FIXED (WORD fraction, SHORT value) entries.
        matrix = (c.c_byte * 16).from_buffer_copy(struct.pack('<HhHhHhHh', 0, 1, 0, 0, 0, 0, 0, 1))
        metrics = Metrics()
        n = self.gdi.GetGlyphOutlineW(self.dc, ord(ch), 6, c.byref(metrics), 0, None, matrix)
        if n == 0xffffffff: raise ValueError('无法读取中文字形。')
        data = (c.c_byte * n)()
        if self.gdi.GetGlyphOutlineW(self.dc, ord(ch), 6, c.byref(metrics), n, data, matrix) == 0xffffffff:
            raise ValueError('中文字形读取失败。')
        if metrics.width > CELL - 2 or metrics.height > CELL - 2:
            raise ValueError('中文字形超出原字库单元，不能安全裁切。')
        stride = (metrics.width + 3) & ~3
        # Match the original square glyph cells; retain punctuation's GDI
        # baseline/origin rather than vertically centering every glyph.
        left = (CELL - metrics.advanceX) // 2 + metrics.origin.x
        top = CELL - 6 - metrics.origin.y
        alpha = [0] * (CELL * CELL)
        for y in range(metrics.height):
            for x in range(metrics.width):
                value = min(255, int(data[y * stride + x]) * 255 // 64)
                xx, yy = left + x, top + y
                if value and not (1 <= xx < CELL - 1 and 1 <= yy < CELL - 1):
                    raise ValueError('中文字形越过单元边界，请调整字库基线。')
                if 0 <= xx < CELL and 0 <= yy < CELL: alpha[yy * CELL + xx] = value
        if not any(alpha): raise ValueError('生成的中文字形为空。')
        bgra = bytearray(CELL * CELL * 4)
        for y in range(CELL):
            for x in range(CELL):
                i = y * CELL + x
                edge = max(alpha[yy * CELL + xx] for yy in range(max(0,y-1), min(CELL,y+2))
                           for xx in range(max(0,x-1), min(CELL,x+2)))
                if edge:
                    gray = 255 - alpha[i] * 255 // edge
                    bgra[i*4:i*4+4] = bytes((gray,gray,gray,edge))
        return bytes(bgra)


def decode_page(data):
    if not data or data[0] != 2: raise ValueError('字库页不是 G00 type2 格式。')
    w_, h, n, off, unp, cuts = g00._type2_unp_and_cuts(data)
    if (w_, h, n) != (1008, 1008, PAGE_SIZE) or len(cuts) != PAGE_SIZE:
        raise ValueError('字库页布局不符合 FKDS 48/441 模板。')
    blocks = {}
    for i, pos, length in cuts:
        blk = unp[pos:pos+length]
        if g00._type2_cut_header(blk) != {'center': (0,0), 'canvas': (CELL,CELL)}:
            raise ValueError('字库单元几何信息与模板不符。')
        g00._parse_cut_block(blk, strict=True)
        blocks[i] = blk
    return off, unp, cuts, blocks


def fill_pages(source, characters, font):
    groups = defaultdict(set)
    for ch in characters:
        if ch.isspace(): continue
        if ord(ch) > 0xffff: raise ValueError('FKDS 图片字库只支持 BMP 字符。')
        groups[ord(ch)//PAGE_SIZE].add(ch)
    glyph_cache = {}
    for prefix in PREFIXES:
        for page, chars in sorted(groups.items()):
            relative = 'g00/%s%04d.g00' % (prefix, page)
            path = source / relative
            if not path.is_file():
                raise ValueError('原字库缺少页 %s；此字库版本需要进一步适配。' % relative)
            data = path.read_bytes()
            off, unp, cuts, blocks = decode_page(data)
            replacements = {}
            for ch in chars:
                i = ord(ch) % PAGE_SIZE
                canvas, _, _ = g00._render_cut_canvas(blocks[i])
                if any(canvas[3::4]): continue
                if ch not in glyph_cache: glyph_cache[ch] = font.render(ch)
                replacements[i] = g00._rebuild_type2_cut_block(blocks[i], glyph_cache[ch])
            if not replacements: continue
            out = bytearray(unp[:4+PAGE_SIZE*8]); cursor = len(out)
            for i, start, size in sorted(cuts, key=lambda x:x[1]):
                if start < cursor: raise ValueError('字库单元数据重叠。')
                out.extend(unp[cursor:start])
                block = replacements.get(i, blocks[i])
                struct.pack_into('<II', out, 4+i*8, len(out), len(block))
                out.extend(block); cursor = start + size
            out.extend(unp[cursor:])
            result = data[:off] + lzss_pack(bytes(out), suppress_empty_tail_group=True)
            _, _, _, check = decode_page(result)
            for i, original in blocks.items():
                if check[i] != replacements.get(i, original):
                    raise ValueError('字库回填验证失败。')
            yield relative, result


def validate_pages(source, output, characters):
    rendered={}
    try:
        _validate_pages(source, output, characters, rendered)
    finally:
        if rendered.get('font'): rendered['font'].close()


def _validate_pages(source, output, characters, rendered):
    for prefix in PREFIXES:
        groups = defaultdict(set)
        for ch in characters:
            if not ch.isspace(): groups[ord(ch)//PAGE_SIZE].add(ord(ch)%PAGE_SIZE)
        for page, indices in groups.items():
            name = 'g00/%s%04d.g00' % (prefix,page)
            old = (source/name).read_bytes()
            path = output/name
            new = path.read_bytes() if path.is_file() else old
            _, _, _, blocks = decode_page(new)
            for i in indices:
                if not any(g00._render_cut_canvas(blocks[i])[0][3::4]):
                    raise ValueError('补丁字库仍有缺字：%s #%s' % (name,i))
            if new == old: continue
            _, _, _, original = decode_page(old)
            for i, blk in original.items():
                if blocks[i] != blk and (i not in indices or any(g00._render_cut_canvas(blk)[0][3::4])):
                    raise ValueError('补丁修改了已有字形或与译文无关的字形。')
                if blocks[i]!=blk:
                    ch=chr(page*PAGE_SIZE+i)
                    if ch not in rendered:
                        if 'font' not in rendered: rendered['font']=Glyphs()
                        rendered[ch]=rendered['font'].render(ch)
                    if g00._render_cut_canvas(blocks[i])[0]!=rendered[ch]:
                        raise ValueError('中文字形与系统字体生成结果不一致。')
