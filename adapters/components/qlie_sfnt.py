"""Derive QLIE TrueType fonts while retaining glyph outlines and advances."""
import struct


def u16(data, offset):
    return struct.unpack_from(">H", data, offset)[0]


def u32(data, offset):
    return struct.unpack_from(">I", data, offset)[0]


def put16(data, offset, value):
    struct.pack_into(">H", data, offset, value & 0xffff)


def checksum(data):
    aligned = len(data) & ~3
    total = sum(value for value, in struct.iter_unpack(">I", memoryview(data)[:aligned]))
    return (total + int.from_bytes(data[aligned:].ljust(4, b"\0"), "big")) & 0xffffffff


def read_cmap(data):
    mapping, seen = {}, set()
    for index in range(u16(data, 2)):
        platform, _, offset = struct.unpack_from(">HHI", data, 4 + 8 * index)
        if platform not in (0, 3) or offset in seen:
            continue
        seen.add(offset)
        format_number = u16(data, offset)
        if format_number == 12:
            for group in range(u32(data, offset + 12)):
                start, end, first = struct.unpack_from(">III", data, offset + 16 + group * 12)
                if not start <= end <= 0x10ffff or first + end - start > 0xffff:
                    raise ValueError("QLIE 字体 cmap 范围无效")
                mapping.update((code, first + code - start) for code in range(start, end + 1)
                               if first + code - start)
        elif format_number == 4:
            count = u16(data, offset + 6) // 2
            ends, starts = offset + 14, offset + 16 + count * 2
            deltas, ranges = starts + count * 2, starts + count * 4
            for index in range(count):
                start, end = u16(data, starts + index * 2), u16(data, ends + index * 2)
                delta = u16(data, deltas + index * 2)
                range_offset = ranges + index * 2
                base = u16(data, range_offset)
                for code in range(start, min(end + 1, 0xffff)):
                    if base:
                        glyph = u16(data, range_offset + base + (code - start) * 2)
                        if not glyph:
                            continue
                        glyph = (glyph + delta) & 0xffff
                    else:
                        glyph = (code + delta) & 0xffff
                    if glyph:
                        mapping[code] = glyph
    return mapping


def bmp_segments(mapping):
    codes = sorted(code for code in mapping if code < 0xffff)
    segments = []

    def delta_run(index):
        start = codes[index]
        delta = (mapping[start] - start) & 0xffff
        end = index + 1
        while (end < len(codes) and codes[end] == codes[end - 1] + 1
               and (mapping[codes[end]] - codes[end]) & 0xffff == delta):
            end += 1
        return end, delta

    index = 0
    while index < len(codes):
        start = codes[index]
        end, delta = delta_run(index)
        if end - index >= 3:
            segments.append((start, codes[end - 1], delta, []))
        else:
            # Short delta runs share one glyph array to keep the format 4 table under 64 KiB.
            end = index + 1
            while end < len(codes) and codes[end] == codes[end - 1] + 1:
                next_end, _ = delta_run(end)
                if next_end - end >= 3:
                    break
                end += 1
            segments.append((start, codes[end - 1], 0, [mapping[code] for code in codes[index:end]]))
        index = end
    return [*segments, (0xffff, 0xffff, 1, [])]


def build_cmap(mapping):
    segments = bmp_segments(mapping)
    count = len(segments)
    cursor = 16 + count * 8
    size = cursor + sum(len(glyphs) * 2 for _, _, _, glyphs in segments)
    if size > 0xffff:
        raise ValueError("QLIE 字体 BMP cmap 超过格式 4 容量")
    power = 1 << (count.bit_length() - 1)
    bmp = bytearray(size)
    struct.pack_into(">7H", bmp, 0, 4, size, 0, count * 2, power * 2,
                     power.bit_length() - 1, (count - power) * 2)
    for index, (start, end, delta, glyphs) in enumerate(segments):
        put16(bmp, 14 + index * 2, end)
        put16(bmp, 16 + (count + index) * 2, start)
        put16(bmp, 16 + (2 * count + index) * 2, delta)
        if glyphs:
            offset = 16 + (3 * count + index) * 2
            put16(bmp, offset, cursor - offset)
            struct.pack_into(f">{len(glyphs)}H", bmp, cursor, *glyphs)
            cursor += len(glyphs) * 2

    groups = []
    for code, glyph in sorted(mapping.items()):
        if groups and code == groups[-1][1] + 1 and glyph == groups[-1][2] + code - groups[-1][0]:
            groups[-1][1] = code
        else:
            groups.append([code, code, glyph])
    full = bytearray(struct.pack(">HHIII", 12, 0, 16 + len(groups) * 12, 0, len(groups)))
    full.extend(b"".join(struct.pack(">III", *group) for group in groups))
    tables = ((0, 3, 36), (0, 4, 36 + len(bmp)), (3, 1, 36), (3, 10, 36 + len(bmp)))
    return struct.pack(">2H", 0, len(tables)) + b"".join(struct.pack(">HHI", *row) for row in tables) + bmp + full


