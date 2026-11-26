"""FilePack layouts that can supply a QLIE script adapter."""
from dataclasses import replace
import re
import struct

from python.archives import qlie, qlie_writer

from . import engine_archive as archive
from . import qlie_pack31


KEY_BYTES = 1 << 20
MAX_INDEX = 2 << 20
MAX_TAIL = 4 << 20
MAX_INDEX_REGION = MAX_INDEX + MAX_TAIL + 28
FOOTER_VERSION = re.compile(rb"FilePackVer([0-9]\.[0-9])\0\0")
HASH13_SIGNATURE = b"HashVer1.3\0\0\0\0\0\0"
HASH14_SIGNATURE = qlie_pack31.TAIL_SIGNATURE


def version(stream):
    size = stream.seek(0, 2)
    if size is None:
        size = stream.tell()
    if size < 28:
        return None
    signature = qlie_pack31.read_at(stream, size - 28, 16)
    match = FOOTER_VERSION.fullmatch(signature)
    return match[1].decode("ascii") if match else None


def hash14_tail30(stream, index, *, max_bytes=MAX_TAIL):
    """Validate a FilePack 3.0 HashVer1.4 filename lookup table and PACKKEY block."""
    size = index.file_size - 28 - index.index_end
    min_tail = qlie_pack31.PACKHASH.size + qlie_pack31.PACKKEY_PREFIX.size + qlie_pack31.KEY_AREA_BYTES
    if not min_tail <= size <= max_bytes:
        raise ValueError("FilePack 3.0 HashVer1.4 查找表超出读取范围")
    tail = qlie_pack31.read_at(stream, index.index_end, size)
    (signature, buckets, count, mapping_size, stored, is_compressed,
     safety_data_size, hash_version, _reserved) = qlie_pack31.PACKHASH.unpack_from(tail, 0)
    key_offset = size - qlie_pack31.KEY_AREA_BYTES - qlie_pack31.PACKKEY_PREFIX.size
    key_signature, hash_size = qlie_pack31.PACKKEY_PREFIX.unpack_from(tail, key_offset)
    if (signature != HASH14_SIGNATURE or buckets != qlie_pack31.HASH_BUCKETS
            or count != len(index.entries) or count > 65535 or mapping_size != count * 2
            or is_compressed not in (0, 1) or safety_data_size not in (0, 2) or hash_version != 0
            or key_offset != qlie_pack31.PACKHASH.size + stored or hash_size != key_offset
            or qlie_pack31.chain_crypt(index.arc_key, key_signature, encrypt=False) != qlie_pack31.PACKKEY_SIGNATURE):
        raise ValueError("FilePack 3.0 HashVer1.4 查找表头或密钥签名无效")
    payload = tail[qlie_pack31.PACKHASH.size:qlie_pack31.PACKHASH.size + stored]
    data = qlie_pack31.chain_crypt(qlie_pack31.HASH_CRYPT_KEY, payload, encrypt=False)
    if is_compressed:
        if data[:4] != b"1PC\xff" or len(data) < 12:
            raise ValueError("FilePack 3.0 HashVer1.4 压缩查找表签名无效")
        data = qlie.decompress(data, expected_size=struct.unpack_from("<I", data, 8)[0], max_output=max_bytes)
    pos, refs = 0, []

    def take(length):
        nonlocal pos
        if length < 0 or pos + length > len(data):
            raise ValueError("FilePack 3.0 HashVer1.4 查找表数据截断")
        chunk = data[pos:pos + length]
        pos += length
        return chunk

    for _ in range(buckets):
        bucket_count = int.from_bytes(take(2), "little")
        if len(refs) + bucket_count > count:
            raise ValueError("FilePack 3.0 HashVer1.4 桶条目数超出范围")
        for _ in range(bucket_count):
            name_length = int.from_bytes(take(2), "little")
            if not 0 < name_length <= 256:
                raise ValueError("FilePack 3.0 HashVer1.4 文件名长度无效")
            name = take(name_length)
            offset, _name_hash = struct.unpack("<QI", take(12))
            refs.append((name, offset))
    mapping = struct.unpack("<" + "H" * count, take(mapping_size))
    if pos != len(data) or len(refs) != count or sorted(mapping) != list(range(count)):
        raise ValueError("FilePack 3.0 HashVer1.4 序号映射表与成员数量不符")
    seen = set()
    for name, offset in refs:
        if offset % 2 or offset >= mapping_size:
            raise ValueError("FilePack 3.0 HashVer1.4 查找偏移超出序号表")
        ordinal = mapping[offset // 2]
        if ordinal in seen or name != index.entries[ordinal].raw_name:
            raise ValueError("FilePack 3.0 HashVer1.4 文件名或序号与索引不一致")
        seen.add(ordinal)
    return tail


def rebuild_pack30_hash14(source, destination, replacements, *, external_key, game_key,
                          max_replacement, max_total=64 << 20):
    """Rebuild a FilePack 3.0 + HashVer1.4 volume while preserving its verified lookup tail."""
    if source is destination:
        raise ValueError("FilePack 3.0 不能原地回包")
    destination.seek(0, 2)
    if destination.tell() != 0:
        raise ValueError("FilePack 3.0 回包目标必须为空文件")
    index = qlie.read_index(source, max_entries=16_000, max_index_bytes=MAX_INDEX)
    tail = hash14_tail30(source, index, max_bytes=MAX_TAIL)
    by_name = {entry.name: entry for entry in index.entries}
    if len(by_name) != len(index.entries) or not isinstance(replacements, dict) or set(replacements) - set(by_name):
        raise ValueError("FilePack 3.0 回包成员无效或存在重复")
    if qlie.PACK_KEY_NAME in replacements:
        raise ValueError("不支持修改 FilePack 3.0 卷内密钥")
    if any(not isinstance(value, bytes) or len(value) > max_replacement for value in replacements.values()):
        raise ValueError("FilePack 3.0 单个替换成员超出大小预算")
    if sum(map(len, replacements.values())) > max_total:
        raise ValueError("FilePack 3.0 替换成员总大小超出预算")
    keys = {}
    active_key = external_key
    for entry in index.entries:
        keys[entry.name] = active_key
        if entry.name == qlie.PACK_KEY_NAME:
            active_key = qlie.read_member(source, index, entry, mode="keyed", key_file=active_key,
                                          game_key=game_key, max_stored=KEY_BYTES, max_output=KEY_BYTES)
    updated = {}
    cursor = 0
    for entry in sorted(index.entries, key=lambda item: (item.offset, item.size)):
        if entry.offset < cursor:
            raise ValueError("FilePack 3.0 成员物理布局重叠")
        archive.copy_range(source, destination, cursor, entry.offset - cursor)
        offset = destination.tell()
        if entry.name in replacements:
            data = replacements[entry.name]
            old = qlie.read_member(source, index, entry, mode="keyed", key_file=keys[entry.name],
                                   game_key=game_key, max_stored=max_replacement, max_output=max_replacement)
            if old == data:
                stored = qlie_pack31.read_at(source, entry.offset, entry.size)
                plain = qlie.decrypt(stored, entry, index.arc_key, mode="keyed",
                                     key_file=keys[entry.name], game_key=game_key)
                encoded = qlie.encrypt(plain, entry, index.arc_key, mode="keyed",
                                       key_file=keys[entry.name], game_key=game_key)
                if encoded != stored:
                    raise ValueError("FilePack 3.0 原样加密往返字节不一致")
                new_entry = replace(entry, offset=offset)
            else:
                new_entry = replace(entry, offset=offset, size=len(data), unpacked_size=len(data), packed=0)
                encoded = qlie.encrypt(data, new_entry, index.arc_key, mode="keyed",
                                       key_file=keys[entry.name], game_key=game_key)
                new_entry = replace(new_entry, checksum=qlie.hash_v3(encoded))
            destination.write(encoded)
            updated[entry.name] = new_entry
        else:
            archive.copy_range(source, destination, entry.offset, entry.size)
            updated[entry.name] = replace(entry, offset=offset)
        cursor = entry.offset + entry.size
    archive.copy_range(source, destination, cursor, index.index_offset - cursor)
    new_index_offset = destination.tell()
    for entry in index.entries:
        rebuilt = updated[entry.name]
        destination.write(struct.pack("<H", len(rebuilt.raw_name)) + qlie.crypt_name(rebuilt.raw_name, index.arc_key))
        destination.write(struct.pack("<QIIIII", rebuilt.offset, rebuilt.size, rebuilt.unpacked_size,
                                      rebuilt.packed, rebuilt.encryption, rebuilt.checksum))
    destination.write(tail)
    destination.write(b"FilePackVer3.0\0\0" + struct.pack("<IQ", len(index.entries), new_index_offset))
    destination.flush()
    check = qlie.read_index(destination, max_entries=16_000, max_index_bytes=MAX_INDEX)
    if hash14_tail30(destination, check, max_bytes=MAX_TAIL) != tail:
        raise ValueError("FilePack 3.0 HashVer1.4 查找表在回包后发生变化")
    for entry in check.entries:
        if entry.name in replacements:
            actual = qlie.read_member(destination, check, entry, mode="keyed", key_file=keys[entry.name],
                                      game_key=game_key, max_stored=max_replacement, max_output=max_replacement)
            if actual != replacements[entry.name]:
                raise ValueError(f"FilePack 3.0 回包成员回读不一致：{entry.name}")


class Pack30:
    version = "3.0"
    template = "qlie-pack3-imos"
    key_source = "external-and-executable"
    unicode_scripts = False

    def index(self, stream):
        return qlie.read_index(stream, max_entries=16_000, max_index_bytes=MAX_INDEX)

    def tail(self, stream, index):
        if index.file_size - 28 - index.index_end >= 16:
            signature = qlie_pack31.read_at(stream, index.index_end, 16)
            if signature == HASH14_SIGNATURE:
                return hash14_tail30(stream, index, max_bytes=MAX_TAIL)
        return qlie_writer.hash13_tail(stream, index, max_bytes=MAX_TAIL)

    def key(self, stream, index, external_key, game_key):
        first = index.entries[0]
        if first.name != qlie.PACK_KEY_NAME or any(e.name == qlie.PACK_KEY_NAME for e in index.entries[1:]):
            raise ValueError("FilePack 3.0 需要唯一的首项卷内密钥")
        return self.read(stream, index, first, external_key, game_key, KEY_BYTES)

    def read(self, stream, index, entry, key, game_key, limit):
        return qlie.read_member(stream, index, entry, mode="keyed", key_file=key,
                                game_key=game_key, max_stored=limit, max_output=limit)

    def rebuild(self, source, output, index, key, external_key, game_key, replacements):
        limit = 16 << 20 if any(k.lower().endswith(".of") for k in replacements) else 8 << 20
        if index.file_size - 28 - index.index_end >= 16 and (
            qlie_pack31.read_at(source, index.index_end, 16) == HASH14_SIGNATURE
        ):
            rebuild_pack30_hash14(source, output, replacements, external_key=external_key,
                                  game_key=game_key, max_replacement=limit)
            return
        qlie_writer.rebuild(source, output, replacements, mode="keyed",
                            key_file=external_key, game_key=game_key, max_replacement=limit)


class Pack31:
    version = qlie_pack31.VERSION
    template = "qlie-pack31-imos"
    key_source = "embedded"
    unicode_scripts = True

    def index(self, stream):
        return qlie_pack31.read_index(stream)

    def tail(self, stream, index):
        return qlie_pack31.writer_tail(stream, index)

    def key(self, stream, index, external_key, game_key):
        return qlie_pack31.volume_key(stream, index, KEY_BYTES)

    def read(self, stream, index, entry, key, game_key, limit):
        return qlie_pack31.read_member(stream, index, entry, key, limit)

    def rebuild(self, source, output, index, key, external_key, game_key, replacements):
        qlie_pack31.rebuild(source, output, index, key, replacements)


LAYOUTS = {layout.version: layout for layout in (Pack30(), Pack31())}


def read_index(stream, layout):
    index = layout.index(stream)
    if index.file_size - index.index_offset > MAX_INDEX_REGION:
        raise ValueError("QLIE 封包索引超出读取范围")
    archive.validate_names(index.entries)
    return index
