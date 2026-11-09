"""XP3 name layouts, preserved index fields and sparse script recognition."""
import io
from pathlib import Path
import struct
import sys
import tempfile
import unittest
import zlib


ROOT = Path(__file__).resolve().parents[3]
sys.path[:0] = [str(ROOT / 'adapters/components'), str(ROOT / 'adapters/scripts/common')]
import xp3_adler
from xp3_container import Archive, INV
from xp3_runtime import execute


def chunk(tag, body):
    return tag + struct.pack('<Q', len(body)) + body


def archive_bytes(members, *, trailer=b'', info_size=None, filtered=True, checksum=None):
    data = bytearray(b'XP3\r\n \n\x1a\x8bg\x01' + bytes(8))
    entries = []
    for name, content in members:
        stored = zlib.compress(content.translate(INV) if filtered else content)
        segment = struct.pack('<IQQQ', 1, len(data), len(content), len(stored))
        data.extend(stored)
        encoded = name.encode('utf-16le')
        info = struct.pack('<IQQH', 0x80000000, len(content) if info_size is None else info_size,
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


def sparse_probe(root, contents):
    source = root / 'source'
    source.mkdir()
    for name, data in contents.items():
        (source / name).write_bytes(data[:19])
    request = dict(operation='probe', sourceDirectory=str(source),
                   probeFiles=[dict(path=name, length=len(data)) for name, data in contents.items()],
                   probeRanges=[])
    reads = []
    for attempt in range(12):
        result = execute(request, 'rot')
        if not result.get('probeReads'):
            return result, reads
        for read in result['probeReads']:
            offset, length = read['offset'], read['length']
            data = contents[read['path']][offset:offset + length]
            part = root / f'range-{attempt}-{len(reads)}'
            part.write_bytes(data)
            request['probeRanges'].append(dict(read, dataPath=str(part)))
            reads.append(read)
    raise AssertionError('XP3 probe did not finish within twelve range requests')


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

    def test_rebuild_preserves_name_terminators_and_unknown_metadata(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            original = root / 'original.xp3'
            original.write_bytes(archive_bytes([('scene.ks', b'Hello\n'), ('other.ks', b'World\n')],
                                               trailer=b'\0\0'))
            archive = Archive(original, 'rot')
            output = root / 'output.xp3'
            archive.build(output, {'scene.ks': b'Changed\n'})
            rebuilt = Archive(output, 'rot')
            self.assertEqual([rebuilt.read(entry) for entry in rebuilt.entries], [b'Changed\n', b'World\n'])
            records = list(xp3_adler._chunks(rebuilt.raw_index))
            self.assertEqual(records[-1], (b'extr', b'preserved archive metadata'))
            for tag, body in records[:-1]:
                fields = dict(xp3_adler._chunks(body))
                self.assertEqual(fields[b'info'][-2:], b'\0\0')
                self.assertEqual(fields[b'extr'], b'preserved member metadata')

    def test_sparse_probe_reads_the_index_and_a_script_before_matching(self):
        with tempfile.TemporaryDirectory() as directory:
            contents = {'data.xp3': archive_bytes([('scene.ks', b'Hello\n')], trailer=b'\0\0'),
                        'image.xp3': archive_bytes([('image.png', b'image bytes')])}
            result, reads = sparse_probe(Path(directory), contents)
            self.assertTrue(reads)
            self.assertEqual(result['probe']['requiredFiles'], ['data.xp3'])

    def test_probe_rejects_a_header_without_a_valid_index(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(ValueError, 'XP3'):
                sparse_probe(Path(directory), {'data.xp3': xp3_adler.MAGIC + bytes(8)})

    def test_probe_streams_full_resource_inputs_without_sparse_ranges(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'data.xp3').write_bytes(archive_bytes([('scene.ks', b'Hello\n')], trailer=b'\0\0'))
            result = execute(dict(operation='probe', sourceDirectory=directory, resourceFiles=['data.xp3']), 'rot')
            self.assertEqual(result['probe']['requiredFiles'], ['data.xp3'])

    def test_probe_requires_a_matching_checksum_even_when_the_declared_value_is_zero(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(ValueError):
                sparse_probe(Path(directory), {'data.xp3': archive_bytes([('scene.ks', b'Hello\n')], checksum=0)})

    def test_probe_rejects_an_archive_without_supported_scripts(self):
        with tempfile.TemporaryDirectory() as directory:
            result, _ = sparse_probe(Path(directory), {'image.xp3': archive_bytes([('image.png', b'image')])})
            self.assertIsNone(result['probe'])

    def test_probe_rejects_an_incompatible_filter_or_member_layout(self):
        for data in (archive_bytes([('scene.ks', b'Hello\n')], filtered=False),
                     archive_bytes([('scene.ks', b'Hello\n')], trailer=b'\0\0', info_size=910)):
            with self.subTest(data=data[-16:]), tempfile.TemporaryDirectory() as directory:
                with self.assertRaises(ValueError):
                    sparse_probe(Path(directory), {'data.xp3': data})


if __name__ == '__main__':
    unittest.main()
