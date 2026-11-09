"""Read-only probe for one Adler-derived XP3 filter, using only the stdlib.

Supports standalone XP3, index flags 0/1, the observed 0x17 pointer block,
and raw/zlib segments. Unknown index chunks are reported for preservation;
this module does not rebuild archives or establish script/runtime support.
The filter and layout follow the validated Xp3Filter/Xp3Archive implementation.
"""
from dataclasses import dataclass
import json
import os
from pathlib import Path
import stat
import struct
import sys
import zlib

MAGIC = b"XP3\r\n \n\x1a\x8bg\x01"
KEY_LENGTH = 32
MAX_INDEX = 16 << 20
MAX_MEMBER = 8 << 20
MAX_TOTAL = 24 << 20
MAX_ENTRIES = 100_000
MAX_SEGMENTS = 100_000
MAX_MEMBERS = 6
TEXT_EXTENSIONS = (".ks", ".tjs", ".ma", ".csv")


def generate_key(adler):
    seed = ((adler & 0xFFFFFFFF) ^ 0x2F91DE55) & 0x7FFFFFFF
    repeated = (seed << 31) | seed
    return bytes((repeated >> ((8 * i) % 31)) & 0xFF for i in range(KEY_LENGTH))


def apply_key_in_place(data, offset, count, key, *, member_offset=None):
    """XOR a buffer range with its position in the uncompressed member.

    member_offset is the member position of data[offset]. Omitting it preserves
    the original API, where buffer position zero is also member position zero.
    For independent segment buffers, pass their cumulative uncompressed offset.
    """
    if member_offset is None:
        member_offset = offset
    if (not isinstance(data, bytearray) or len(key) != KEY_LENGTH
            or any(type(n) is not int for n in (offset, count, member_offset))
            or min(offset, count, member_offset) < 0 or offset > len(data) - count):
        raise ValueError("invalid XP3 filter range, phase or key length")
    for index in range(offset, offset + count):
        data[index] ^= key[(member_offset + index - offset) & 31]


@dataclass(frozen=True)
class Segment:
    flag: int
    offset: int
    raw_size: int
    packed_size: int


@dataclass(frozen=True)
class Entry:
    name: str
    flags: int
    raw_size: int
    packed_size: int
    adler32: int
    segments: tuple

    @property
    def protected(self):
        return bool(self.flags & 0x80000000)


@dataclass(frozen=True)
class Index:
    entries: tuple
    unknown_chunks: tuple
    unknown_chunk_count: int
    archive_size: int
    index_size: int


def _read_at(stream, archive_size, offset, count):
    if offset < 0 or count < 0 or offset > archive_size - count:
        raise ValueError("XP3 range outside archive")
    stream.seek(offset)
    data = stream.read(count)
    if len(data) != count:
        raise ValueError("truncated XP3 range")
    return data


def _inflate(data, expected, limit):
    if expected > limit:
        raise ValueError("XP3 decompression budget exceeded")
    decoder = zlib.decompressobj()
    value = decoder.decompress(data, expected + 1)
    if (len(value) != expected or not decoder.eof or decoder.unconsumed_tail
            or decoder.unused_data):
        raise ValueError("XP3 zlib length, truncation or trailing-data mismatch")
    return value


def _chunks(data):
    offset = 0
    while offset < len(data):
        if len(data) - offset < 12:
            raise ValueError("truncated XP3 index chunk")
        tag, size = struct.unpack_from("<4sQ", data, offset)
        offset += 12
        if size > len(data) - offset:
            raise ValueError("XP3 index chunk outside index")
        yield tag, data[offset:offset + size]
        offset += size


