"""YPF 474 with 32-bit offsets, FF name XOR, CRC32 names and Adler32 stored members."""
import struct
import zlib

from python.archives import yuris


NAME_LENGTHS = dict(zip(yuris.SWAP_TABLE_00[::2], yuris.SWAP_TABLE_00[1::2]))
NAME_LENGTHS.update(zip(yuris.SWAP_TABLE_00[1::2], yuris.SWAP_TABLE_00[::2]))


def read_index(stream, *, max_entries, max_index_size, record_offsets=None):
    size = stream.seek(0, 2)
    stream.seek(0)
    header = stream.read(32)
    if len(header) != 32 or not header.startswith(b'YPF\0\xda\x01\0\0'):
        raise ValueError("expected a YPF 474 archive")
    count, end = struct.unpack_from("<II", header, 8)
    if not 0 < count <= max_entries or not 32 + count * 24 <= end <= min(size, max_index_size):
        raise ValueError("invalid YPF 474 directory size")
    table = stream.read(end - 32)
    entries = []
    pos = 0
    for ordinal in range(count):
        if pos + 5 > len(table):
            raise ValueError("truncated YPF 474 directory")
        name_hash, length = struct.unpack_from("<IB", table, pos)
        length ^= 255
        length = NAME_LENGTHS.get(length, length)
        pos += 5
        if not length or pos + length + 18 > len(table):
            raise ValueError("invalid YPF 474 record length")
        raw_name = bytes(b ^ 255 for b in table[pos:pos + length])
        if zlib.crc32(raw_name) != name_hash:
            raise ValueError("YPF 474 filename CRC32 mismatch")
        pos += length
        kind, packed, plain, stored, offset, checksum = struct.unpack_from("<BBIIII", table, pos)
        if packed not in (0, 1) or (not packed and stored != plain) or not end <= offset <= size - stored:
            raise ValueError("invalid YPF 474 member range or packing flag")
        if record_offsets is not None:
            record_offsets.append(32 + pos + 2)
        entries.append(yuris.IndexEntry(ordinal, raw_name.decode("cp932"), offset, stored, plain,
                                        bool(packed), kind, name_hash, checksum, b""))
        pos += 18
    if pos != end - 32:
        raise ValueError("unrecognized YPF 474 directory layout")
    previous_end = end
    for entry in sorted(entries, key=lambda e: (e.offset, e.size)):
        if entry.offset < previous_end:
            raise ValueError("overlapping YPF 474 members")
        previous_end = entry.offset + entry.size
    return yuris.ArchiveIndex(474, size, end, end, tuple(entries))


def read_member(stream, index, entry, maximum):
    stored = yuris.read_member(stream, index, entry, max_stored_size=maximum)
    return yuris.decode_member(entry, stored, verify_checksum=True, max_output_size=maximum)


def patch_record(header, record, offset, entry, replacement):
    struct.pack_into("<I", header, record + 8, offset)
    if replacement is None:
        return None
    stored = zlib.compress(replacement) if entry.packed else replacement
    struct.pack_into("<II", header, record, len(replacement), len(stored))
    struct.pack_into("<I", header, record + 12, zlib.adler32(stored))
    return stored
