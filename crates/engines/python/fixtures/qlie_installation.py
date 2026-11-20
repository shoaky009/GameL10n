"""Small keyed PACK fixtures with ImoScripter settings in a separate volume."""
from pathlib import Path
import struct
import sys

if __name__ == '__main__':
    sys.path.insert(0, sys.argv[1])
from python.archives import qlie

GAME_KEY = bytes(range(256))
EXTERNAL_KEY = GAME_KEY[::-1]


def short(data):
    return bytes([len(data)]) + data


def key_resource_exe():
    picture = b'\x05TIcon' + GAME_KEY
    blob = b'TPF0' + short(b'TForm') + short(b'Form1') + b'\0'
    blob += short(b'TImage') + short(b'IconKeyImage') + short(b'Picture.Data')
    blob += b'\x0a' + struct.pack('<I', len(picture)) + picture + b'\0\0\0'

    resource = bytearray(112) + blob
    struct.pack_into('<HHII', resource, 12, 0, 1, 10, 0x80000018)
    struct.pack_into('<HHII', resource, 36, 1, 0, 0x80000058, 0x80000030)
    struct.pack_into('<HHII', resource, 60, 0, 1, 1041, 72)
    struct.pack_into('<II', resource, 72, 0x1000 + 112, len(blob))
    resource[88:102] = struct.pack('<H', 6) + 'TFORM1'.encode('utf-16-le')

    header = bytearray(512)
    header[:2] = b'MZ'
    struct.pack_into('<I', header, 0x3C, 64)
    header[64:68] = b'PE\0\0'
    struct.pack_into('<HHIIIHH', header, 68, 0x14C, 1, 0, 0, 0, 224, 0x102)
    optional = 88
    struct.pack_into('<H', header, optional, 0x10B)
    struct.pack_into('<I', header, optional + 16, 0x1000 + len(resource))
    struct.pack_into('<III', header, optional + 28, 0x400000, 0x1000, 512)
    struct.pack_into('<II', header, optional + 56, 0x2000, len(header))
    struct.pack_into('<I', header, optional + 92, 16)
    struct.pack_into('<II', header, optional + 112, 0x1000, len(resource))
    section = optional + 224
    header[section:section + 8] = b'.rsrc\0\0\0'
    struct.pack_into('<IIII', header, section + 8, len(resource) + 1, 0x1000, 512, len(header))
    struct.pack_into('<I', header, section + 36, 0x60000020)
    return header + (resource + b'\xc3').ljust(512, b'\0')


def make_pack(members, internal_key):
    arc_key = qlie.hash_v3(bytes(256)) & 0xFFFFFFF
    contents = bytearray()
    index = bytearray()
    names = []
    for name, raw in [(qlie.PACK_KEY_NAME, internal_key), *members]:
        encoded_name = name.encode('cp932')
        names.append(encoded_name)
        entry = qlie.Entry(name, encoded_name, len(contents), len(raw), len(raw), 0, 4, 0)
        cipher = qlie.encrypt(raw, entry, arc_key, mode='keyed', game_key=GAME_KEY,
                              key_file=EXTERNAL_KEY if len(names) == 1 else internal_key)
        index += struct.pack('<H', len(encoded_name)) + qlie.crypt_name(encoded_name, arc_key)
        index += struct.pack('<QIIIII', entry.offset, entry.size, entry.unpacked_size,
                             entry.packed, entry.encryption, qlie.hash_v3(cipher))
        contents.extend(cipher)

    table = struct.pack('<H', len(names))
    for ordinal, name in enumerate(names):
        table += struct.pack('<H', len(name)) + name
        table += struct.pack('<QI', ordinal * 2, 0)
    table += bytes(2 * 255) + struct.pack(f'<{len(names)}H', *range(len(names)))
    # Identity byte-pair table with skips over both halves of the alphabet.
    packed = b'1PC\xff' + struct.pack('<II', 0, len(table))
    packed += bytes([255, 128, 254]) + struct.pack('<I', len(table)) + table
    hash_entry = qlie.Entry('hash', b'hash', 0, len(packed), len(packed), 0, 1, 0)
    encrypted = qlie.encrypt(packed, hash_entry, 0x428, mode='legacy')
    tail = b'HashVer1.3\0\0\0\0\0\0' + struct.pack('<IIII', 256, len(names), len(names) * 2, len(encrypted))
    tail += encrypted + bytes(32) + struct.pack('<I', 32 + len(encrypted)) + bytes(1024)
    footer = b'FilePackVer3.0\0\0' + struct.pack('<IQ', len(names), len(contents))
    return contents + index + tail + footer


def make_installation(root):
    (root / 'GameData').mkdir()
    (root / 'DLL').mkdir()
    (root / 'game.exe').write_bytes(key_resource_exe())
    (root / 'DLL/key.fkey').write_bytes(EXTERNAL_KEY)
    (root / 'GameData/system.pack').write_bytes(make_pack([
        ('asset.bin', bytes(70 << 10)),
        ('avg/projectsetting.txt', b'FormatType=1\r\n'), ('avg/setting.txt', b'ReturnCode=[n]\r\n'),
    ], bytes([113]) * 256))
    (root / 'GameData/story.pack').write_bytes(make_pack([
        ('scenario/story.s', '【花子】\r\nこんにちは\r\n\r\nさようなら\r\n'.encode('cp932')),
    ], bytes([157]) * 256))
    (root / 'GameData/update.pack').write_bytes(make_pack([
        ('scenario/story.s', '別の会話\r\n'.encode('cp932')),
        ('notes.txt', b'original member'),
    ], bytes([253]) * 256))
    (root / 'GameData/media.pack').write_bytes(make_pack([
        ('image.bin', b'original media'),
    ], bytes([195]) * 256))


if __name__ == '__main__':
    make_installation(Path(sys.argv[2]))
