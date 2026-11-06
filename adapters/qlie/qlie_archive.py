"""QLIE template archives. Preserve ciphertext, index padding, and lookup tables.

The 3.0 cipher and byte-pair decoder come from the pinned engine library.
The 3.1 cipher follows GARbro (morkt, MIT); see tools/GARbro-LICENSE.txt.
"""
from dataclasses import dataclass, replace
from pathlib import Path
import shutil
import struct

from python.archives.qlie import Entry as LibraryEntry, _add, crypt_name, decrypt, encrypt, hash_v3, decompress

MASK = 0xffffffff


def hash31(data):
    h = k = 0
    for p in range(0, len(data) - 7, 8):
        h = _add(h, 0xA35793A7A35793A7, 16)
        k = _add(k, struct.unpack_from('<Q', data, p)[0] ^ h, 16)
        k = ((k & 0x1fffffff1fffffff) << 3) | ((k >> 29) & 0x700000007)
    def signed(v):
        v &= 65535
        return v - 65536 if v >= 32768 else v
    return (signed(k) * signed(k >> 32) + signed(k >> 16) * signed(k >> 48)) & MASK


def name31(data, arc):
    count = len(data) // 2
    h = ((count * count) ^ count ^ 0x3e13 ^ (arc >> 16) ^ arc) & 65535
    key = h
    result = bytearray(data)
    for i in range(count):
        key = (h + i + 8 * key) & MASK
        result[2*i] ^= key & 255
        result[2*i+1] ^= (key >> 8) & 255
    return bytes(result)


