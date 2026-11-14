"""EAGLS field identities, profile checks and independent sample stages."""
import json
from pathlib import Path
import struct
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[3]
PIN = json.loads((ROOT / 'third_party/engine-tools/current.json').read_text('utf-8'))
sys.path[:0] = [str(ROOT / 'adapters/components'),
               str(ROOT / 'third_party/engine-tools/references' / PIN['revision'] / 'library.zip')]
from eagls_script import ScriptText, main
from python.engines.eagls import crypt_script
from python.engines.eagls_text import Profile


def member(text, version=2, text_offset=3600):
    header = b'START'.ljust(32, b'\0') + struct.pack('<I', 0) + bytes(text_offset - 36)
    plain = header + ('$START\r\n' + text).encode('cp932') + bytes([0x83]) * version
    return crypt_script(plain, text_offset=text_offset, key=b'EAGLS_SYSTEM', version=version)


class EaglsScriptTest(unittest.TestCase):
    def test_label_padding_hint_supports_other_text_offsets_without_accepting_damaged_tables(self):
        for text_offset in (1000, 7200):
            for version in (1, 2):
                with self.subTest(text_offset=text_offset, version=version):
                    raw = member('&1"本文"\r\n', version=version, text_offset=text_offset)
                    scene = ScriptText(raw)
                    self.assertEqual(scene.profile.text_offset, text_offset)
                    self.assertEqual(scene.build({}), raw)
                    self.assertEqual(ScriptText(scene.build({'dialogue|1|0': '変更'})).fields[0][1], '変更')
                    damaged = bytearray(raw)
                    damaged[32:36] = struct.pack('<I', len(raw) + 100)
                    with self.assertRaises(ValueError):
                        ScriptText(bytes(damaged))

    def test_known_profiles_keep_names_choices_and_variable_references_separate(self):
        text = '#太郎,0&7"こんにちは"\r\n52("_SelStr1","進む")\r\n&7"また会う"\r\n#:Player&8"続き"\r\n52("_Asset","scene.dat")\r\n52("_Title","表題")\r\n'
        for version in (1, 2):
            with self.subTest(version=version):
                raw = member(text, version)
                scene = ScriptText(raw)
                fields = {p: value for p, value, _ in scene.fields}
                self.assertEqual(scene.profile.version, version)
                self.assertEqual(len(fields), 5)
                self.assertEqual(fields['dialogue|7|0|name'], '太郎')
                self.assertEqual(scene.speaker_refs['dialogue|7|0'], 'dialogue|7|0|name')
                self.assertEqual(scene.speaker_refs['dialogue|8|0'], ':Player')
                self.assertNotIn(':Player', fields.values())
                self.assertNotIn('表題', fields.values())
                self.assertEqual(scene.build({}), raw)
                changes = {'dialogue|7|0': '長い変更後の本文', 'dialogue|7|0|name': '花子'}
                rebuilt = ScriptText(scene.build(changes))
                self.assertEqual({p: value for p, value, _ in rebuilt.fields}, fields | changes)
                self.assertEqual(rebuilt.speaker_refs, scene.speaker_refs)

    def test_unsupported_edits_do_not_change_command_or_name_semantics(self):
        scene = ScriptText(member('#太郎&1"本文%1"\r\n'))
        for changes in ({'unknown': '本文'}, {'dialogue|1|0|name': ':Variable'},
                        {'dialogue|1|0': '制御文字を削除'}, {'dialogue|1|0': '"injection"'},
                        {'dialogue|1|0': ''}):
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                scene.build(changes)

    def test_cp932_limits_leave_parse_and_original_roundtrip_evidence_available(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'scene.dat'
            raw = member('#太郎&1"本文"\r\n')
            path.write_bytes(raw)
            result = main(dict(operation='verify', input=str(path)))
            self.assertFalse(result['success'])
            self.assertEqual(result['parseStatus'], 'passed')
            self.assertEqual(result['roundtripStatus'], 'passed')
            self.assertEqual(result['translationStatus'], 'failed')
            self.assertEqual(result['diagnostic']['errorType'], 'UnicodeEncodeError')
            self.assertEqual(path.read_bytes(), raw)
            result = main(dict(operation='verify', input=str(path), checkTranslation=False))
            self.assertTrue(result['success'])
            self.assertEqual(result['translationStatus'], 'skipped')
            output = Path(directory) / 'changed.dat'
            result = main(dict(operation='build', input=str(path), outputFile=str(output),
                               translations={'dialogue|1|0': '変更後の本文'}))
            self.assertTrue(result['success'])
            self.assertEqual(ScriptText(output.read_bytes()).fields[0][1], '変更後の本文')
            with self.assertRaises(FileExistsError):
                main(dict(operation='build', input=str(path), outputFile=str(output)))

    def test_invalid_profiles_and_damaged_headers_remain_parse_failures(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'scene.dat'
            path.write_bytes(member('&1"本文"\r\n'))
            result = main(dict(operation='verify', input=str(path), profile=dict(version=2, key='wrong')))
            self.assertEqual(result['parseStatus'], 'failed')
            self.assertEqual(result['roundtripStatus'], 'not-run')
            path.write_bytes(b'not a complete script')
            result = main(dict(operation='verify', input=str(path)))
            self.assertFalse(result['success'])
            self.assertEqual(result['parseStatus'], 'failed')


if __name__ == '__main__':
    unittest.main()
