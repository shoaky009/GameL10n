"""Reuse shipped dynamic Unifont assets without changing the primary font."""
import struct
from serialized_file import Reader


class Settings:
    def __init__(self, raw):
        r = Reader(raw, 28)
        r.string()
        for _ in range(7): r.boolean()
        r.i32(); r.boolean(); r.ptr(); r.string(); r.skip(28)
        r.boolean(); r.boolean()
        self.count_offset = r.pos
        self.fallbacks = [r.ptr() for _ in range(r.count(12))]
        self.after = r.pos
        r.boolean(); r.ptr(); r.string(); r.boolean(); r.i32()
        r.string(); r.ptr(); r.string(); r.ptr(); r.ptr(); r.boolean()
        if r.pos != len(raw):
            raise ValueError('TMP Settings 布局不受支持')

    def add(self, raw, ptr):
        if ptr in self.fallbacks:
            return raw
        return (raw[:self.count_offset] + struct.pack('<i', len(self.fallbacks) + 1)
                + raw[self.count_offset + 4:self.after] + struct.pack('<iq', *ptr) + raw[self.after:])


class Font:
    def __init__(self, raw):
        r = Reader(raw, 28)
        r.string(); r.i32(); r.ptr(); r.i32()
        if r.string() != '1.1.0':
            raise ValueError('不支持的 TMP FontAsset 版本')
        r.string()
        self.source = r.ptr()
        self.mode = r.i32()
        r.i32()
        self.family = r.string()
        r.string(); r.skip(72); r.array(52); r.array(16)
        atlas_count = r.count(12)
        r.skip(atlas_count * 12)
        index = r.i32()
        self.multi_atlas_offset = r.pos
        self.multi_atlas = r.boolean() != 0
        r.boolean()
        if not 0 <= index < atlas_count:
            raise ValueError('TMP atlas 索引错误')


class Coverage:
    def __init__(self, raw):
        self.raw = raw
        self.maps = []
        start, cmap, maxp = -1, -1, -1
        def u16(p): return struct.unpack_from('>H', raw, p)[0]
        def u32(p): return struct.unpack_from('>I', raw, p)[0]
        self.u16, self.u32 = u16, u32
        pos = 0
        while True:
            pos = raw.find(b'\x00\x01\x00\x00', pos)
            if pos < 0 or pos + 12 > len(raw):
                break
            count = u16(pos + 4)
            if 5 <= count <= 100 and pos + 12 + count * 16 <= len(raw):
                found, valid = {}, True
                for i in range(count):
                    at = pos + 12 + i * 16
                    tag = raw[at:at + 4]
                    offset, size = u32(at + 8), u32(at + 12)
                    if pos + offset + size > len(raw) or any(c < 32 or c > 126 for c in tag):
                        valid = False
                        break
                    found[tag] = pos + offset
                if valid and b'cmap' in found and b'maxp' in found:
                    start, cmap, maxp = pos, found[b'cmap'], found[b'maxp']
                    break
            pos += 1
        if start < 0:
            raise ValueError('源字体缺少可验证的 TrueType cmap')
        self.num_glyphs = u16(maxp + 4)
        for i in range(u16(cmap + 2)):
            at = cmap + 4 + i * 8
            platform, encoding = u16(at), u16(at + 2)
            if platform != 0 and (platform != 3 or encoding not in (1, 10)):
                continue
            sub = cmap + u32(at + 4)
            if u16(sub) in (4, 12): self.maps.append(sub)
        self.maps.sort(key=u16, reverse=True)
        if not self.maps: raise ValueError('源字体无 Unicode cmap')

    def glyph(self, code):
        u16, u32 = self.u16, self.u32
        for sub in self.maps:
            glyph = 0
            if u16(sub) == 12:
                lo, hi = 0, u32(sub + 12) - 1
                while lo <= hi:
                    mid = (lo + hi) // 2
                    at = sub + 16 + mid * 12
                    first, last = u32(at), u32(at + 4)
                    if code < first: hi = mid - 1
                    elif code > last: lo = mid + 1
                    else:
                        glyph = u32(at + 8) + code - first
                        break
            elif code <= 65535:
                count = u16(sub + 6) // 2
                end = sub + 14
                begin = end + count * 2 + 2
                delta = begin + count * 2
                ranges = delta + count * 2
                for i in range(count):
                    if code > u16(end + i * 2): continue
                    first = u16(begin + i * 2)
                    if code < first: break
                    offset, add = u16(ranges + i * 2), u16(delta + i * 2)
                    if offset == 0: glyph = (code + add) & 65535
                    else:
                        glyph = u16(ranges + i * 2 + offset + 2 * (code - first))
                        if glyph: glyph = (glyph + add) & 65535
                    break
            if 0 < glyph < self.num_glyphs: return glyph
        return 0


def prepare(unity, changed):
    needed = {ord(c) for text in changed for c in text if ord(c) > 127 and not (127 <= ord(c) <= 159)}
    if not needed:
        return {}
    settings, fonts, patches = [], [], {}
    for path, asset in unity.assets.items():
        for obj in asset.objects.values():
            if obj.class_id != 114: continue
            raw = asset.raw(obj)
            script = unity.script(path, raw)
            if script == 'TMPro.TMP_Settings': settings.append((path, obj.path_id, raw, Settings(raw)))
            elif script == 'TMPro.TMP_FontAsset':
                try:
                    font = Font(raw)
                    if font.mode == 1 and 'unifont' in font.family.casefold():
                        fonts.append((path, obj.path_id, raw, font))
                except (ValueError, struct.error):
                    pass  # Other TMP layouts cannot be used as fallback candidates.
    if not settings:
        raise ValueError('中文回填需要 TMP_Settings 全局回退配置及其依赖资源')
    coverage = {}
    for path, pid, raw, setting in settings:
        found = False
        for font_path, font_id, font_raw, font in sorted(fonts, key=lambda f: f[0] != path):
            file_id = 0
            if font_path != path:
                for index in range(1, len(unity.assets[path].externals) + 1):
                    try:
                        if unity.resolve(path, index) == font_path:
                            file_id = index
                            break
                    except ValueError:
                        continue
                if file_id == 0: continue
            source_path = unity.resolve(font_path, font.source[0])
            key = (source_path, font.source[1])
            if key not in coverage:
                coverage[key] = Coverage(unity.assets[source_path].raw(font.source[1]))
            if any(coverage[key].glyph(c) == 0 for c in needed): continue
            patches[path, pid] = setting.add(raw, (file_id, font_id))
            if not font.multi_atlas:
                updated = bytearray(font_raw)
                updated[font.multi_atlas_offset] = 1
                patches[font_path, font_id] = bytes(updated)
            found = True
            break
        if not found:
            raise ValueError('内嵌动态 Unifont 缺少所需字形或依赖；不能静默替换字符')
    return patches