def crypt31(data, name, key, arc, method, encrypting=False):
    if method == 0:
        return data
    if method not in (1, 2):
        raise ValueError('不支持的 3.1 加密方法。')
    h, s, mult, salt, tk = ((0x85f532, 0x33f641, 7, 0x8f32dc, 0x8df21431)
                             if method == 1 else (0x86f7e2, 0x4437f1, 13, 0x56e213, 0x8a77f473))
    # The cipher hashes UTF-16 code units, including surrogate pairs.
    units = struct.unpack('<' + 'H' * (len(name.encode('utf-16le')) // 2), name.encode('utf-16le'))
    for i, c in enumerate(units):
        h = (h + (c << (i & 7))) & MASK
        s ^= h
    n = len(data)
    s = (s + (arc ^ ((mult * (n & 0xffffff) + n + h + (h ^ n ^ salt)) & MASK))) & MASK
    s = ((9 if method == 1 else 13) * (s & 0xffffff)) & MASK
    table = []
    for _ in range(32):
        x = tk * (s ^ tk)
        s = ((x >> 32) + x) & MASK
        table.append(s)
    kd = bytearray(1024)
    if method == 2:
        for i in range(256):
            struct.pack_into('<I', kd, i*4, ((i+7) * (-(i+3) if i % 3 else i+3)) & MASK)
        if key is not None and len(key) >= 128:
            k, step = key[49] % 73 + 128, key[79] % 7 + 7
            for i in range(1024):
                k = (k + step) % len(key)
                kd[i] ^= key[k]
    index = 2 * (table[13] & 15) if method == 1 else (8 * (table[8] & 13)) & 127
    v = table[6] | (table[7] << 32)
    out = bytearray(data)
    for p in range(0, n - 7, 8):
        j = index if method == 1 else 2 * (index & 15)
        x = table[j] | (table[j+1] << 32)
        if method == 2:
            x ^= struct.unpack_from('<Q', kd, 8*index)[0]
        v = _add(v ^ x, x, 32)
        raw = struct.unpack_from('<Q', data, p)[0]
        d = raw if encrypting else raw ^ v
        struct.pack_into('<Q', out, p, raw ^ v)
        v = _add(v, d, 8) ^ d
        v = _add((v & 0x7fffffff7fffffff) << 1, d, 16)
        index = (index + 2) & 31 if method == 1 else (index + 1) & 127
    return bytes(out)


@dataclass
class Entry:
    name: str
    raw_name: bytes
    offset: int
    size: int
    unpacked_size: int
    packed: int
    encryption: int
    checksum: int
    index_position: int
    key: bytes


class Archive:
    def __init__(self, path, key, game_key=None, unicode=False):
        self.path = Path(path)
        self.unicode = unicode
        self.game_key = game_key
        self.length = self.path.stat().st_size
        if self.length < 0x41c:
            raise ValueError('封包过短。')
        self.tail_offset = self.length - 0x41c
        self.tail = self.slice(self.tail_offset, 0x41c)
        version = b'FilePackVer3.1' if unicode else b'FilePackVer3.0'
        if self.tail[-28:-14] != version:
            raise ValueError('不支持的 QLIE 封包版本。')
        self.hash = hash31 if unicode else hash_v3
        self.arc_key = self.hash(self.tail[:256]) & 0xfffffff
        count, self.index_offset = struct.unpack_from('<iq', self.tail, len(self.tail)-12)
        if not 1 <= count <= 1000000 or not 0 <= self.index_offset < self.tail_offset:
            raise ValueError('封包索引范围无效。')
        self.index = self.slice(self.index_offset, self.tail_offset-self.index_offset)
        self.entries = []
        p, need_pack_key = 0, key is not None
        for _ in range(count):
            if p + 2 > len(self.index):
                raise ValueError('封包名称越界。')
            length = struct.unpack_from('<H', self.index, p)[0] * (2 if unicode else 1)
            p += 2
            if not 1 <= length <= (512 if unicode else 256) or p+length+28 > len(self.index):
                raise ValueError('封包索引项无效。')
            raw = self.index[p:p+length]
            p += length
            raw = name31(raw, self.arc_key) if unicode else crypt_name(raw, self.arc_key)
            name = raw.decode('utf-16le' if unicode else 'cp932')
            values = struct.unpack_from('<qIIiiI', self.index, p)
            e = Entry(name, raw, *values, p, key)
            p += 28
            if (e.offset < 0 or e.offset + e.size > self.index_offset or e.unpacked_size > 512 << 20
                    or (unicode and (e.encryption not in (0, 1, 2) or e.packed not in (0, 1)))):
                raise ValueError('封包成员范围或标志无效。')
            if unicode and ('\0' in name or '..' in name.replace('\\', '/').split('/') or Path(name).is_absolute()):
                raise ValueError('封包成员路径无效。')
            self.entries.append(e)
            if need_pack_key and 'pack_keyfile' in name.lower():
                key = self.read(e)
                need_pack_key = False
        if unicode and (p+16 > len(self.index) or self.index[p:p+10] != b'HashVer1.4'):
            raise ValueError('暂不支持此 3.1 哈希索引版本。')

    def slice(self, offset, size):
        with self.path.open('rb') as f:
            f.seek(offset)
            data = f.read(size)
        if len(data) != size:
            raise ValueError('封包数据截断。')
        return data

    def crypt(self, data, e, encrypting=False):
        if self.unicode:
            return crypt31(data, e.name, e.key, self.arc_key, e.encryption, encrypting)
        # The old adapter treats every nonzero encryption flag as the keyed cipher.
        entry = LibraryEntry(e.name, e.raw_name, e.offset, len(data), len(data), 0, e.encryption, 0)
        return (encrypt if encrypting else decrypt)(data, entry, self.arc_key, mode='keyed',
                                                    key_file=e.key, game_key=self.game_key)

    def read(self, e):
        data = self.slice(e.offset, e.size)
        if self.unicode and self.hash(data) != e.checksum:
            raise ValueError('封包密文校验失败：' + e.name)
        data = self.crypt(data, e)
        if e.packed:
            data = decompress(data, expected_size=e.unpacked_size, max_output=512 << 20)
        if (e.packed or self.unicode) and len(data) != e.unpacked_size:
            raise ValueError('成员长度不符：' + e.name)
        return data

    def build(self, target, replacements):
        changes = []
        for name, data in replacements.items():
            matches = [e for e in self.entries if e.name == name]
            if len(matches) != 1:
                raise ValueError('替换成员不唯一：' + name)
            e = matches[0]
            if data != self.read(e):
                if 'pack_keyfile' in e.name.lower():
                    raise ValueError('禁止修改封包密钥。')
                changes.append((e, data))
        if not changes:
            shutil.copyfile(self.path, target)
            return
        index = bytearray(self.index)
        # Append only: bytes of every original member remain at the same offsets.
        with self.path.open('rb') as source, Path(target).open('wb') as out:
            left = self.index_offset
            while left:
                data = source.read(min(left, 1 << 20))
                if not data:
                    raise ValueError('封包数据截断。')
                out.write(data)
                left -= len(data)
            for e, plain in changes:
                data = self.crypt(plain, e, True)
                at = out.tell()
                out.write(data)
                struct.pack_into('<qIIi', index, e.index_position, at, len(data), len(data), 0)
                struct.pack_into('<I', index, e.index_position+24, self.hash(data))
            at = out.tell()
            out.write(index)
            out.write(self.tail)
            out.seek(-8, 2)
            out.write(struct.pack('<q', at))
