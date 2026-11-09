"""SMIL identities, SmileCrypt vectors and Unicode resource roundtrips."""
import hashlib
import io
import json
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest
import zlib

ROOT = Path(__file__).resolve().parents[3]
FIXTURES = ROOT / 'crates/engines/tests/fixtures'
PIN = json.loads((ROOT / 'third_party/engine-tools/current.json').read_text('utf-8'))
sys.path[:0] = [str(ROOT / 'adapters/components'), str(ROOT / 'adapters/scripts/common'),
               str(ROOT / 'adapters/kirikiri'), str(FIXTURES),
               str(ROOT / 'third_party/engine-tools/references' / PIN['revision'] / 'library.zip')]
import smil_adapter as adapter
import xp3_smil as xp3
from xp3_adler import MAGIC, _chunks
from kirikiri_scn_fixture import psb


def chunk(tag, body):
    return tag + struct.pack('<Q', len(body)) + body


def archive_bytes(members, *, filter_spec=adapter.FILTER, stub=True, mismatch=False):
    """Independent XP3 layout writer; members are (name, bytes, alias, filtered)."""
    data = bytearray(MAGIC + struct.pack('<Q', 23) + bytes(13) + bytes(8))
    data.extend(b'\x89PNG\n\x1a\n' + bytes(950 - 47))
    records = []
    if stub:
        name = '$$$security'.encode('utf-16le')
        info = struct.pack('<IQQH', 0, 910, 910, len(name) // 2) + name + b'\0\0'
        body = chunk(b'info', info) + chunk(b'segm', struct.pack('<IQQQ', 0, 88, 157, 157))
        records.append(chunk(b'File', body + chunk(b'adlr', struct.pack('<I', 0x689792e4))))
    for name, content, alias, filtered in members:
        checksum = zlib.adler32(content) & 0xffffffff
        if alias is None:
            alias = hashlib.md5(name.translate(xp3.ASCII_LOWER).encode('utf-16le')).hexdigest()
        stored = filter_spec.transform(content, checksum) if filtered else content
        # Two segments exercise logical offset filtering across segment boundaries.
        split = len(stored) // 2
        segments = []
        for part in (stored[:split], stored[split:]):
            packed = zlib.compress(part)
            segments.append(struct.pack('<IQQQ', 1, len(data), len(part), len(packed)))
            data.extend(packed)
        encoded = alias.encode('utf-16le')
        info = struct.pack('<IQQH', 0x80000000, len(content),
                           sum(struct.unpack('<IQQQ', part)[3] for part in segments), len(encoded) // 2)
        body = chunk(b'adlr', struct.pack('<I', checksum)) + chunk(b'segm', b''.join(segments))
        body += chunk(b'info', info + encoded + b'\0\0') + chunk(b'time', b'12345678')
        records.append(chunk(b'File', body))
        if alias != 'startup.tjs':
            encoded = name.encode('utf-16le')
            records.append(chunk(b'smil', struct.pack('<IH', checksum ^ int(mismatch), len(encoded) // 2)
                                 + encoded + b'\0\0'))
    raw = b''.join(records)
    packed = zlib.compress(raw)
    struct.pack_into('<Q', data, 32, len(data))
    data.extend(b'\1' + struct.pack('<QQ', len(packed), len(raw)) + packed)
    return bytes(data)


def sample(dialogue_count=1):
    tree = dict(name='scenario', scenes=[dict(title='Opening', texts=[
        ['internal-id', [['Alice' if index == 0 else None, r'Hello\nworld.', 11, 'Helloworld.', 'Helloworld.']], None, None, None]
        for index in range(dialogue_count)],
        selects=[dict(text='Continue')])])
    return archive_bytes([
        ('startup.tjs', b'TJS2100\0retained bytecode', 'startup.tjs', False),
        ('startup.tjs', b'Scripts.execStorage("system/Initialize.tjs");\n', None, True),
        ('scene.ks.scn', psb(tree), None, True),
        ('dialogue.ks', 'こんにちは。\n'.encode('cp932'), None, True),
        ('config.tjs', 'var internal="resource-id"; caption="設定";\n'.encode('cp932'), None, True),
        ('Image/Ａ.png', b'unchanged image payload', None, True),
    ])


class SmilTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.source = self.root / 'source'
        self.source.mkdir()
        (self.source / 'data.xp3').write_bytes(sample())
        self.request = dict(operation='extract', sourceDirectory=str(self.source), resourceFiles=['data.xp3'])

    def test_filter_known_vectors_and_logical_offsets(self):
        f = xp3.SmileFilter(0x11223344, 0x7b, 0x5e)
        # Folded key 0x44, first-byte mask 0x44; independent expected bytes.
        self.assertEqual(f.transform(b'ABC', 0), b'A\x06\x07')
        self.assertEqual(f.transform(b'ABC', 0, offset=5), b'\x05\x06\x07')
        # Zero hash uses both fallback masks.
        self.assertEqual(f.transform(b'ABC', 0x11223344), b'd\x1c\x1d')
        self.assertEqual(f.transform(b'', 0), b'')
        for values in ((-1, 1, 2), (0, 256, 2), (True, 1, 2)):
            with self.assertRaises(ValueError):
                xp3.SmileFilter(*values)

    def test_ascii_hash_and_two_startup_members_keep_distinct_aliases(self):
        stream = io.BytesIO(sample())
        index = xp3.read_index(stream)
        self.assertEqual(index.security_stub_count, 1)
        startup = [entry for entry in index.entries if entry.name == 'startup.tjs']
        self.assertEqual(len(startup), 2)
        self.assertNotEqual(startup[0].alias, startup[1].alias)
        image = next(entry for entry in index.entries if entry.name == 'Image/Ａ.png')
        self.assertEqual(image.alias, hashlib.md5('image/Ａ.png'.encode('utf-16le')).hexdigest())
        self.assertEqual(xp3.read_member(stream, image, adapter.FILTER), b'unchanged image payload')
        self.assertFalse(xp3.decode_member(stream, startup[0], adapter.FILTER)[1])
        self.assertTrue(xp3.decode_member(stream, startup[1], adapter.FILTER)[1])

    def test_bad_mapping_and_filter_fail_before_acceptance(self):
        with self.assertRaises(xp3.UnsupportedArchive):
            xp3.read_index(io.BytesIO(archive_bytes([('one.ks', b'Hello\n', None, True)], mismatch=True)))
        wrong = archive_bytes([('one.ks', b'Hello\n', None, True), ('two.ks', b'World\n', None, True)],
                              filter_spec=xp3.SmileFilter(123, 7, 9))
        (self.source / 'data.xp3').write_bytes(wrong)
        self.assertIsNone(adapter.execute(dict(self.request, operation='probe'))['probe'])

    def test_checksum_alias_fallback_keeps_the_literal_bootstrap_identity(self):
        content = b'TJS2100 shared bytes'
        raw = archive_bytes([('startup.tjs', content, 'startup.tjs', False),
                             ('other.tjs', content, 'opaque-alias', True)])
        index = xp3.read_index(io.BytesIO(raw))
        self.assertEqual([(entry.alias, entry.name) for entry in index.entries],
                         [('startup.tjs', 'startup.tjs'), ('opaque-alias', 'other.tjs')])

    def test_native_and_unicode_aliases_do_not_merge_distinct_fullwidth_names(self):
        names = ['Image/Ａ.png', 'Image/ａ.png']
        raw = archive_bytes([(name, b'same checksum', None, True) for name in names])
        self.assertEqual([entry.name for entry in xp3.read_index(io.BytesIO(raw)).entries], names)
        alias = hashlib.md5(names[0].lower().encode('utf-16le')).hexdigest()
        alternative = archive_bytes([(names[0], b'Unicode alias', alias, True)])
        self.assertEqual(xp3.read_index(io.BytesIO(alternative)).entries[0].name, names[0])

    def test_rebuild_preserves_prefix_aliases_stub_times_and_unmodified_segments(self):
        original = self.source / 'data.xp3'
        index = xp3.read_index(io.BytesIO(original.read_bytes()))
        changed = next(entry for entry in index.entries if entry.name == 'dialogue.ks')
        destination = self.root / 'changed.xp3'
        new_text = b'\xff\xfe' + '中文。\n'.encode('utf-16le')
        xp3.rebuild(original, destination, index, {changed.alias: new_text}, adapter.FILTER)
        rebuilt = xp3.read_index(io.BytesIO(destination.read_bytes()))
        self.assertEqual([(e.alias, e.name, e.flags) for e in index.entries],
                         [(e.alias, e.name, e.flags) for e in rebuilt.entries])
        old, new = original.read_bytes(), destination.read_bytes()
        self.assertEqual(old[:32] + old[40:950], new[:32] + new[40:950])
        self.assertEqual(index.chunks[0], rebuilt.chunks[0])
        for a, b in zip(index.entries, rebuilt.entries, strict=True):
            if a.alias == changed.alias:
                self.assertEqual(xp3.read_member(io.BytesIO(new), b, adapter.FILTER), new_text)
            else:
                self.assertEqual(a.adler32, b.adler32)
                self.assertEqual([old[s.offset:s.offset+s.packed_size] for s in a.segments],
                                 [new[s.offset:s.offset+s.packed_size] for s in b.segments])
        for (tag, before), (_, after) in zip(index.chunks, rebuilt.chunks, strict=True):
            if tag == b'File':
                self.assertEqual(dict(_chunks(before)).get(b'time'), dict(_chunks(after)).get(b'time'))

    def test_profile_extracts_semantic_text_and_partial_chinese_readback(self):
        units = adapter.execute(self.request)['extraction']['units']
        self.assertEqual(len(units), 6)
        self.assertEqual({unit['kind'] for unit in units}, {'dialogue', 'ui', 'name', 'choice'})
        self.assertFalse(any(unit['sourceText'] in ('internal-id', 'resource-id') for unit in units))
        chosen = {unit['id']: '汉化：' + unit['sourceText'] for unit in units if unit['kind'] != 'name'}
        output = self.root / 'changed'
        adapter.execute(dict(self.request, operation='build', outputDirectory=str(output), translations=chosen))
        after = adapter.execute(dict(self.request, sourceDirectory=str(output)))['extraction']['units']
        self.assertEqual({u['id']: u['sourceText'] for u in after},
                         {u['id']: chosen.get(u['id'], u['sourceText']) for u in units})
        self.assertTrue(adapter.execute(dict(self.request, operation='validate', outputDirectory=str(output),
                                            translations=chosen))['validation']['passed'])
        with (output / 'data.xp3').open('rb') as stream:
            index = xp3.read_index(stream)
            entry = next(e for e in index.entries if e.name == 'config.tjs')
            self.assertTrue(xp3.read_member(stream, entry, adapter.FILTER).startswith(b'\xff\xfe'))

    def test_no_translation_is_byte_identical_and_unknown_ids_fail(self):
        output = self.root / 'noop'
        adapter.execute(dict(self.request, operation='build', outputDirectory=str(output), translations={}))
        self.assertEqual((self.source / 'data.xp3').read_bytes(), (output / 'data.xp3').read_bytes())
        with self.assertRaisesRegex(ValueError, '文本 ID'):
            adapter.execute(dict(self.request, operation='build', outputDirectory=str(output),
                                 translations={'data.xp3|unknown|field': '中文'}))

    def test_tjs_literals_escape_the_original_quote_and_embedded_newlines(self):
        content = b"var key='internal'; caption='Hello'; hint=\"Line\\nTwo\";\n"
        (self.source / 'data.xp3').write_bytes(archive_bytes([('config.tjs', content, None, True)]))
        units = adapter.execute(self.request)['extraction']['units']
        values = {unit['id']: "中文'\"\n第二行" for unit in units}
        output = self.root / 'quotes'
        adapter.execute(dict(self.request, operation='build', outputDirectory=str(output), translations=values))
        after = adapter.execute(dict(self.request, sourceDirectory=str(output)))['extraction']['units']
        self.assertEqual({u['id']: u['sourceText'] for u in after}, values)
        self.assertEqual([u['id'].split('|')[-1] for u in units], ['L1:1', 'L1:2'])

    def test_validation_context_requires_current_resource_bindings(self):
        units = adapter.execute(self.request)['extraction']['units']
        output = self.root / 'output'
        context = dict(schemaVersion=1, sourceDirectory=str(self.source), outputDirectory=str(output),
                       originalUnits=units, outputUnits=units, outputFiles=['data.xp3'], resourceFiles=['data.xp3'])
        path = self.root / 'context.json'
        path.write_text(json.dumps(context), 'utf-8')
        request = dict(self.request, operation='validate', outputDirectory=str(output), validationContextFile=str(path))
        self.assertTrue(adapter.execute(request)['validation']['passed'])
        with self.assertRaises(ValueError):
            adapter.execute(dict(request, resourceFiles=['other.xp3']))

    def test_sparse_probe_finishes_within_host_round_and_range_limits(self):
        contents = (self.source / 'data.xp3').read_bytes()
        staged = self.root / 'staged'
        staged.mkdir()
        (staged / 'data.xp3').write_bytes(contents[:40])
        request = dict(self.request, operation='probe', sourceDirectory=str(staged), probeRanges=[],
                       probeFiles=[dict(path='data.xp3', length=len(contents))])
        for attempt in range(5):
            result = adapter.execute(request)
            if not result.get('probeReads'):
                self.assertEqual(result['probe']['requiredFiles'], ['data.xp3'])
                break
            for part in result['probeReads']:
                self.assertLessEqual(part['length'], 1 << 20)
                target = self.root / ('range-' + str(len(request['probeRanges'])))
                target.write_bytes(contents[part['offset']:part['offset']+part['length']])
                request['probeRanges'].append(dict(part, dataPath=str(target)))
        else:
            self.fail('SMIL probe exceeded the host round limit')

    def test_scn_strict_and_partial_component_contracts(self):
        script = FIXTURES / 'check_kirikiri_scn_component.py'
        component = ROOT / 'adapters/components/kirikiri_scn_text.py'
        bootstrap = 'import runpy,sys;sys.path[:0]=sys.argv[1:3];sys.argv=sys.argv[3:];runpy.run_path(sys.argv[0],run_name="__main__")'
        result = subprocess.run([sys.executable, '-I', '-X', 'utf8', '-c', bootstrap,
                                 str(FIXTURES / 'kirikiri-scn-reference-442ae2f.zip'), str(FIXTURES),
                                 str(script), str(component)], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == '__main__':
    if len(sys.argv) in (3, 4) and sys.argv[1] == '--fixture-directory':
        directory = Path(sys.argv[2])
        directory.mkdir(parents=True, exist_ok=True)
        (directory / 'data.xp3').write_bytes(sample(int(sys.argv[3]) if len(sys.argv) == 4 else 1))
        print(json.dumps({'success': True}))
    else:
        unittest.main()