def read_index(stream):
    """Read and validate metadata without reading member contents.

    Returned names are archive identifiers, not safe filesystem output paths.
    Unknown chunks must be preserved by any future writer, not discarded.
    """
    stream.seek(0, 2)
    size = stream.tell()
    read = lambda offset, count: _read_at(stream, size, offset, count)
    if read(0, len(MAGIC)) != MAGIC:
        raise ValueError("not a standalone XP3 archive")
    index_at, = struct.unpack("<Q", read(11, 8))
    header_end = 19
    if index_at == 0x17 and size >= 40:
        candidate, = struct.unpack("<Q", read(0x20, 8))
        if 19 <= candidate < size:
            index_at, header_end = candidate, 40
    if not header_end <= index_at < size:
        raise ValueError("XP3 index overlaps header or is outside archive")
    flag = read(index_at, 1)[0]
    if flag == 0:
        raw_size, = struct.unpack("<Q", read(index_at + 1, 8))
        packed_size, prefix = raw_size, 9
    elif flag == 1:
        packed_size, raw_size = struct.unpack("<QQ", read(index_at + 1, 16))
        prefix = 17
    else:
        raise ValueError("unsupported XP3 index flag or index chain")
    if max(packed_size, raw_size) > MAX_INDEX:
        raise ValueError("XP3 index budget exceeded")
    packed = read(index_at + prefix, packed_size)
    data = _inflate(packed, raw_size, MAX_INDEX) if flag else packed
    index_end = index_at + prefix + packed_size
    entries, unknown, names, segment_count, unknown_count = [], [], set(), 0, 0
    for tag, body in _chunks(data):
        if tag != b"File":
            unknown_count += 1
            if len(unknown) < 8:
                unknown.append(dict(scope="index", tag=tag.hex(), bytes=len(body)))
            continue
        if len(entries) >= MAX_ENTRIES:
            raise ValueError("XP3 entry count budget exceeded")
        sections, extra = {}, []
        for section, value in _chunks(body):
            if section not in (b"info", b"segm", b"adlr"):
                unknown_count += 1
                if len(unknown) + len(extra) < 8:
                    extra.append(dict(scope="member", tag=section.hex(), bytes=len(value)))
            elif section in sections:
                raise ValueError("duplicate XP3 standard section")
            else:
                sections[section] = value
        if set(sections) != {b"info", b"segm", b"adlr"}:
            raise ValueError("missing XP3 standard section")
        info = sections[b"info"]
        if len(info) < 22:
            raise ValueError("truncated XP3 info")
        flags, raw, stored, units = struct.unpack_from("<IQQH", info)
        if flags not in (0, 0x80000000):
            raise ValueError("unsupported XP3 info flags")
        name_end = 22 + 2 * units
        if (not 0 < units <= 4096 or len(info) < name_end
                or info[name_end:] not in (b"", b"\0\0")):
            raise ValueError("unsupported XP3 info/name layout")
        name = info[22:name_end].decode("utf-16-le", errors="strict")
        if "\0" in name or name in names:
            raise ValueError("NUL or duplicate XP3 member name")
        names.add(name)
        unknown.extend(item | dict(member=name) for item in extra)
        if len(sections[b"adlr"]) != 4:
            raise ValueError("unsupported XP3 adlr layout")
        adler, = struct.unpack("<I", sections[b"adlr"])
        table = sections[b"segm"]
        if not table or len(table) % 28:
            raise ValueError("invalid XP3 segment table")
        segment_count += len(table) // 28
        if segment_count > MAX_SEGMENTS:
            raise ValueError("XP3 segment count budget exceeded")
        segments, raw_sum, stored_sum = [], 0, 0
        for at in range(0, len(table), 28):
            sf, offset, length, packed_length = struct.unpack_from("<IQQQ", table, at)
            if sf not in (0, 1) or sf == 0 and length != packed_length:
                raise ValueError("unsupported XP3 segment flag/size")
            if offset < header_end or offset > size - packed_length:
                raise ValueError("XP3 segment outside archive")
            if packed_length and offset < index_end and offset + packed_length > index_at:
                raise ValueError("XP3 segment overlaps index")
            raw_sum += length
            stored_sum += packed_length
            if raw_sum > raw or stored_sum > stored:
                raise ValueError("XP3 member/segment lengths disagree")
            segments.append(Segment(sf, offset, length, packed_length))
        if raw_sum != raw or stored_sum != stored:
            raise ValueError("XP3 member/segment lengths disagree")
        entries.append(Entry(name, flags, raw, stored, adler, tuple(segments)))
    if not entries:
        raise ValueError("empty XP3 member index")
    return Index(tuple(entries), tuple(unknown), unknown_count, size, len(data))


def read_member(stream, entry, *, max_size=MAX_MEMBER):
    """Return checked plaintext; segment phase follows cumulative raw length."""
    if max(entry.raw_size, entry.packed_size) > max_size:
        raise ValueError("XP3 member read/decompression budget exceeded")
    stream.seek(0, 2)
    size = stream.tell()
    result = bytearray()
    key = generate_key(entry.adler32) if entry.protected else None
    for segment in entry.segments:
        packed = _read_at(stream, size, segment.offset, segment.packed_size)
        value = bytearray(_inflate(packed, segment.raw_size, max_size)
                          if segment.flag else packed)
        if key is not None:
            apply_key_in_place(value, 0, len(value), key, member_offset=len(result))
        result.extend(value)
    if len(result) != entry.raw_size:
        raise ValueError(f"{entry.name[:240]}: XP3 member plaintext length mismatch")
    if zlib.adler32(result) & 0xFFFFFFFF != entry.adler32:
        raise ValueError(f"{entry.name[:240]}: XP3 plaintext Adler32 mismatch; filter conflicts with this member")
    return bytes(result)


