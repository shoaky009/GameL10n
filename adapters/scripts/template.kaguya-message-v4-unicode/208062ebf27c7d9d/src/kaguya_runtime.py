"""Hash-gated Graphics CP932 to UTF16 lookup and font/lead-byte patch."""
import hashlib
import json
import struct
from pe import Pe, cs_json

ORIGINAL_HASH = 'E1292E3D47D1C65F4AE52A30B1159A476137422BD7A9272A5A2E0129F3CB0684'
MAGIC = b'KAGUYA-UTF16-MAP1\0'
CODE, TABLE, TOTAL = 0x8000, 0x10000, 0x50000
# The original .NET CP932 decoder rejects NEC duplicate byte sequences and
# uses the corresponding IBM/JIS mappings. Python accepts both aliases.
CP932_DUPLICATES = {0x4afa, 0x4bfa, 0x4cfa, 0x4dfa, 0x4efa, 0x4ffa,
                    0x50fa, 0x51fa, 0x52fa, 0x53fa, 0x54fa, 0x58fa,
                    0x59fa, 0x5afa, 0x5bfa, 0x9087, 0x9187, 0x9287,
                    0x9587, 0x9687, 0x9787, 0x9a87, 0x9b87, 0x9c87}


def read_map(source):
    pe = Pe(source)
    sh = pe.section('.kguw')
    if sh is None:
        return {}
    raw = pe.u32(sh + 20)
    if pe.data[raw:raw + len(MAGIC)] != MAGIC:
        raise ValueError('Unicode运行库标记损坏。')
    size = pe.u32(raw + 32)
    if not 2 <= size <= CODE - 64:
        raise ValueError('Unicode映射长度不正确。')
    return {int(k): v for k, v in json.loads(pe.data[raw + 64:raw + 64 + size]).items()}


def payload(rva, mapping):
    data = bytearray(TOTAL)
    data[:len(MAGIC)] = MAGIC
    metadata = cs_json(mapping)
    if len(metadata) >= CODE - 64:
        raise ValueError('映射表超过已验证容量。')
    struct.pack_into('<I', data, 32, len(metadata))
    data[64:64 + len(metadata)] = metadata
    for index in range(65536):
        value = 0xfffd
        raw = bytes([index]) if index < 256 else struct.pack('<H', index)
        try:
            text = raw.decode('cp932')
            if len(text) == 1 and index & 255 not in (0xed, 0xee) and index not in CP932_DUPLICATES:
                text = chr(mapping.get(ord(text), ord(text)))
                encoded = text.encode('utf-16-le')
                value = int.from_bytes(encoded, 'little')
        except UnicodeError:
            pass
        struct.pack_into('<I', data, TABLE + index * 4, value)
    code = bytes.fromhex('0fb74508e8000000005a8b8482') + struct.pack('<i', TABLE - CODE - 9) + bytes.fromhex('894508c1e810f7d819c0f7d840c3')
    data[CODE:CODE + len(code)] = code
    font = bytearray(bytes.fromhex('8d4e08c64117868a411b24f03c10'))
    jumps = []

    def short(op):
        font.extend([op, 0])
        return len(font) - 1

    jumps.append(short(0x74))
    font.extend(bytes.fromhex('8d511cb81d000000'))
    loop = len(font)
    font.extend(bytes.fromhex('813a96be92a9'))
    jumps.append(short(0x74))
    font.extend([0x42, 0x48, 0x75, (loop - len(font) - 4) & 255])

    def face(name):
        encoded = name.encode('ascii').ljust(32, b'\0')
        for index in range(0, 32, 4):
            font.extend([0xc7, 0x41, 28 + index])
            font.extend(encoded[index:index + 4])

    face('SimHei')
    done = short(0xeb)
    for jump in jumps:
        font[jump] = len(font) - jump - 1
    face('SimSun')
    font[done] = len(font) - done - 1
    font.extend(bytes.fromhex('e800000000'))
    popped = len(font)
    font.extend(bytes.fromhex('5a8b82'))
    font.extend(struct.pack('<i', 0x4d010 - (rva + CODE + 0x100 + popped)))
    font.extend(bytes.fromhex('51ffd0c3'))
    data[CODE + 0x100:CODE + 0x100 + len(font)] = font
    lead = bytes.fromhex('8b4424042d8100000083f81e760d83e85f83f81c0f96c00fb6c0c3b801000000c3')
    data[CODE + 0x200:CODE + 0x200 + len(lead)] = lead
    return data


def patch_lead(pe, rva):
    site = 0x3ad8a
    at = pe.offset(site)
    target = (site + 5 + pe.u32(at + 1)) & 0xffffffff
    replacement = rva + CODE + 0x200
    if pe.data[at] != 0xe8 or target not in (0x41248, replacement):
        raise ValueError('消息双字节解析位置不是已验证的版本。')
    pe.w32(at + 1, replacement - site - 5)


def apply(original, mapping):
    pe = Pe(original)
    sh = pe.section('.kguw')
    if sh is not None:
        read_map(original)
        raw, rva = pe.u32(sh + 20), pe.u32(sh + 12)
        built = payload(rva, mapping)
        if pe.u32(sh + 16) < len(built):
            raise ValueError('现有Unicode节容量不足。')
        pe.data[raw:raw + len(built)] = built
        patch_lead(pe, rva)
        return bytes(pe.data)
    if hashlib.sha256(original).hexdigest().upper() != ORIGINAL_HASH:
        raise ValueError('图形库不是已验证的Unicode补丁版本。')
    rva = pe.next_rva()
    if pe.add_section(payload(rva, mapping), '.kguw', 0x60000060) != rva:
        raise ValueError('PE节地址不匹配。')
    at = pe.offset(0x405bf)
    pe.data[at:at + 27] = b'\x90' * 27
    pe.data[at] = 0xe8
    pe.w32(at + 1, rva + CODE - 0x405bf - 5)
    pe.data[at + 5:at + 10] = bytes.fromhex('508d450850')
    at = pe.offset(0x40781)
    pe.data[at:at + 10] = b'\x90' * 10
    pe.data[at] = 0xe8
    pe.w32(at + 1, rva + CODE + 0x100 - 0x40781 - 5)
    pe.remove_relocations(0x40781, 10)
    patch_lead(pe, rva)
    descriptor = pe.offset(pe.u32(pe.opt + 104))
    while pe.u32(descriptor + 12):
        lookup = pe.u32(descriptor) or pe.u32(descriptor + 16)
        at = pe.offset(lookup)
        while pe.u32(at):
            value = pe.u32(at)
            if not value & 0x80000000:
                start = pe.offset(value) + 2
                end = pe.data.find(0, start)
                if pe.data[start:end] == b'TextOutA':
                    pe.data[start + 7] = ord('W')
            at += 4
        descriptor += 20
    return bytes(pe.data)
