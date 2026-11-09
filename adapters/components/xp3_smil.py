"""Bounded SMIL XP3 metadata, parameterized SmileCrypt and preserving rebuilds.

SMIL aliases and SmileCrypt follow GARbro ArcXP3.cs/CryptAlgorithms.cs (MIT).
Copyright (C) 2014-2017 by morkt

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:
The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.
THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
"""
from dataclasses import dataclass
import hashlib
from pathlib import Path
import shutil
import struct
import zlib

if __package__:
    from .xp3_adler import MAX_ENTRIES, MAX_SEGMENTS, Segment, _chunks, _inflate, _read_at, read_index_data
else:
    from xp3_adler import MAX_ENTRIES, MAX_SEGMENTS, Segment, _chunks, _inflate, _read_at, read_index_data

ASCII_LOWER = str.maketrans('ABCDEFGHIJKLMNOPQRSTUVWXYZ', 'abcdefghijklmnopqrstuvwxyz')


class UnsupportedArchive(ValueError):
    """A structurally unsupported resource, distinct from dependency/I/O errors."""


@dataclass(frozen=True)
class SmileFilter:
    key_xor: int
    first_xor: int
    zero_xor: int

    def __post_init__(self):
        for value, maximum in ((self.key_xor, 0xffffffff), (self.first_xor, 255), (self.zero_xor, 255)):
            if type(value) is not int or not 0 <= value <= maximum:
                raise ValueError('invalid SmileCrypt parameter')

    def transform(self, data, checksum, *, offset=0):
        if type(offset) is not int or offset < 0 or not 0 <= checksum <= 0xffffffff:
            raise ValueError('invalid SmileCrypt checksum/offset')
        value = checksum ^ self.key_xor
        key = (value ^ (value >> 8) ^ (value >> 16) ^ (value >> 24)) & 255
        key = key or self.zero_xor
        result = bytearray(data.translate(bytes(byte ^ key for byte in range(256))))
        if result and offset == 0:
            result[0] ^= (value & 255) or self.first_xor
        return bytes(result)


@dataclass(frozen=True)
class Entry:
    name: str
    alias: str
    flags: int
    raw_size: int
    packed_size: int
    adler32: int
    segments: tuple


@dataclass(frozen=True)
class Index:
    entries: tuple
    chunks: tuple
    archive_size: int
    prefix_length: int
    pointer_offset: int
    security_stub_count: int


def _name(value):
    if isinstance(value, bytes):
        value = value.decode('utf-16le')
    normalized = value.replace('\\', '/')
    if (not value or '\0' in value or ':' in value or normalized.startswith('/')
            or any(part in ('', '.', '..') for part in normalized.split('/'))):
        raise UnsupportedArchive('XP3 文件名无效或超出资源目录。')
    return value


