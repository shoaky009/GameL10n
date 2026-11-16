"""XP3 component name layouts, index metadata and checked member reads."""
import io
from pathlib import Path
import struct
import sys
import unittest
import zlib


ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / 'adapters/components'))
import xp3_adler


def chunk(tag, body):
    return tag + struct.pack('<Q', len(body)) + body


def archive_bytes(members, *, trailer=b'', info_size=None, checksum=None):
    data = bytearray(b'XP3\r\n \n\x1a\x8bg\x01' + bytes(8))
    entries = []
    for name, content in members:
        stored = zlib.compress(content)
        segment = struct.pack('<IQQQ', 1, len(data), len(content), len(stored))
        data.extend(stored)
        encoded = name.encode('utf-16le')
        info = struct.pack('<IQQH', 0, len(content) if info_size is None else info_size,
                           len(stored), len(encoded) // 2) + encoded + trailer
        body = chunk(b'info', info) + chunk(b'segm', segment)
        body += chunk(b'adlr', struct.pack('<I', zlib.adler32(content) & 0xffffffff
                                          if checksum is None else checksum))
        body += chunk(b'extr', b'preserved member metadata')
        entries.append(chunk(b'File', body))
    index = b''.join(entries) + chunk(b'extr', b'preserved archive metadata')
    packed = zlib.compress(index)
    struct.pack_into('<Q', data, 11, len(data))
    data.extend(b'\1' + struct.pack('<QQ', len(packed), len(index)) + packed)
    return bytes(data)


class Xp3Test(unittest.TestCase):
    def test_names_accept_an_optional_utf16_terminator(self):
        for trailer in (b'', b'\0\0'):
            with self.subTest(trailer=trailer):
                stream = io.BytesIO(archive_bytes([('scene.ks', b'Hello\n')], trailer=trailer))
                index = xp3_adler.read_index(stream)
                self.assertEqual([entry.name for entry in index.entries], ['scene.ks'])

    def test_names_reject_unknown_trailers_and_embedded_nuls(self):
        for name, trailer in [('scene.ks', b'\0'), ('scene.ks', b'xx'),
                              ('scene.ks', bytes(4)), ('scene\0.ks', b'\0\0')]:
            with self.subTest(name=name, trailer=trailer):
                data = archive_bytes([(name, b'Hello\n')], trailer=trailer)
                with self.assertRaises(ValueError):
                    xp3_adler.read_index(io.BytesIO(data))

    def test_member_length_mismatches_remain_rejected_with_a_terminated_name(self):
        data = archive_bytes([('scene.ks', b'Hello\n')], trailer=b'\0\0', info_size=910)
        with self.assertRaisesRegex(ValueError, 'member/segment lengths disagree'):
            xp3_adler.read_index(io.BytesIO(data))

    def test_index_reports_unknown_metadata_for_preservation(self):
        stream = io.BytesIO(archive_bytes([('scene.ks', b'Hello\n')], trailer=b'\0\0'))
        index = xp3_adler.read_index(stream)
        self.assertEqual(index.unknown_chunk_count, 2)
        self.assertEqual(xp3_adler.read_member(stream, index.entries[0]), b'Hello\n')

    def test_member_requires_a_matching_checksum_even_when_declared_zero(self):
        stream = io.BytesIO(archive_bytes([('scene.ks', b'Hello\n')], checksum=0))
        index = xp3_adler.read_index(stream)
        with self.assertRaisesRegex(ValueError, 'Adler32 mismatch'):
            xp3_adler.read_member(stream, index.entries[0])


if __name__ == '__main__':
    unittest.main()
