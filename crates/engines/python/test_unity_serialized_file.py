"""Unity container preservation, TextAsset byte payloads and bounded inspection."""
import json
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / 'adapters/unity'))
from serialized_file import SerializedFile, probe


def aligned_bytes(raw, padding=b'\0'):
    return struct.pack('<i', len(raw)) + raw + padding * (-len(raw) % 4)


def text_payload(name, script):
    return aligned_bytes(name.encode('utf-8'), b'\xa5') + aligned_bytes(script, b'\xb6') + b'unknown-tail'


def container(objects, *, tree=False, unity_version=b'2022.3.62f2'):
    """Write independent v22 metadata; objects are (path ID, class ID, payload)."""
    classes = list(dict.fromkeys(class_id for _, class_id, _ in objects))
    meta = bytearray(unity_version + b'\0' + struct.pack('<iBi', 5, tree, len(classes)))
    for class_id in classes:
        meta.extend(struct.pack('<iBh', class_id, 0, -1))
        if class_id == 114:
            meta.extend(bytes(16))
        meta.extend(bytes(16))
        if tree:
            meta.extend(struct.pack('<iii', 0, 0, 0))
    meta.extend(struct.pack('<i', len(objects)))
    payload = bytearray(b'GAPDATA!')
    entries = {}
    for pid, class_id, raw in objects:
        meta.extend(bytes(-len(meta) % 4))
        payload.extend(b'\xcc' * (-len(payload) % 8))
        entries[pid] = 48 + len(meta)
        meta.extend(struct.pack('<qqIi', pid, len(payload), len(raw), classes.index(class_id)))
        payload.extend(raw)
    meta.extend(struct.pack('<iii', 0, 0, 0) + b'\0')
    data_offset = (48 + len(meta) + 15) // 16 * 16
    data = bytearray(48)
    struct.pack_into('>I', data, 8, 22)
    struct.pack_into('>IQQ', data, 20, len(meta), data_offset + len(payload) + 7, data_offset)
    data.extend(meta)
    data.extend(bytes(data_offset - len(data)))
    data.extend(payload + b'trailer')
    return bytes(data), entries


class UnitySerializedFileTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.path = self.root / 'data.assets'
        self.pid = 2**48 + 7
        self.original, self.entries = container([
            (self.pid, 49, text_payload('dialogue', b'original')),
            (19, 28, b'\xff\x01opaque-object'),
            (42, 49, text_payload('binary', b'\xff\xfe\0\0')),
        ])
        self.path.write_bytes(self.original)

    def test_changed_text_preserves_names_binary_assets_and_opaque_data(self):
        asset = SerializedFile(self.path)
        original = asset.text_asset(self.pid)
        self.assertEqual(original.name, 'dialogue')
        changed = '中文更长的文本😀'.encode('utf-8')
        output = asset.rebuild_text_assets({self.pid: changed})
        output_path = self.root / 'output.assets'
        output_path.write_bytes(output)
        rebuilt = SerializedFile(output_path)
        self.assertEqual(set(asset.objects), set(rebuilt.objects))
        self.assertEqual(rebuilt.text_asset(self.pid).script, changed)
        self.assertEqual(rebuilt.text_asset(self.pid).name, original.name)
        self.assertEqual(rebuilt.raw(19), asset.raw(19))
        self.assertEqual(rebuilt.raw(42), asset.raw(42))
        self.assertEqual(rebuilt.raw(self.pid)[:original.script_offset], original.raw[:original.script_offset])
        self.assertTrue(rebuilt.raw(self.pid).endswith(b'unknown-tail'))
        self.assertIn(b'GAPDATA!', output)
        self.assertTrue(output.endswith(b'trailer'))
        self.assertEqual(self.path.read_bytes(), self.original)

    def test_unchanged_scripts_keep_nonzero_padding_and_original_bytes(self):
        asset = SerializedFile(self.path)
        self.assertEqual(asset.rebuild_text_assets({}), self.original)
        self.assertEqual(asset.rebuild_text_assets({self.pid: b'original'}), self.original)
        self.assertEqual(asset.text_asset(42).script, b'\xff\xfe\0\0')
        with self.assertRaises(ValueError):
            asset.rebuild_text_assets({19: b'text'})
        with self.assertRaises(KeyError):
            asset.rebuild_text_assets({999: b'text'})
        with self.assertRaises(TypeError):
            asset.rebuild_text_assets({self.pid: 'text'})

    def test_inspection_filters_and_pages_names_without_returning_script_content(self):
        result = probe(self.path, limit=1)
        self.assertEqual(result['textAssetCount'], 2)
        self.assertTrue(result['more'])
        self.assertEqual(result['textAssets'][0]['pathId'], self.pid)
        self.assertEqual(probe(self.path, offset=1)['textAssets'][0]['name'], 'binary')
        self.assertEqual(probe(self.path, query='BINARY')['totalMatches'], 1)
        self.assertNotIn('original', json.dumps(result))
        for options in ({'offset': -1}, {'limit': 0}, {'limit': 33}, {'limit': True}, {'query': 1}):
            with self.assertRaises(ValueError):
                probe(self.path, **options)

    def test_metadata_and_objects_cannot_extend_into_invalid_ranges(self):
        variants = []
        wrong_version = bytearray(self.original)
        struct.pack_into('>I', wrong_version, 8, 21)
        variants.append(wrong_version)
        big_endian = bytearray(self.original)
        big_endian[16] = 1
        variants.append(big_endian)
        wrong_count = bytearray(self.original)
        struct.pack_into('<i', wrong_count, 48 + 12 + 5, 100000)
        variants.append(wrong_count)
        overlap = bytearray(self.original)
        struct.pack_into('<q', overlap, self.entries[19] + 8, 8)
        variants.append(overlap)
        variants.append(self.original[:-1])
        for raw in variants:
            self.path.write_bytes(raw)
            with self.assertRaises(ValueError):
                SerializedFile(self.path)

    def test_type_tree_and_zero_textasset_containers_are_supported(self):
        raw, _ = container([(1, 28, b'opaque')], tree=True)
        self.path.write_bytes(raw)
        result = probe(self.path)
        self.assertTrue(result['matched'])
        self.assertEqual(result['textAssetCount'], 0)
        self.assertEqual(result['textAssets'], [])

    def test_non_2022_unity_v22_versions_roundtrip_and_probe(self):
        for version in (b'2021.3.35f1', b'6000.0.32f1'):
            with self.subTest(version=version):
                raw, _ = container([(7, 49, text_payload('story', b'hello'))], unity_version=version)
                self.path.write_bytes(raw)
                result = probe(self.path)
                self.assertTrue(result['matched'])
                self.assertEqual(result['unityVersion'], version.decode('ascii'))
                asset = SerializedFile(self.path)
                self.assertEqual(asset.rebuild_text_assets({}), raw)
                rebuilt = asset.rebuild_text_assets({7: '中文'.encode('utf-8')})
                output = self.root / f'{version.decode("ascii")}.assets'
                output.write_bytes(rebuilt)
                self.assertEqual(SerializedFile(output).text_asset(7).script, '中文'.encode('utf-8'))

    def test_probe_cli_returns_json_and_never_writes_the_input(self):
        completed = subprocess.run([sys.executable, '-I', '-X', 'utf8',
            str(ROOT / 'adapters/unity/serialized_file.py')],
            input=json.dumps({'operation': 'probe', 'input': str(self.path), 'query': 'dialogue'}),
            text=True, encoding='utf-8', capture_output=True, check=True)
        self.assertEqual(json.loads(completed.stdout)['totalMatches'], 1)
        self.assertEqual(self.path.read_bytes(), self.original)


if __name__ == '__main__':
    unittest.main()
