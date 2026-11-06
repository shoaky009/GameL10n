"""Bounded PE32 editing for the hash-gated Kaguya runtime template."""
import json
import struct


def align(value, step):
    return (value + step - 1) // step * step


def cs_json(value):
    # Match System.Text.Json's default encoder in adapter-owned metadata.
    text = json.dumps(value, ensure_ascii=True, separators=(',', ':'))
    import re
    text = re.sub(r'\\u([0-9a-f]{4})', lambda m: '\\u' + m[1].upper(), text)
    def escape_string(match):
        content = match[0][1:-1].replace('\\"', '\\u0022')
        for char in "&<>'+`":
            content = content.replace(char, '\\u%04X' % ord(char))
        return '"' + content + '"'
    text = re.sub(r'"(?:[^"\\]|\\.)*"', escape_string, text)
    return text.encode('ascii')


class Pe:
    def __init__(self, data):
        self.data = bytearray(data)
        if len(data) < 512 or self.u16(0) != 0x5a4d:
            raise ValueError('不是PE程序。')
        self.pe = self.u32(0x3c)
        self.opt = self.pe + 24
        self.sections = self.opt + self.u16(self.pe + 20)
        if self.u16(self.opt) != 0x10b:
            raise ValueError('仅支持已验证的32位PE结构。')
        self.image_base = self.u32(self.opt + 28)

    def u16(self, at):
        return struct.unpack_from('<H', self.data, at)[0]

    def u32(self, at):
        return struct.unpack_from('<I', self.data, at)[0]

    def w16(self, at, value):
        struct.pack_into('<H', self.data, at, value)

    def w32(self, at, value):
        struct.pack_into('<I', self.data, at, value & 0xffffffff)

    def headers(self):
        return range(self.sections, self.sections + self.u16(self.pe + 6) * 40, 40)

    def section(self, name):
        return next((sh for sh in self.headers() if self.data[sh:sh + 8].rstrip(b'\0') == name.encode('ascii')), None)

    def offset(self, rva):
        if rva < self.u32(self.opt + 60) and rva < len(self.data):
            return rva
        for sh in self.headers():
            start, size = self.u32(sh + 12), self.u32(sh + 16)
            if start <= rva < start + size:
                at = self.u32(sh + 20) + rva - start
                if at < len(self.data):
                    return at
        raise ValueError(f'PE RVA越界：{rva:X}。')

    def rva(self, at):
        for sh in self.headers():
            raw = self.u32(sh + 20)
            if raw <= at < raw + self.u32(sh + 16):
                return self.u32(sh + 12) + at - raw
        return at

    def next_rva(self):
        return align(max(self.u32(sh + 12) + max(self.u32(sh + 8), self.u32(sh + 16)) for sh in self.headers()), self.u32(self.opt + 32))

    def add_section(self, payload, name, flags=0x40000040):
        count = self.u16(self.pe + 6)
        sh = self.sections + count * 40
        first = min(self.u32(h + 20) for h in self.headers() if self.u32(h + 20))
        if len(name) > 8 or sh + 40 > min(first, self.u32(self.opt + 60)):
            raise ValueError('PE节表没有可安全扩展的空间。')
        if self.u32(self.opt + 128):
            raise ValueError('不修改带签名的未知程序。')
        rva = self.next_rva()
        raw = align(len(self.data), self.u32(self.opt + 36))
        size = align(len(payload), self.u32(self.opt + 36))
        self.data.extend(bytes(raw + size - len(self.data)))
        self.data[raw:raw + len(payload)] = payload
        self.data[sh:sh + 40] = bytes(40)
        self.data[sh:sh + len(name)] = name.encode('ascii')
        for off, value in ((8, len(payload)), (12, rva), (16, size), (20, raw), (36, flags)):
            self.w32(sh + off, value)
        self.w16(self.pe + 6, count + 1)
        self.w32(self.opt + 56, align(rva + len(payload), self.u32(self.opt + 32)))
        field = self.opt + (4 if flags & 0x20 else 8)
        self.w32(field, self.u32(field) + size)
        self.w32(self.opt + 64, 0)
        return rva

    def remove_relocations(self, start, length):
        rva, size = self.u32(self.opt + 136), self.u32(self.opt + 140)
        if not rva:
            return
        at = self.offset(rva)
        end = at + size
        while at < end:
            page, size = self.u32(at), self.u32(at + 4)
            if size < 8 or at + size > end:
                raise ValueError('PE重定位表损坏。')
            for pos in range(at + 8, at + size, 2):
                value = self.u16(pos)
                if value >> 12 and start <= page + (value & 0xfff) < start + length:
                    self.w16(pos, value & 0xfff)
            at += size


class Asm:
    def __init__(self):
        self.data = bytearray()
        self.labels, self.refs = {}, []

    def b(self, *values):
        self.data.extend(values)

    def u(self, value):
        self.data.extend(struct.pack('<I', value & 0xffffffff))

    def label(self, name):
        self.labels[name] = len(self.data)

    def jump(self, name, condition=0):
        self.b(0x0f, condition) if condition else self.b(0xe9)
        self.refs.append((len(self.data), name))
        self.u(0)

    def finish(self):
        for at, name in self.refs:
            struct.pack_into('<i', self.data, at, self.labels[name] - at - 4)
        return bytes(self.data)
