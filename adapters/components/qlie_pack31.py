"""FilePack 3.1 with a HashVer1.4 lookup table and a volume-local key."""
from dataclasses import replace
import struct

from python.archives import qlie

from . import engine_archive as archive


VERSION = "3.1"
TAIL_SIGNATURE = b"HashVer1.4\0\0\0\0\0\0"
PACKKEY_SIGNATURE = b"8hr48uky,8ugi8ewra4g8d5vbf5hb5s6"
FOOTER = struct.Struct("<16sIQ")
MEMBER = struct.Struct("<QIIIII")
PACKHASH = struct.Struct("<16s7I24s")
PACKKEY_PREFIX = struct.Struct("<32sI")
KEY_AREA_BYTES = 1024
HASH_BUCKETS = 256
HASH_CRYPT_KEY = 0x0428
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


def chain_crypt(key, data, *, encrypt=False):
    """Reversible 8-byte chained XOR used by FilePack 3.1 key and hash blocks."""
    output = bytearray(data)
    state_word = (key + len(output)) ^ 0xFEC9753E
    state = (state_word & MASK32) | ((state_word & MASK32) << 32)
    accumulator = 0xA73C5F9D | (0xA73C5F9D << 32)
    increment = 0xCE24F523 | (0xCE24F523 << 32)
    for offset in range(0, len(output) - 7, 8):
        original, = struct.unpack_from("<Q", output, offset)
        accumulator = add_lanes(accumulator, increment, 32) ^ state
        changed = original ^ accumulator
        struct.pack_into("<Q", output, offset, changed)
        state = original if encrypt else changed
    return bytes(output)


def filename_hash(raw_name):
    result = 0
    for index, (code_unit,) in enumerate(struct.iter_unpack("<H", raw_name), 1):
        result = ((code_unit << (index & 7)) + result) & 0x3fffffff
    return result


def hash_bucket(name_hash):
    return ((name_hash & 0xffff) + (name_hash >> 8) + (name_hash >> 16)) % HASH_BUCKETS


def build_hash_data(raw_names):
    """Build a HashVer1.4 256-bucket filename lookup table for UTF-16LE member names."""
    buckets = [[] for _ in range(HASH_BUCKETS)]
    for ordinal, raw_name in enumerate(raw_names):
        name_hash = filename_hash(raw_name)
        buckets[hash_bucket(name_hash)].append((ordinal, raw_name, name_hash))
    buffer = bytearray()
    for items in buckets:
        buffer.extend(struct.pack("<I", len(items)))
        for ordinal, raw_name, name_hash in items:
            buffer.extend(struct.pack("<H", len(raw_name) // 2))
            buffer.extend(raw_name)
            buffer.extend(struct.pack("<QI", ordinal * 4, name_hash))
    buffer.extend(struct.pack(f"<{len(raw_names)}I", *range(len(raw_names))))
    return bytes(buffer)


def build_tail(raw_names, key_area):
    """Build a HashVer1.4 lookup block and PACKKEY block matching the given 1024-byte key area."""
    if len(key_area) != KEY_AREA_BYTES:
        raise ValueError("FilePack 3.1 密钥区需要 1024 字节")
    archive_key = checksum(key_area[:256]) & 0x0fffffff
    hash_data = chain_crypt(HASH_CRYPT_KEY, build_hash_data(raw_names), encrypt=True)
    pack_hash = PACKHASH.pack(
        TAIL_SIGNATURE, HASH_BUCKETS, len(raw_names), 4 * len(raw_names),
        len(hash_data), 0, 4, 0, bytes(24),
    )
    key_signature = chain_crypt(archive_key, PACKKEY_SIGNATURE, encrypt=True)
    pack_key = PACKKEY_PREFIX.pack(key_signature, len(pack_hash) + len(hash_data)) + key_area
    return pack_hash + hash_data + pack_key


def writer_tail(stream, index):
    length = index.file_size - index.index_end - FOOTER.size
    min_tail = PACKHASH.size + PACKKEY_PREFIX.size + KEY_AREA_BYTES
    if not min_tail <= length <= MAX_TAIL:
        raise ValueError("FilePack 3.1 查找表超出读取范围")
    tail = read_at(stream, index.index_end, length)
    (signature, table_size, count, ordinals, data_size, is_compressed,
     safety_data_size, hash_version, _reserved) = PACKHASH.unpack_from(tail, 0)
    key_offset = length - KEY_AREA_BYTES - PACKKEY_PREFIX.size
    key_signature, hash_size = PACKKEY_PREFIX.unpack_from(tail, key_offset)
    if (signature != TAIL_SIGNATURE or table_size != HASH_BUCKETS or count != len(index.entries)
            or ordinals != 4 * count or data_size == 0 or is_compressed not in (0, 1)
            or safety_data_size not in (0, 4) or hash_version != 0
            or hash_size != key_offset or key_offset != PACKHASH.size + data_size
            or chain_crypt(index.arc_key, key_signature, encrypt=False) != PACKKEY_SIGNATURE):
        raise ValueError("FilePack 3.1 回包需要已确认的 HashVer1.4 查找表与密钥块")
    data = chain_crypt(HASH_CRYPT_KEY, tail[PACKHASH.size:PACKHASH.size + data_size], encrypt=False)
    if is_compressed:
        if data[:4] != b"1PC\xff" or len(data) < 12:
            raise ValueError("FilePack 3.1 HashVer1.4 压缩查找表签名无效")
        data = qlie.decompress(data, expected_size=struct.unpack_from("<I", data, 8)[0], max_output=MAX_TAIL)
    pos, refs = 0, []

    def take(size):
        nonlocal pos
        if size < 0 or pos + size > len(data):
            raise ValueError("FilePack 3.1 HashVer1.4 查找表数据截断")
        chunk = data[pos:pos + size]
        pos += size
        return chunk

    for bucket_index in range(table_size):
        bucket_count = int.from_bytes(take(4), "little")
        if len(refs) + bucket_count > count:
            raise ValueError("FilePack 3.1 HashVer1.4 桶条目数超出范围")
        for _ in range(bucket_count):
            char_count = int.from_bytes(take(2), "little")
            if not 0 < char_count <= 256:
                raise ValueError("FilePack 3.1 HashVer1.4 文件名长度无效")
            raw_name = take(char_count * 2)
            offset, name_hash = struct.unpack("<QI", take(12))
            expected_hash = filename_hash(raw_name)
            if name_hash != expected_hash or bucket_index != hash_bucket(expected_hash):
                raise ValueError("FilePack 3.1 HashVer1.4 文件名哈希或桶位置与成员名不一致")
            refs.append((raw_name, offset))
    mapping = struct.unpack("<" + "I" * count, take(ordinals))
    if pos != len(data) or len(refs) != count or sorted(mapping) != list(range(count)):
        raise ValueError("FilePack 3.1 HashVer1.4 序号映射表与成员数量不符")
    seen = set()
    for raw_name, offset in refs:
        if offset % 4 or offset >= ordinals:
            raise ValueError("FilePack 3.1 HashVer1.4 查找偏移超出序号表")
        ordinal = mapping[offset // 4]
        if ordinal in seen or raw_name != index.entries[ordinal].raw_name:
            raise ValueError("FilePack 3.1 HashVer1.4 文件名或序号与索引不一致")
        seen.add(ordinal)
    return tail


def member_seed(entry, size, archive_seed):
    if entry.encryption == 1:
        first, second, constant, factor, multiplier = 0x85F532, 0x33F641, 0x8F32DC, 9, 0x8DF21431
        length_term = size + (size & 0xffffff) * 7
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
