"""FilePack 3.1 with a HashVer1.4 lookup table and a volume-local key."""
from dataclasses import replace
import struct

from python.archives import qlie

from . import engine_archive as archive


VERSION = "3.1"
TAIL_SIGNATURE = b"HashVer1.4\0\0\0\0\0\0"
FOOTER = struct.Struct("<16sIQ")
MEMBER = struct.Struct("<QIIIII")
KEY_AREA_BYTES = 1024
MAX_INDEX = 2 << 20
MAX_TAIL = 4 << 20
MAX_ENTRIES = 16_000
MASK32 = (1 << 32) - 1


def read_at(stream, offset, length):
    stream.seek(offset)
    data = stream.read(length)
    if len(data) != length:
        raise ValueError("QLIE 封包在记录的边界前结束")
    return data


def add_lanes(left, right, width):
    mask = (1 << width) - 1
    return sum((((left >> shift) + (right >> shift)) & mask) << shift
               for shift in range(0, 64, width))


def checksum(data):
    value = increment = 0
    for (word,) in struct.iter_unpack("<Q", memoryview(data)[:len(data) & ~7]):
        increment = add_lanes(increment, 0xA35793A7A35793A7, 16)
        value = add_lanes(value, word ^ increment, 16)
        low, high = value & MASK32, value >> 32
        value = ((low << 3 | low >> 29) & MASK32) | (((high << 3 | high >> 29) & MASK32) << 32)
    a, b, c, d = struct.unpack("<4h", value.to_bytes(8, "little"))
    return (a * c + b * d) & MASK32


def crypt_name(data, seed):
    length = len(data) // 2
    key = ((seed >> 16) ^ seed ^ length ^ (length * length) ^ 0x3E13) & 0xffff
    word = key
    decoded = bytearray()
    for index, (code_unit,) in enumerate(struct.iter_unpack("<H", data)):
        word = ((word << 3) + index + key) & 0xffff
        decoded.extend(struct.pack("<H", code_unit ^ word))
    return bytes(decoded)


def read_index(stream):
    stream.seek(0, 2)
    size = stream.tell()
    key_offset = size - FOOTER.size - KEY_AREA_BYTES
    if key_offset < 0:
        raise ValueError("FilePack 3.1 尾部不完整")
    signature, count, start = FOOTER.unpack(read_at(stream, size - FOOTER.size, FOOTER.size))
    if signature != b"FilePackVer3.1\0\0" or not 0 < count <= MAX_ENTRIES or not 0 <= start < key_offset:
        raise ValueError("FilePack 3.1 索引边界或成员数量无效")
    seed = checksum(read_at(stream, key_offset, 256)) & 0x0fffffff
    stream.seek(start)
    end = min(start + MAX_INDEX, key_offset)
    entries = []
    for _ in range(count):
        cursor = stream.tell()
        if cursor + 2 > end:
            raise ValueError("FilePack 3.1 索引超出读取范围")
        length, = struct.unpack("<H", read_at(stream, cursor, 2))
        if not 0 < length <= 256 or cursor + 2 + length * 2 + MEMBER.size > end:
            raise ValueError("FilePack 3.1 名称或索引长度无效")
        raw_name = crypt_name(read_at(stream, cursor + 2, length * 2), seed)
        name = raw_name.decode("utf-16-le")
        offset, stored, unpacked, packed, encryption, check = MEMBER.unpack(
            read_at(stream, stream.tell(), MEMBER.size))
        if (offset + stored > start or packed not in (0, 1) or encryption not in (0, 1, 2)
                or not packed and stored != unpacked):
            raise ValueError("FilePack 3.1 成员边界或标志无效")
        entries.append(qlie.Entry(name, raw_name, offset, stored, unpacked, packed, encryption, check))
    ranges = sorted((entry.offset, entry.offset + entry.size) for entry in entries if entry.size)
    if any(first[1] > second[0] for first, second in zip(ranges, ranges[1:])):
        raise ValueError("FilePack 3.1 成员范围重叠")
    return qlie.Index(tuple(entries), seed, size, start, stream.tell())


def writer_tail(stream, index):
    length = index.file_size - index.index_end - FOOTER.size
    if not KEY_AREA_BYTES + 32 <= length <= MAX_TAIL:
        raise ValueError("FilePack 3.1 查找表超出读取范围")
    tail = read_at(stream, index.index_end, length)
    key_size, count, ordinals, packed_size = struct.unpack_from("<IIII", tail, 16)
    if (tail[:16] != TAIL_SIGNATURE or key_size != 256 or count != len(index.entries)
            or ordinals != 4 * count or not 0 < packed_size <= length - KEY_AREA_BYTES - 32):
        raise ValueError("FilePack 3.1 回包需要已确认的 HashVer1.4 查找表")
    return tail


