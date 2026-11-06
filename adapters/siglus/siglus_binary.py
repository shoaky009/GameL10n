"""Byte-preserving edits around the pinned SSU reader. No script recompilation."""
import bisect
import struct

from siglus_ssu import common, const, pck, textmap


def package_header(data):
    if len(data) < const.PACK_HDR_SIZE:
        raise ValueError('Scene.pck 文件头不完整。')
    h = common.parse_i32_header(data, const.PACK_HDR_FIELDS, const.PACK_HDR_SIZE)
    if h['header_size'] != const.PACK_HDR_SIZE:
        raise ValueError('尚不支持此 Scene.pck 文件头版本。')
    for index, count in [('scn_data_index_list_ofs', 'scn_data_cnt'),
                         ('scn_name_index_list_ofs', 'scn_name_index_cnt')]:
        # Some package revisions name the two counts differently; use the data
        # count for names only after the pinned reader has checked its header.
        if index not in h or count not in h:
            continue
        if not (0 < h[count] <= 100000 and const.PACK_HDR_SIZE <= h[index] <= len(data) - h[count] * 8):
            raise ValueError('Scene.pck 索引超出文件范围。')
    return h


def replace_strings(blob, replacements):
    if not replacements:
        return blob
    if len(blob) < 132:
        raise ValueError('场景文件头不完整。')
    h = list(struct.unpack_from('<33I', blob))
    if h[0] != 132 or not (132 <= h[3] <= h[3] + h[4] * 8 <= h[5] <= h[1] <= len(blob)):
        raise ValueError('尚不支持此场景区段布局。')
    strings, _ = textmap._parse_scn_dat(blob)
    pairs = [struct.unpack_from('<II', blob, h[3] + i * 8) for i in range(h[4])]
    changes = []
    for i, replacement in replacements.items():
        if not isinstance(i, int) or not 0 <= i < len(pairs):
            raise ValueError('字符串索引无效。')
        offset, size = pairs[i]
        start, end = h[5] + offset * 2, h[5] + (offset + size) * 2
        if not (h[5] <= start < end <= h[1]):
            raise ValueError('待回填字符串为空或超出文本区段。')
        key = (i * 0x7087) & 65535
        def encode(value):
            raw = value.encode('utf-16le', 'strict')
            return bytes(v ^ ((key >> (8 * (j % 2))) & 255) for j, v in enumerate(raw))
        if encode(strings[i]) != blob[start:end]:
            raise ValueError('字符串编码与当前适配规则不符。')
        changes.append((start, end, encode(replacement), i))
    changes.sort()
    chunks, cursor, ends, deltas = [], 0, [], [0]
    for start, end, encoded, _ in changes:
        if start < cursor:
            raise ValueError('字符串区间重叠，不能安全回填。')
        chunks.extend((blob[cursor:start], encoded))
        cursor = end
        ends.append(end)
        deltas.append(deltas[-1] + len(encoded) - (end - start))
    chunks.append(blob[cursor:])
    output = bytearray(b''.join(chunks))
    starts = [x[0] for x in changes]
    for i, (offset, size) in enumerate(pairs):
        start, end = h[5] + offset * 2, h[5] + (offset + size) * 2
        if not h[5] <= start <= end <= h[1]:
            raise ValueError('字符串区间越界。')
        pos = bisect.bisect_right(ends, start)
        if i not in replacements and size and pos < len(starts) and starts[pos] < end:
            raise ValueError('共享字符串区间不能单独修改。')
        new_size = len(replacements[i].encode('utf-16le')) // 2 if i in replacements else size
        struct.pack_into('<II', output, h[3] + i * 8, offset + deltas[pos] // 2, new_size)
    delta = len(output) - len(blob)
    for field in range(1, 33, 2):
        if h[field] >= h[1]:
            struct.pack_into('<I', output, field * 4, h[field] + delta)
    if output[h[1] + delta:] != blob[h[1]:]:
        raise ValueError('回填意外修改了字节码或非文本区段。')
    expected = list(strings)
    for i, text in replacements.items():
        expected[i] = text
    if textmap._parse_scn_dat(output)[0] != expected:
        raise ValueError('回填后的文本读取校验失败。')
    return bytes(output)


def replace_members(package, members):
    if not members:
        return package
    h = package_header(package)
    index, base, count = (h[k] for k in ('scn_data_index_list_ofs', 'scn_data_list_ofs', 'scn_data_cnt'))
    pairs = [struct.unpack_from('<II', package, index + i * 8) for i in range(count)]
    if not set(members).issubset(range(count)) or index + count * 8 > base:
        raise ValueError('场景索引无效。')
    out, cursor = bytearray(package[:base]), base
    for i, (pos, length) in sorted(enumerate(pairs), key=lambda x: x[1][0]):
        start, end = base + pos, base + pos + length
        if not cursor <= start <= end <= len(package):
            raise ValueError('场景区段重叠或越界。')
        out.extend(package[cursor:start])
        encoded = members.get(i, package[start:end])
        struct.pack_into('<II', out, index + i * 8, len(out) - base, len(encoded))
        out.extend(encoded)
        cursor = end
    out.extend(package[cursor:])
    return bytes(out)


def read_sources(package):
    result = {}
    for item in pck._iter_pck_original_source_items(package):
        if len(item['raw']) > 4 * 1024 * 1024:
            raise ValueError('内嵌源码异常过大。')
        result[item['name']] = common.decode_text_auto(item['raw'])[0]
    return result