def _report():
    return dict(protocolVersion=1, operation="probe", success=True, status="needs-evidence",
                filterId="kirikiri-xp3-adler-filter", filterOnly=True, runtimeTested=False,
                reason="", counts=dict(entries=0, protectedEntries=0, candidates=0, selected=0,
                                       verifiedProtected=0, verifiedUnprotected=0, skipped=0, missing=0),
                samples=[], unknownIndexChunks=[], unknownIndexChunkCount=0,
                unknownIndexChunksTruncated=False, rebuildRequiresPreservation=None,
                memberBytesRead=0, memberBytesDecoded=0)


def probe_archive(path, members=None, sample_limit=2):
    """Confirm only this filter using at most six members and bounded reads."""
    report = _report()
    try:
        if type(sample_limit) is not int or not 0 <= sample_limit <= 2:
            raise ValueError("sampleLimit must be an integer from 0 to 2")
        if members is not None and (not isinstance(members, list) or len(members) > MAX_MEMBERS
                or any(not isinstance(n, str) or not n or len(n) > 4096 for n in members)
                or len(set(members)) != len(members)):
            raise ValueError("members must contain at most six distinct exact member names")
        if not isinstance(path, (str, Path)) or not str(path):
            raise ValueError("input must be an XP3 file path")
        with open(path, "rb") as stream:
            before = os.fstat(stream.fileno())
            if not stat.S_ISREG(before.st_mode):
                raise ValueError("input must be a regular file")
            index = read_index(stream)
            counts = report["counts"]
            counts.update(entries=len(index.entries),
                          protectedEntries=sum(e.protected for e in index.entries))
            unknown = index.unknown_chunks
            report.update(indexBytes=index.index_size, unknownIndexChunkCount=index.unknown_chunk_count,
                          unknownIndexChunks=[{k: v[:240] if isinstance(v, str) else v
                                               for k, v in item.items()} for item in unknown[:8]],
                          unknownIndexChunksTruncated=index.unknown_chunk_count > len(unknown),
                          rebuildRequiresPreservation=bool(index.unknown_chunk_count))
            if members is None:
                candidates = [e for e in index.entries
                              if e.protected and e.name.lower().endswith(TEXT_EXTENSIONS)]
                eligible = [e for e in candidates if 0 < e.raw_size <= MAX_MEMBER and e.packed_size <= MAX_MEMBER]
                selected = sorted(eligible, key=lambda e: (e.packed_size, e.name))[:MAX_MEMBERS]
            else:
                by_name = {e.name: e for e in index.entries}
                missing = [n for n in members if n not in by_name]
                report["missingMembers"] = [n[:240] for n in missing]
                counts["missing"] = len(missing)
                candidates = selected = [by_name[n] for n in members if n in by_name]
            counts.update(candidates=len(candidates), selected=len(selected))
            skipped = []
            for entry in selected:
                if (not entry.raw_size or max(entry.raw_size, entry.packed_size) > MAX_MEMBER
                        or report["memberBytesRead"] + entry.packed_size > MAX_TOTAL
                        or report["memberBytesDecoded"] + entry.raw_size > MAX_TOTAL):
                    counts["skipped"] += 1
                    skipped.append(dict(name=entry.name[:240], reason="empty member or probe byte budget"))
                    continue
                read_member(stream, entry)
                report["memberBytesRead"] += entry.packed_size
                report["memberBytesDecoded"] += entry.raw_size
                counts["verifiedProtected" if entry.protected else "verifiedUnprotected"] += 1
                if len(report["samples"]) < sample_limit:
                    report["samples"].append(dict(name=entry.name[:240], rawBytes=entry.raw_size,
                        segmentCount=len(entry.segments), protected=entry.protected,
                        adler32=f"{entry.adler32:08x}", checksumVerified=True))
            report["skippedMembers"] = skipped
            after = os.fstat(stream.fileno())
            if (before.st_size, before.st_mtime_ns) != (after.st_size, after.st_mtime_ns):
                raise ValueError("XP3 archive changed during probe")
            if counts["verifiedProtected"] >= 2:
                report.update(status="matched", reason="Adler32 confirmed on at least two distinct protected members; filter only, script semantics and runtime untested")
            else:
                report["reason"] = "need at least two distinct nonempty protected members with matching plaintext Adler32"
    except (OSError, ValueError, TypeError, struct.error, zlib.error) as error:
        report.update(success=False, status="rejected", reason=str(error)[:400])
    return report


def main():
    try:
        body = sys.stdin.read(65537)
        if len(body) > 65536:
            raise ValueError("probe request exceeds 64 KiB")
        request = json.loads(body)
        if not isinstance(request, dict) or request.get("operation") != "probe":
            raise ValueError("operation must be probe")
        result = probe_archive(request.get("input"), request.get("members"), request.get("sampleLimit", 2))
    except (ValueError, TypeError) as error:
        result = _report()
        result.update(success=False, status="rejected", reason=str(error)[:400])
    print(json.dumps(result, ensure_ascii=True, separators=(",", ":")))
    return 1 if result["status"] == "rejected" else 0


if __name__ == "__main__":
    raise SystemExit(main())
