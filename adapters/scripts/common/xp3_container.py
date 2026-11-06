"""Streaming writer over the existing checked XP3 index/filter component."""
import shutil
import struct
import zlib

from xp3_adler import MAGIC, _chunks, apply_key_in_place, generate_key, read_index

ROT = bytes(((value << (value.bit_count() & 7)) | (value >> (8 - (value.bit_count() & 7)))) & 255 for value in range(256))
INV = bytes(ROT.index(value) for value in range(256))


def chunk(tag, data):
    return tag + struct.pack('<Q', len(data)) + data


class Archive:
    def __init__(self, path, variant):
        self.path, self.variant = path, variant
        with path.open('rb') as stream:
            self.index = read_index(stream)
            self.entries = self.index.entries
            stream.seek(11)
            pointer = struct.unpack('<Q', stream.read(8))[0]
            self.prefix_length = 40 if pointer == 0x17 else 19
            if pointer == 0x17:
                stream.seek(32)
                pointer = struct.unpack('<Q', stream.read(8))[0]
            stream.seek(pointer)
            flag = stream.read(1)[0]
            size = struct.unpack('<Q', stream.read(8))[0]
            if flag:
                raw = struct.unpack('<Q', stream.read(8))[0]
                self.raw_index = zlib.decompress(stream.read(size))
                if len(self.raw_index) != raw:
                    raise ValueError('XP3 索引长度不符。')
            else:
                self.raw_index = stream.read(size)

    def read(self, entry):
        if entry.raw_size > 64 << 20:
            raise ValueError('XP3 成员过大: ' + entry.name)
        result = bytearray()
        with self.path.open('rb') as stream:
            for segment in entry.segments:
                stream.seek(segment.offset)
                stored = stream.read(segment.packed_size)
                if len(stored) != segment.packed_size:
                    raise ValueError('XP3 成员段截断。')
                if segment.flag:
                    try:
                        data = zlib.decompress(stored)
                    except zlib.error:
                        if self.variant != 'v2':
                            raise
                        data = stored
                else:
                    data = stored
                if len(data) != segment.raw_size:
                    raise ValueError('XP3 成员段长度不符。')
                result.extend(data)
        if self.variant == 'rot':
            result = result.translate(ROT)
        elif self.variant == 'adler' and entry.protected:
            apply_key_in_place(result, 0, len(result), generate_key(entry.adler32))
        if len(result) != entry.raw_size or entry.adler32 and zlib.adler32(result) & 0xffffffff != entry.adler32:
            raise ValueError('XP3 成员长度或校验失败: ' + entry.name)
        return bytes(result)

    def build(self, destination, replacements):
        destination.parent.mkdir(parents=True, exist_ok=True)
        if not replacements:
            shutil.copyfile(self.path, destination)
            return
        by_name = {entry.name: entry for entry in self.entries}
        if replacements.keys() - by_name.keys():
            raise ValueError('XP3 回填包含未知成员。')
        updated = {}
        with self.path.open('rb') as source, destination.open('w+b') as target:
            prefix = source.read(self.prefix_length) if self.variant == 'v2' else MAGIC + bytes(8)
            target.write(prefix)
            for entry in self.entries:
                segments = []
                value = replacements.get(entry.name)
                checksum = entry.adler32
                if value is not None:
                    checksum = zlib.adler32(value) & 0xffffffff
                    stored = value
                    if self.variant == 'rot':
                        stored = value.translate(INV)
                    elif self.variant == 'adler' and entry.protected:
                        stored = bytearray(value)
                        apply_key_in_place(stored, 0, len(stored), generate_key(checksum))
                    packed = zlib.compress(stored)
                    segments.append((1, target.tell(), len(value), len(packed)))
                    target.write(packed)
                else:
                    for segment in entry.segments:
                        segments.append((segment.flag, target.tell(), segment.raw_size, segment.packed_size))
                        source.seek(segment.offset)
                        left = segment.packed_size
                        while left:
                            data = source.read(min(1 << 20, left))
                            if not data:
                                raise ValueError('XP3 原始成员段截断。')
                            target.write(data)
                            left -= len(data)
                updated[entry.name] = (segments, checksum)
            records = []
            for tag, body in _chunks(self.raw_index):
                if tag != b'File':
                    records.append(chunk(tag, body))
                    continue
                fields = list(_chunks(body))
                info = dict(fields)[b'info']
                units = struct.unpack_from('<H', info, 20)[0]
                name = info[22:22 + units * 2].decode('utf-16le')
                segments, checksum = updated[name]
                newinfo = bytearray(info)
                struct.pack_into('<QQ', newinfo, 4, sum(seg[2] for seg in segments), sum(seg[3] for seg in segments))
                values = {b'info': bytes(newinfo), b'segm': b''.join(struct.pack('<IQQQ', *seg) for seg in segments),
                          b'adlr': struct.pack('<I', checksum)}
                records.append(chunk(tag, b''.join(chunk(key, values.get(key, value)) for key, value in fields)))
            raw = b''.join(records)
            packed = zlib.compress(raw)
            offset = target.tell()
            target.write(b'\1' + struct.pack('<QQ', len(packed), len(raw)) + packed)
            target.seek(32 if self.variant == 'v2' and self.prefix_length == 40 else 11)
            target.write(struct.pack('<Q', offset))
