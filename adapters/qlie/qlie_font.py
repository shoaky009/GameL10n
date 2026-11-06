"""Derive SFNT fonts without changing outlines, glyph advances, or source files."""
from pathlib import Path
import struct


def u16(data, at):
    return struct.unpack_from('>H', data, at)[0]


def u32(data, at):
    return struct.unpack_from('>I', data, at)[0]


def w16(data, at, value):
    struct.pack_into('>H', data, at, value & 65535)


def w32(data, at, value):
    struct.pack_into('>I', data, at, value & 0xffffffff)


def checksum(data):
    padded = bytes(data)+b'\0'*((-len(data)) % 4)
    return sum(v[0] for v in struct.iter_unpack('>I', padded)) & 0xffffffff


def read_map(data):
    result = {}
    for i in range(u16(data, 2)):
        record = 4+8*i
        if u16(data, record) not in (0, 3):
            continue
        p = u32(data, record+4)
        form = u16(data, p)
        if form == 12:
            for j in range(u32(data, p+12)):
                a, z, glyph = struct.unpack_from('>III', data, p+16+j*12)
                if z > 0x10ffff or a > z:
                    raise ValueError('字体 cmap 范围无效。')
                result.update((c, glyph+c-a) for c in range(a, z+1) if glyph+c-a)
        elif form == 4:
            count = u16(data, p+6)//2
            ends, starts = p+14, p+16+count*2
            delta, ranges = starts+count*2, starts+count*4
            for j in range(count):
                a, z, d, r = u16(data, starts+j*2), u16(data, ends+j*2), u16(data, delta+j*2), u16(data, ranges+j*2)
                for c in range(a, min(z+1, 65535)):
                    glyph = (c+d) & 65535 if not r else u16(data, ranges+j*2+r+(c-a)*2)
                    if r and glyph:
                        glyph = (glyph+d) & 65535
                    if glyph:
                        result[c] = glyph
    return result


def cmap(mapping):
    keys, segments = sorted(c for c in mapping if c < 65535), []
    i = 0
    while i < len(keys):
        a, j = keys[i], i+1
        delta = (mapping[a]-a) & 65535
        while j < len(keys) and keys[j] == keys[j-1]+1 and (mapping[keys[j]]-keys[j]) & 65535 == delta:
            j += 1
        if j-i >= 3:
            segments.append((a, keys[j-1], delta, None))
            i = j
            continue
        glyphs, j = [mapping[a]], i+1
        while j < len(keys) and keys[j] == keys[j-1]+1:
            k, d = j+1, (mapping[keys[j]]-keys[j]) & 65535
            while k < len(keys) and keys[k] == keys[k-1]+1 and (mapping[keys[k]]-keys[k]) & 65535 == d:
                k += 1
            if k-j >= 3:
                break
            glyphs.append(mapping[keys[j]])
            j += 1
        segments.append((a, keys[j-1], 0, glyphs))
        i = j
    segments.append((65535, 65535, 1, None))
    n = len(segments)
    cursor = 16+n*8
    length = cursor+sum(len(s[3])*2 for s in segments if s[3] is not None)
    if length > 65535:
        raise ValueError('字体 BMP cmap 超过格式4容量。')
    f4 = bytearray(length)
    power = 1 << (n.bit_length()-1)
    for at, value in [(0, 4), (2, length), (6, n*2), (8, power*2), (10, power.bit_length()-1), (12, n*2-power*2)]:
        w16(f4, at, value)
    for i, (a, z, delta, glyphs) in enumerate(segments):
        w16(f4, 14+i*2, z)
        w16(f4, 16+n*2+i*2, a)
        w16(f4, 16+n*4+i*2, delta)
        if glyphs is not None:
            at = 16+n*6+i*2
            w16(f4, at, cursor-at)
            for glyph in glyphs:
                w16(f4, cursor, glyph)
                cursor += 2
    groups = []
    for c, glyph in sorted(mapping.items()):
        if groups and c == groups[-1][1]+1 and glyph == groups[-1][2]+c-groups[-1][0]:
            groups[-1][1] = c
        else:
            groups.append([c, c, glyph])
    f12 = bytearray(16+len(groups)*12)
    w16(f12, 0, 12)
    w32(f12, 4, len(f12))
    w32(f12, 12, len(groups))
    for i, group in enumerate(groups):
        struct.pack_into('>III', f12, 16+i*12, *group)
    result = bytearray(36)+f4+f12
    w16(result, 2, 4)
    for i, (platform, encoding) in enumerate([(0, 3), (0, 4), (3, 1), (3, 10)]):
        struct.pack_into('>HHI', result, 4+i*8, platform, encoding, 36+(len(f4) if i % 2 else 0))
    return result


