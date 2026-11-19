"""CRC64-indexed Whale DAT v2 archives."""

from dataclasses import dataclass
import struct
import zlib


COUNT_MASK = 0x26ACA46E
MAX_ARCHIVE = 0xFFFFFFFF
INDEX_ENTRY = struct.Struct("<QBIII")
SCRIPT = 2
MASK64 = (1 << 64) - 1


def crc_table():
    table = []
    for byte in range(256):
        value = byte << 56
        for _ in range(8):
            high_bit = value >> 63
            value = (value << 1) & MASK64
            if high_bit:
                value ^= 0x42F0E1EBA9EA3693
        table.append(value)
    return tuple(table)


CRC_TABLE = crc_table()


def crc64(data):
    value = MASK64
    for byte in data:
        value = CRC_TABLE[(value >> 56) ^ byte] ^ ((value << 8) & MASK64)
    return value ^ MASK64


@dataclass(frozen=True)
class Entry:
    key: int
    kind: int
    name: str
    raw_name: bytes
    offset: int
    size: int
    decoded_size: int


@dataclass(frozen=True)
class Index:
    entries: tuple
    missing: tuple
    data_start: int
    length: int


def read_index(stream, file_names, max_entries):
    length = stream.seek(0, 2)
    stream.seek(0)
    header = stream.read(4)
    if len(header) != 4 or length > MAX_ARCHIVE:
        raise ValueError("invalid Whale DAT file length")
    count = int.from_bytes(header, "little") ^ COUNT_MASK
    index_size = count * INDEX_ENTRY.size
    data_start = 4 + index_size
    if not 1 <= count <= max_entries or data_start > length:
        raise ValueError("Whale DAT index exceeds the file or entry budget")
    raw_index = stream.read(index_size)
    if len(raw_index) != index_size:
        raise ValueError("truncated Whale DAT index")

    candidates = {}
    for name in file_names:
        for variant in {name, name.replace("\\", "/"), name.replace("/", "\\")}:
            try:
                raw = variant.encode("cp932")
            except UnicodeEncodeError:
                continue
            key = crc64(raw)
            candidate = (name, raw)
            previous = candidates.setdefault(key, candidate)
            if previous[1] != raw:
                raise ValueError("Whale DAT filename candidates have the same hash")

    entries, missing, seen = [], [], set()
    for key, kind, offset, size, decoded_size in INDEX_ENTRY.iter_unpack(raw_index):
        low_word = key & 0xFFFFFFFF
        kind ^= low_word & 0xFF
        offset, size, decoded_size = offset ^ low_word, size ^ low_word, decoded_size ^ low_word
        if kind not in (0, 1, SCRIPT) or key in seen:
            raise ValueError("invalid Whale DAT member type or duplicate hash")
        seen.add(key)
        if kind == SCRIPT:
            name, raw = candidates.get(key, (f"{key:016x}.script", b""))
        elif key not in candidates:
            missing.append(key)
            continue
        else:
            name, raw = candidates[key]
            offset ^= raw[len(raw) // 2]
            size ^= raw[len(raw) // 4]
            decoded_size ^= raw[len(raw) // 8]
        if (offset < data_start or offset + size > length or kind == SCRIPT and size < 8
                or kind != SCRIPT and size != decoded_size):
            raise ValueError(f"invalid Whale DAT member range for {name}")
        entries.append(Entry(key, kind, name, raw, offset, size, decoded_size))

    boundary = data_start
    for entry in sorted(entries, key=lambda entry: (entry.offset, entry.size)):
        if entry.offset < boundary:
            raise ValueError("overlapping Whale DAT member ranges")
        boundary = entry.offset + entry.size
    return Index(tuple(entries), tuple(missing), data_start, length)


def title_key(title):
    if not isinstance(title, str) or not title.strip() or len(title) > 256 or "\0" in title:
        raise ValueError("Whale scripts require the exact gameTitle")
    return crc64(title.encode("cp932")) & 0xFFFFFFFF


def transform_script(data, key):
    boundary = len(data) - len(data) % 4
    mask = key.to_bytes(4, "little")
    blocks = []
    for start in range(0, boundary, 1 << 20):
        part = data[start:min(start + (1 << 20), boundary)]
        words = int.from_bytes(part, "little")
        pattern = int.from_bytes(mask * (len(part) // 4), "little")
        blocks.append((words ^ pattern).to_bytes(len(part), "little"))
    blocks.append(data[boundary:])
    return b"".join(blocks)


def read_member(stream, entry, maximum, game_key):
    if max(entry.size, entry.decoded_size) > maximum:
        raise ValueError("Whale DAT member exceeds the read budget")
    stream.seek(entry.offset)
    data = stream.read(entry.size)
    if len(data) != entry.size:
        raise ValueError("truncated Whale DAT member")
    if entry.kind == SCRIPT:
        if game_key is None:
            raise ValueError("Whale script extraction requires gameTitle")
        encoded = transform_script(data, (entry.key & 0xFFFFFFFF) ^ game_key)
        decoder = zlib.decompressobj()
        result = decoder.decompress(encoded, entry.decoded_size + 1)
        if (len(result) != entry.decoded_size or not decoder.eof
                or decoder.unused_data or decoder.unconsumed_tail):
            raise ValueError("Whale script decoded length or stream boundary does not match")
        return result
    if entry.kind == 1:
        length = len(data) // len(entry.raw_name)
        result = bytearray(data)
        position = 0
        for mask in entry.raw_name[:-1]:
            end = position + length
            result[position:end] = result[position:end].translate(bytes(value ^ mask for value in range(256)))
            position = end
        return bytes(result)
    return data


def encode_script(data, entry, game_key):
    return transform_script(zlib.compress(data), (entry.key & 0xFFFFFFFF) ^ game_key)


def write_script_index(stream, entries):
    stream.seek(0)
    stream.write(struct.pack("<I", len(entries) ^ COUNT_MASK))
    for entry in entries:
        if entry.kind != SCRIPT:
            raise ValueError("Whale DAT replacement requires a script-only archive")
        mask = entry.key & 0xFFFFFFFF
        stream.write(INDEX_ENTRY.pack(entry.key, entry.kind ^ (mask & 0xFF),
                                      entry.offset ^ mask, entry.size ^ mask, entry.decoded_size ^ mask))