def read_index(stream):
    """Resolve SMIL names with strict identities and retain every original chunk."""
    data, size, header_end, index_at, index_end = read_index_data(stream)
    chunks = tuple(_chunks(data))
    if {tag for tag, _ in chunks} != {b'File', b'smil'}:
        raise UnsupportedArchive('不是受支持的 SMIL XP3 索引。')
    mappings, unicode_aliases, by_checksum, mapping_names = {}, {}, {}, set()
    for tag, body in chunks:
        if tag != b'smil':
            continue
        if len(mapping_names) >= MAX_ENTRIES or len(body) < 8:
            raise UnsupportedArchive('SMIL 文件名映射截断或超出数量限制。')
        checksum, units = struct.unpack_from('<IH', body)
        if not 0 < units <= 4096 or len(body) != 8 + 2 * units or body[-2:] != b'\0\0':
            raise UnsupportedArchive('SMIL 文件名映射布局不支持。')
        name = _name(body[6:-2])
        if name in mapping_names:
            raise UnsupportedArchive('SMIL 文件名映射重复。')
        mapping_names.add(name)
        # Native aliases take priority: Unicode folding can merge distinct names.
        alias = hashlib.md5(name.translate(ASCII_LOWER).encode('utf-16le')).hexdigest()
        if alias in mappings and mappings[alias] != (checksum, name):
            raise UnsupportedArchive('SMIL 文件名映射冲突。')
        mappings[alias] = checksum, name
        alternative = hashlib.md5(name.lower().encode('utf-16le')).hexdigest()
        unicode_aliases.setdefault(alternative, set()).add((checksum, name))
        by_checksum.setdefault(checksum, set()).add(name)
    entries, names, aliases, segment_count, stub_count = [], set(), set(), 0, 0
    for tag, body in chunks:
        if tag != b'File':
            continue
        if len(entries) + stub_count >= MAX_ENTRIES:
            raise UnsupportedArchive('XP3 成员数量超出限制。')
        parts = list(_chunks(body))
        fields = dict(parts)
        if (len(fields) != len(parts) or set(fields) - {b'info', b'segm', b'adlr', b'time'}
                or not {b'info', b'segm', b'adlr'} <= set(fields)):
            raise UnsupportedArchive('SMIL XP3 成员字段不支持。')
        info, table = fields[b'info'], fields[b'segm']
        if len(info) < 22 or len(fields[b'adlr']) != 4 or not table or len(table) % 28:
            raise UnsupportedArchive('SMIL XP3 成员元数据截断。')
        flags, raw, packed, units = struct.unpack_from('<IQQH', info)
        end = 22 + 2 * units
        if (flags not in (0, 0x80000000) or not 0 < units <= 4096 or len(info) < end
                or info[end:] not in (b'', b'\0\0')):
            raise UnsupportedArchive('SMIL XP3 文件名布局不支持。')
        alias = info[22:end].decode('utf-16le')
        checksum, = struct.unpack('<I', fields[b'adlr'])
        segments = tuple(Segment(*values) for values in struct.iter_unpack('<IQQQ', table))
        segment_count += len(segments)
        if segment_count > MAX_SEGMENTS:
            raise UnsupportedArchive('XP3 分段数量超出限制。')
        for segment in segments:
            if (segment.flag not in (0, 1) or segment.flag == 0 and segment.raw_size != segment.packed_size
                    or segment.offset < header_end or segment.offset > size - segment.packed_size
                    or segment.packed_size and segment.offset < index_end
                    and segment.offset + segment.packed_size > index_at):
                raise UnsupportedArchive('SMIL XP3 分段范围或长度无效。')
        # This is the known security-stub layout accepted by GARbro's mapped XP3 reader.
        # Its incorrect info lengths are not applied to ordinary archive members.
        if (flags == 0 and raw == packed == 910 and checksum == 0x689792e4 and alias.startswith('$$$')
                and segments == (Segment(0, 88, 157, 157),)
                and _read_at(stream, size, 40, 7) == b'\x89PNG\n\x1a\n'):
            stub_count += 1
            if stub_count > 1:
                raise UnsupportedArchive('SMIL XP3 保护声明条目重复。')
            continue
        name = alias
        if alias in mappings:
            expected, name = mappings[alias]
            if checksum != expected:
                raise UnsupportedArchive('SMIL 文件名映射与成员校验标识不一致。')
        elif alias in unicode_aliases:
            matches = {name for expected, name in unicode_aliases[alias] if expected == checksum}
            if len(matches) != 1:
                raise UnsupportedArchive('SMIL Unicode 别名没有唯一的名称及校验标识。')
            name = next(iter(matches))
        elif alias != 'startup.tjs' and len(by_checksum.get(checksum, ())) == 1:
            name = next(iter(by_checksum[checksum]))
        elif alias != 'startup.tjs':
            raise UnsupportedArchive('SMIL XP3 成员缺少唯一的文件名映射。')
        _name(name)
        identity = name.translate(ASCII_LOWER)
        if identity in names and identity != 'startup.tjs' or alias in aliases:
            raise UnsupportedArchive('SMIL XP3 成员名称重复。')
        if sum(s.raw_size for s in segments) != raw or sum(s.packed_size for s in segments) != packed:
            raise UnsupportedArchive('SMIL XP3 成员与分段长度不一致。')
        names.add(identity)
        aliases.add(alias)
        entries.append(Entry(name, alias, flags, raw, packed, checksum, segments))
    mapped_names = {name.translate(ASCII_LOWER) for _, name in mappings.values()}
    resolved_names = {entry.name.translate(ASCII_LOWER) for entry in entries if entry.alias != 'startup.tjs'}
    if not entries or mapped_names != resolved_names:
        raise UnsupportedArchive('SMIL 文件名映射未完整对应成员。')
    prefix_length = min(s.offset for entry in entries for s in entry.segments)
    if prefix_length > 1 << 20 or stub_count and prefix_length < 245:
        raise UnsupportedArchive('SMIL XP3 前缀布局超出支持范围。')
    return Index(tuple(entries), chunks, size, prefix_length, 32 if header_end == 40 else 11, stub_count)