def name_table(old, family, revision, regular_full_name=False):
    records = []
    start = u16(old, 4)
    for i in range(u16(old, 2)):
        platform, encoding, language, identity, length, offset = struct.unpack_from('>6H', old, 6+i*12)
        value = old[start+offset:start+offset+length]
        text = None
        if identity in (1, 16, 21):
            text = family
        elif identity == 4:
            text = family+(' Regular' if revision >= 2 or regular_full_name else '')
        elif identity in (2, 17, 22):
            text = 'Regular'
        elif identity == 3:
            text = family+';QLIE;'+str(revision)
        elif identity == 6:
            text = family.replace(' ', '')+'-Regular'
        if text is not None:
            if platform in (0, 3):
                value = text.encode('utf-16be')
            elif platform == 1:
                value = text.encode('ascii')
            else:
                continue
        records.append((platform, encoding, language, identity, value))
    start = 6+len(records)*12
    result, payload = bytearray(start), bytearray()
    w16(result, 2, len(records))
    w16(result, 4, start)
    for i, (*head, value) in enumerate(records):
        struct.pack_into('>6H', result, 6+i*12, *head, len(value), len(payload))
        payload += value
    return result+payload


def create(source, target, family, aliases, revision=3, coverage=None, regular_full_name=False):
    original = Path(source).read_bytes()
    tables = {}
    for i in range(u16(original, 4)):
        p = 12+i*16
        tag = original[p:p+4].decode('ascii')
        if tag != 'DSIG':
            offset, length = u32(original, p+8), u32(original, p+12)
            tables[tag] = bytearray(original[offset:offset+length])
    mapping = read_map(tables['cmap'])
    for slot, c in aliases.items():
        if c not in mapping:
            raise ValueError(f'字体缺少字符 U+{c:04X}')
        mapping[slot] = mapping[c]
    if coverage is not None:
        missing = sorted({ord(c) for text in coverage for c in text if ord(c) >= 32 and (ord(c) > 65535 or ord(c) not in mapping)})
        if missing:
            raise ValueError('汉化字体无法覆盖部分字符：'+', '.join(f'U+{c:04X}' for c in missing[:20]))
    tables['cmap'] = cmap(mapping)
    tables['name'] = name_table(tables['name'], family, revision, regular_full_name)
    os2 = tables['OS/2']
    if len(os2) >= 86:
        w32(os2, 78, u32(os2, 78) | (1 << 17))
    w32(tables['head'], 8, 0)
    if revision >= 3:
        em = u16(tables['head'], 18)
        ascent = (em*220+128)//256
        descent = em-ascent
        for at, value in [(2, (em+1)//2), (68, ascent), (70, -descent), (72, 0), (74, ascent), (76, descent)]:
            w16(os2, at, value)
        for at, value in [(4, ascent), (6, -descent), (8, 0)]:
            w16(tables['hhea'], at, value)
        tables.pop('MVAR', None)
    n = len(tables)
    offset = 12+n*16
    result = bytearray(offset+sum((len(v)+3) & ~3 for v in tables.values()))
    result[:12] = original[:12]
    power = 1 << (n.bit_length()-1)
    for at, value in [(4, n), (6, power*16), (8, power.bit_length()-1), (10, n*16-power*16)]:
        w16(result, at, value)
    head = None
    for i, (tag, value) in enumerate(sorted(tables.items())):
        p = 12+i*16
        result[p:p+4] = tag.encode('ascii')
        struct.pack_into('>III', result, p+4, checksum(value), offset, len(value))
        result[offset:offset+len(value)] = value
        if tag == 'head':
            head = offset
        offset += (len(value)+3) & ~3
    w32(result, head+8, 0xB1B0AFBA-checksum(result))
    Path(target).parent.mkdir(parents=True, exist_ok=True)
    Path(target).write_bytes(result)