def member_seed(entry, size, archive_seed):
    if entry.encryption == 1:
        first, second, constant, factor, multiplier = 0x85F532, 0x33F641, 0x8F32DC, 9, 0x8DF21431
        length_term = (size & 0xffffff) * 8
    else:
        first, second, constant, factor, multiplier = 0x86F7E2, 0x4437F1, 0x56E213, 13, 0x8A77F473
        length_term = size + (size & 0xffffff) * 13
    for index, (word,) in enumerate(struct.iter_unpack("<H", entry.raw_name)):
        first = (first + (word << (index & 7))) & MASK32
        second ^= first
    value = (((size ^ first ^ constant) + first + length_term) & MASK32) ^ archive_seed
    value = ((value + second) & 0xffffff) * factor
    material = bytearray()
    for _ in range(64):
        product = ((value ^ multiplier) * multiplier) & ((1 << 64) - 1)
        value = ((product >> 32) + product) & MASK32
        material.extend(struct.pack("<I", value))
    return material


def crypt(data, entry, archive_seed, key, *, encrypt=False):
    if not entry.encryption:
        return data
    material = member_seed(entry, len(data), archive_seed)
    state, = struct.unpack_from("<Q", material, 0x18)
    if entry.encryption == 1:
        cursor = (struct.unpack_from("<I", material, 0x34)[0] & 0xf) * 8
    else:
        if key is None:
            raise ValueError("FilePack 3.1 成员需要卷内密钥")
        cursor = (struct.unpack_from("<I", material, 0x20)[0] & 0xd) * 8
    output = bytearray(data)
    for offset in range(0, len(data) - 7, 8):
        if entry.encryption == 1:
            word, = struct.unpack_from("<Q", material, cursor)
            cursor = (cursor + 8) & 0x7f
        else:
            word, = struct.unpack_from("<Q", material, (cursor & 0xf) * 8)
            word ^= struct.unpack_from("<Q", key, cursor * 8)[0]
            cursor = (cursor + 1) & 0x7f
        state = add_lanes(state ^ word, word, 32)
        original, = struct.unpack_from("<Q", data, offset)
        changed = original ^ state
        struct.pack_into("<Q", output, offset, changed)
        plain = original if encrypt else changed
        state = add_lanes(state, plain, 8) ^ plain
        state = (state << 1) & 0xfffffffefffffffe
        state = add_lanes(state, plain, 16)
    return bytes(output)


def read_member(stream, index, entry, key, limit):
    if max(entry.size, entry.unpacked_size) > limit:
        raise ValueError("FilePack 3.1 成员超出读取范围")
    data = read_at(stream, entry.offset, entry.size)
    if checksum(data) != entry.checksum:
        raise ValueError("FilePack 3.1 成员密文校验不匹配")
    plain = crypt(data, entry, index.arc_key, key)
    if entry.packed:
        return qlie.decompress(plain, expected_size=entry.unpacked_size, max_output=limit)
    return plain


def derive_volume_key(data):
    if len(data) < 0x50:
        raise ValueError("FilePack 3.1 卷内密钥长度不足")
    key = bytearray()
    for i in range(256):
        value = (i + 3) * (i + 7)
        key.extend(struct.pack("<i", value if i % 3 == 0 else -value))
    cursor = data[0x31] % 0x49 + 0x80
    stride = data[0x4f] % 7 + 7
    for i in range(len(key)):
        cursor = (cursor + stride) % len(data)
        key[i] ^= data[cursor]
    return bytes(key)


def volume_key(stream, index, limit):
    first = index.entries[0]
    if (first.name != qlie.PACK_KEY_NAME or first.encryption == 2
            or any(entry.name == qlie.PACK_KEY_NAME for entry in index.entries[1:])):
        raise ValueError("FilePack 3.1 需要唯一且可独立读取的首项卷内密钥")
    return derive_volume_key(read_member(stream, index, first, None, limit))


def rebuild(source, destination, index, key, replacements):
    tail = writer_tail(source, index)
    if (source is destination or qlie.PACK_KEY_NAME in replacements
            or replacements.keys() - {entry.name for entry in index.entries}):
        raise ValueError("FilePack 3.1 回包成员或目标无效")
    cursor = 0
    rebuilt = {}
    for entry in sorted(index.entries, key=lambda item: (item.offset, item.size)):
        if entry.offset < cursor:
            raise ValueError("FilePack 3.1 空成员位于另一成员内部")
        archive.copy_range(source, destination, cursor, entry.offset - cursor)
        new_entry = replace(entry, offset=destination.tell())
        if entry.name in replacements:
            plain = replacements[entry.name]
            stored = crypt(plain, entry, index.arc_key, key, encrypt=True)
            new_entry = replace(new_entry, size=len(stored), unpacked_size=len(plain),
                                packed=0, checksum=checksum(stored))
            destination.write(stored)
        else:
            archive.copy_range(source, destination, entry.offset, entry.size)
        rebuilt[entry.name] = new_entry
        cursor = entry.offset + entry.size
    archive.copy_range(source, destination, cursor, index.index_offset - cursor)
    start = destination.tell()
    for old in index.entries:
        entry = rebuilt[old.name]
        destination.write(struct.pack("<H", len(old.raw_name) // 2))
        destination.write(crypt_name(old.raw_name, index.arc_key))
        destination.write(MEMBER.pack(entry.offset, entry.size, entry.unpacked_size, entry.packed,
                                      entry.encryption, entry.checksum))
    destination.write(tail)
    destination.write(FOOTER.pack(b"FilePackVer3.1\0\0", len(index.entries), start))