def decode_member(stream, entry, filter_spec, *, max_size=32 << 20):
    """Return plaintext and whether the parameterized filter was required."""
    if max(entry.raw_size, entry.packed_size) > max_size:
        raise UnsupportedArchive('XP3 文本成员超出处理大小限制。')
    stream.seek(0, 2)
    size = stream.tell()
    parts = []
    for segment in entry.segments:
        data = _read_at(stream, size, segment.offset, segment.packed_size)
        parts.append(_inflate(data, segment.raw_size, max_size) if segment.flag else data)
    data = b''.join(parts)
    if len(data) != entry.raw_size:
        raise UnsupportedArchive('XP3 成员明文长度不符。')
    if zlib.adler32(data) & 0xffffffff == entry.adler32:
        return data, False
    if entry.flags:
        data = filter_spec.transform(data, entry.adler32)
        if zlib.adler32(data) & 0xffffffff == entry.adler32:
            return data, True
    raise UnsupportedArchive('SmileCrypt 参数与成员明文校验不匹配。')


def read_member(stream, entry, filter_spec, *, max_size=32 << 20):
    return decode_member(stream, entry, filter_spec, max_size=max_size)[0]


def _chunk(tag, body):
    return tag + struct.pack('<Q', len(body)) + body


def rebuild(source, destination, index, replacements, filter_spec):
    """Preserve the prefix, aliases, metadata and unchanged stored member bytes."""
    source, destination = Path(source), Path(destination)
    if source.resolve() == destination.resolve():
        raise ValueError('XP3 输出必须独立于原始封包。')
    by_alias = {entry.alias: entry for entry in index.entries}
    if set(replacements) - by_alias.keys():
        raise ValueError('XP3 回填包含未知成员。')
    destination.parent.mkdir(parents=True, exist_ok=True)
    if not replacements:
        shutil.copyfile(source, destination)
        return
    updated, name_checksums = {}, {}
    with source.open('rb') as original, destination.open('w+b') as output:
        output.write(original.read(index.prefix_length))
        for entry in index.entries:
            value = replacements.get(entry.alias)
            checksum = entry.adler32
            if value is not None:
                _, filtered = decode_member(original, entry, filter_spec, max_size=32 << 20)
                if len(value) > 32 << 20:
                    raise ValueError('XP3 回填成员超出大小限制。')
                checksum = zlib.adler32(value) & 0xffffffff
                stored = zlib.compress(filter_spec.transform(value, checksum) if filtered else value)
                segments = (Segment(1, output.tell(), len(value), len(stored)),)
                output.write(stored)
            else:
                segments = []
                for segment in entry.segments:
                    segments.append(Segment(segment.flag, output.tell(), segment.raw_size, segment.packed_size))
                    original.seek(segment.offset)
                    left = segment.packed_size
                    while left:
                        data = original.read(min(left, 1 << 20))
                        if not data:
                            raise ValueError('XP3 原始成员数据截断。')
                        output.write(data)
                        left -= len(data)
            updated[entry.alias] = (segments, checksum)
            if entry.alias != 'startup.tjs':
                name_checksums[entry.name] = checksum
        records = []
        for tag, body in index.chunks:
            if tag == b'smil':
                units, = struct.unpack_from('<H', body, 4)
                name = body[6:6 + units * 2].decode('utf-16le')
                checksum = name_checksums[name]
                records.append(_chunk(tag, struct.pack('<I', checksum) + body[4:]))
                continue
            fields = list(_chunks(body))
            info = dict(fields)[b'info']
            units, = struct.unpack_from('<H', info, 20)
            alias = info[22:22 + units * 2].decode('utf-16le')
            if alias not in updated:
                records.append(_chunk(tag, body))
                continue
            segments, checksum = updated[alias]
            new_info = bytearray(info)
            struct.pack_into('<QQ', new_info, 4, sum(s.raw_size for s in segments), sum(s.packed_size for s in segments))
            values = {b'info': bytes(new_info), b'adlr': struct.pack('<I', checksum),
                      b'segm': b''.join(struct.pack('<IQQQ', s.flag, s.offset, s.raw_size, s.packed_size) for s in segments)}
            records.append(_chunk(tag, b''.join(_chunk(key, values.get(key, value)) for key, value in fields)))
        raw = b''.join(records)
        packed = zlib.compress(raw)
        offset = output.tell()
        output.write(b'\1' + struct.pack('<QQ', len(packed), len(raw)) + packed)
        output.seek(index.pointer_offset)
        output.write(struct.pack('<Q', offset))