def rename(data, family, *, variable=False):
    replacements = {
        1: family, 2: "Regular", 3: f"{family};QLIE;3", 4: family + " Regular",
        6: family + "-Regular", 16: family, 21: family,
    }
    if not variable:
        # Preserve typographic/WWS subfamily names on variable fonts whose default axis is not
        # Regular (e.g. Thin/ExtraLight). Overwriting ID 17/22 to "Regular" makes GDI emit a
        # colliding elfFullName for the default instance, which poisons Locale Emulator's
        # EnumFontFamiliesEx cache and hides the real Regular (wght=400) face from the game.
        replacements[17] = replacements[22] = "Regular"
    start = u16(data, 4)
    records, values = bytearray(), bytearray()
    for index in range(u16(data, 2)):
        platform, encoding, language, name_id, length, offset = struct.unpack_from(">6H", data, 6 + index * 12)
        value = data[start + offset:start + offset + length]
        if name_id in replacements:
            if platform not in (0, 1, 3):
                continue
            value = replacements[name_id].encode("ascii" if platform == 1 else "utf-16be")
        records.extend(struct.pack(">6H", platform, encoding, language, name_id, len(value), len(values)))
        values.extend(value)
    return struct.pack(">3H", 0, len(records) // 12, 6 + len(records)) + records + values


def normalize_metrics(tables):
    # QLIE's system fonts use MS Gothic/Mincho's cell height and half-em width.
    em = u16(tables[b"head"], 18)
    ascent = (em * 220 + 128) // 256
    descent = em - ascent
    metrics = tables[b"OS/2"]
    for offset, value in [(2, (em + 1) // 2), (68, ascent), (70, -descent), (72, 0), (74, ascent), (76, descent)]:
        put16(metrics, offset, value)
    if len(metrics) >= 86:
        struct.pack_into(">I", metrics, 78, u32(metrics, 78) | (1 << 17))  # Windows Shift-JIS
    for offset, value in [(4, ascent), (6, -descent), (8, 0)]:
        put16(tables[b"hhea"], offset, value)
    tables.pop(b"MVAR", None)


def derive(source, family, aliases):
    if source[:4] != b"\0\1\0\0":
        raise ValueError("QLIE 字体源需要单字体 TrueType SFNT")
    tables = {}
    try:
        for index in range(u16(source, 4)):
            tag, _, offset, size = struct.unpack_from(">4sIII", source, 12 + index * 16)
            if tag == b"DSIG":
                continue
            if offset + size > len(source) or tag in tables:
                raise ValueError("QLIE 字体表范围无效")
            tables[tag] = bytearray(source[offset:offset + size])
        mapping = read_cmap(tables[b"cmap"])
        for slot, code in aliases.items():
            if code not in mapping:
                raise ValueError(f"QLIE 字体缺少字符 U+{code:04X}")
            mapping[slot] = mapping[code]
        tables[b"cmap"] = build_cmap(mapping)
        tables[b"name"] = rename(tables[b"name"], family, variable=b"fvar" in tables)
        normalize_metrics(tables)
        struct.pack_into(">I", tables[b"head"], 8, 0)
    except (KeyError, struct.error) as error:
        raise ValueError("QLIE TrueType 字体缺少完整的名称、映射或度量表") from error

    count = len(tables)
    offset = 12 + count * 16
    power = 1 << (count.bit_length() - 1)
    result = bytearray(offset + sum((len(table) + 3) & ~3 for table in tables.values()))
    struct.pack_into(">4s4H", result, 0, source[:4], count, power * 16, power.bit_length() - 1, (count - power) * 16)
    for index, (tag, table) in enumerate(sorted(tables.items())):
        struct.pack_into(">4sIII", result, 12 + index * 16, tag, checksum(table), offset, len(table))
        result[offset:offset + len(table)] = table
        if tag == b"head":
            head = offset
        offset += (len(table) + 3) & ~3
    struct.pack_into(">I", result, head + 8, (0xb1b0afba - checksum(result)) & 0xffffffff)
    return result
