"""Yuris protocol regressions; pass --executable with a built native parser."""
import argparse
import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest


REPO = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO / 'adapters' / 'components'))
from sjis_tunnel import Tunnel, SLOTS, _native_bytes

EXECUTABLE = None
COMMANDS = [
    ('WORD', ['STR']), ('RETURNCODE', ['CODE']), ('_', ['EVAL']),
    ('GOSUB', ['NAME', 'ARG']), ('CG', ['NAME']),
    ('DIALOG', ['CAPTION', 'STR']), ('FONT', ['NAME']), ('LET', ['?', '?']),
]


def command_table():
    data = bytearray(struct.pack('<4s3I', b'YSCM', 494, len(COMMANDS), 0))
    for name, attributes in COMMANDS:
        data.extend(name.encode('cp932') + b'\0' + bytes([len(attributes)]))
        for attribute in attributes:
            data.extend(attribute.encode('cp932') + b'\0\0\0')
    return bytes(data)


def raw(text, ident=0):
    return ident, 0, text.encode('cp932')


def expression(text, ident=0):
    value = ('"' + text + '"').encode('cp932')
    return ident, 3, b'M' + struct.pack('<H', len(value)) + value


def scenario(commands):
    instructions, descriptors, values = bytearray(), bytearray(), bytearray()
    for name, attributes in commands:
        command = next(i for i, (candidate, _) in enumerate(COMMANDS) if name == candidate)
        instructions.extend(struct.pack('<BBH', command, len(attributes), 0))
        for ident, typ, value in attributes:
            descriptors.extend(struct.pack('<HHII', ident, typ, len(value), len(values)))
            values.extend(value)
    return struct.pack('<4s7I', b'YSTB', 494, len(commands), len(instructions),
                       len(descriptors), len(values), 0, 0) + instructions + descriptors + values


def config(caption):
    data = bytearray(78)
    data[:8] = b'YSCF' + struct.pack('<I', 494)
    struct.pack_into('<h', data, 76, len(caption))
    return data + caption


def attribute_values(data):
    """Read fixture fields to compare their stored bytes, without decoding text."""
    instructions, descriptors = struct.unpack_from('<II', data, 12)
    start = 32 + instructions
    values = start + descriptors
    return [data[values + offset:values + offset + length]
            for _, _, length, offset in struct.iter_unpack('<HHII', data[start:values])]


class YurisTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='yuris-test-', dir=REPO / 'target')
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.source = self.root / 'source'
        self.source.mkdir()
        (self.source / 'ysc.ybn').write_bytes(command_table())

    def invoke(self, operation, source=None, success=True, **parameters):
        request = dict(protocolVersion=1, operation=operation, format='Yuris',
                       sourceDirectory=str(source or self.source))
        request.update(parameters)
        process = subprocess.run([str(EXECUTABLE)], input=json.dumps(request, ensure_ascii=True),
                                 encoding='utf-8', stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                 timeout=60, creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0)
        result = json.loads(process.stdout)
        self.assertEqual(result['success'], success, result)
        self.assertEqual(process.returncode, 0 if success else 1, process.stderr)
        return result

    def units(self, source=None, **parameters):
        return self.invoke('extract', source=source, **parameters)['extraction']['units']

    def test_optional_sources_keep_units_and_expose_field_origins(self):
        (self.source / 'scene.ybn').write_bytes(scenario([
            ('WORD', [raw('主人公')]), ('_', [expression('（原文）')]),
            ('DIALOG', [expression('確認'), expression('終了しますか', 1)]),
            ('FONT', [expression('ＭＳ ゴシック')]),
            ('LET', [expression('内部甲'), expression('内部乙', 1)]),
        ]))
        caption = 'ウィンドウ'.encode('cp932')
        config = bytearray(78)
        config[:8] = b'YSCF' + struct.pack('<I', 494)
        struct.pack_into('<h', config, 76, len(caption))
        (self.source / 'yscfg.ybn').write_bytes(config + caption)

        default = self.units()
        self.assertTrue(all('yurisSource' not in unit for unit in default))
        self.assertEqual(default, self.units(includeYurisSources=False))
        enriched = self.units(includeYurisSources=True)
        self.assertEqual(default, [{k: v for k, v in unit.items() if k != 'yurisSource'} for unit in enriched])
        self.assertEqual(len(default), 8)
        name, message = enriched[:2]
        self.assertEqual([name['yurisSource']['role'], message['yurisSource']['role']], ['name', 'dialogue'])
        self.assertEqual(name['yurisSource']['groupIndex'], message['yurisSource']['groupIndex'])
        self.assertEqual(name['yurisSource']['attributes'], [
            dict(commandIndex=0, command='WORD', attributeIndex=0, attributeId=0, attribute='STR', attributeType=0),
            dict(commandIndex=1, command='_', attributeIndex=1, attributeId=0, attribute='EVAL', attributeType=3),
        ])
        self.assertEqual(enriched[3]['yurisSource']['attributes'][0],
                         dict(commandIndex=2, command='DIALOG', attributeIndex=3,
                              attributeId=1, attribute='STR', attributeType=3))
        self.assertEqual(enriched[6]['yurisSource']['attributes'][0]['attribute'], '?')
        self.assertEqual(enriched[-1]['yurisSource'], dict(groupIndex=0, role='caption', attributes=[]))
        # Field selection uses the same extraction, including explicit evidence for exclusions.
        accepted = [unit for unit in enriched if all(
            attribute['command'] not in {'FONT', 'LET'} for attribute in unit['yurisSource']['attributes'])]
        self.assertEqual(len(accepted), 5)

    def test_named_message_edits_preserve_grouping(self):
        originals = ['主人公（考え中）', '主人公「本文」', '主人公『本文』']
        for original in originals:
            with self.subTest(original=original):
                script = self.source / 'scene.ybn'
                data = scenario([('WORD', [raw(original)]), ('DIALOG', [expression('未変更')])])
                script.write_bytes(data)
                before = self.units()
                self.assertEqual(len(before), 3)
                message = before[1]
                texts = ['前置' + message['sourceText'], message['sourceText'] + '後置',
                         '新しい文章', '「独立した引用」', '（新しい考え）']
                for number, text in enumerate(texts):
                    with self.subTest(text=text):
                        output = self.root / ('out-' + str(number))
                        self.invoke('build', outputDirectory=str(output), translations={message['id']: text})
                        expected = [dict(unit, sourceText=text) if unit['id'] == message['id'] else unit for unit in before]
                        self.assertEqual(expected, self.units(source=output))
                        self.assertEqual(script.read_bytes(), data)

    def test_mismatch_reports_two_bounded_locators(self):
        (self.source / 'scene.ybn').write_bytes(scenario([('WORD', [raw('元の文')])]))
        unit = self.units()[0]
        text = '名前' * 100 + '「' + '本文' * 100 + '」'
        result = self.invoke('build', success=False, outputDirectory=str(self.root / 'out'),
                             translations={unit['id']: text})
        error = result['error']
        self.assertIn('scene.ybn', error)
        self.assertIn('string:0 difference=kind+text', error)
        self.assertIn('string:1 difference=extra', error)
        self.assertEqual(error.count('string:'), 2)
        self.assertIn('expected=', error)
        self.assertIn('actual=', error)
        self.assertIn('…', error)
        self.assertLess(len(error), 700)
        self.assertEqual(result['diagnostic']['resourcePath'], 'scene.ybn')

    def test_default_cp932_still_rejects_chinese(self):
        data = scenario([('WORD', [raw('原文')])])
        (self.source / 'scene.ybn').write_bytes(data)
        unit = self.units()[0]
        result = self.invoke('build', success=False, outputDirectory=str(self.root / 'out'),
                             translations={unit['id']: '这是一段简体中文'})
        self.assertIn('原始编码', result['error'])
        self.assertEqual((self.source / 'scene.ybn').read_bytes(), data)

    def test_tunnel_build_preserves_id_grouping_and_nontext_bytes(self):
        data = scenario([
            ('WORD', [raw('主人公「原文')]), ('_', [expression('後半」')]),
            ('RETURNCODE', [(0, 0, b'\0\xEF\xF0\0')]),
            ('WORD', [(0, 0, '据置'.encode('cp932') + b'\xEF\xF2' + '終'.encode('cp932'))]),
            ('DIALOG', [expression('確認'), expression('終了しますか', 1)]),
            ('CG', [(0, 0, b'\0\x81\x01\xFF')]),
        ])
        (self.source / 'scene.ybn').write_bytes(data)
        (self.source / 'untouched.ybn').write_bytes(scenario([('WORD', [raw('未変更')])]))
        (self.source / 'yscfg.ybn').write_bytes(config('ウィンドウ'.encode('cp932')))
        before = self.units(includeYurisSources=True)
        replacements = {'主人公': '主角', '原文後半': '这是一段中文台词',
                        '確認': '游戏选项', 'ウィンドウ': '中文窗口'}
        translations = {unit['id']: replacements[unit['sourceText']]
                        for unit in before if unit['sourceText'] in replacements}
        tunnel = Tunnel()
        for text in translations.values():
            tunnel.encode(text)
        mapping = tunnel.serialize().decode('utf-16-le')
        options = dict(source='cp932', output='sjis-tunnel', mapping=mapping)
        output = self.root / 'out'
        built = self.invoke('build', outputDirectory=str(output), translations=translations,
                            yurisEncoding=options)
        self.assertTrue(any('尚未实机验证' in warning for warning in built['build']['warnings']))
        after = self.units(source=output, includeYurisSources=True,
                           yurisEncoding=dict(source='sjis-tunnel', mapping=mapping))
        public = lambda unit: {key: value for key, value in unit.items() if key != 'yurisSource'}
        self.assertEqual([public(unit) for unit in after],
                         [dict(public(unit), sourceText=translations.get(unit['id'], unit['sourceText']))
                          for unit in before])
        self.assertEqual([(unit['yurisSource']['groupIndex'], unit['yurisSource']['role']) for unit in after],
                         [(unit['yurisSource']['groupIndex'], unit['yurisSource']['role']) for unit in before])
        values, original = attribute_values((output / 'scene.ybn').read_bytes()), attribute_values(data)
        self.assertEqual(values[0], tunnel.encode('主角「这是一段中文台词」'))
        self.assertEqual(values[1], b'')  # Existing WORD + _ groups consolidate into their first field.
        self.assertEqual(values[4], b'M' + struct.pack('<H', len(tunnel.encode('`游戏选项`')))
                         + tunnel.encode('`游戏选项`'))
        for index in (2, 3, 5, 6):
            self.assertEqual(values[index], original[index])
        self.assertEqual((output / 'yscfg.ybn').read_bytes(), config(tunnel.encode('中文窗口')))
        for name in ('ysc.ybn', 'untouched.ybn'):
            self.assertEqual((output / name).read_bytes(), (self.source / name).read_bytes())
        self.assertFalse((output / 'sjis_ext.bin').exists())
        self.assertEqual((self.source / 'scene.ybn').read_bytes(), data)
        validated = self.invoke('validate', outputDirectory=str(output), yurisEncoding=options)['validation']
        self.assertTrue(validated['passed'], validated)
        self.assertEqual(validated['runtimeStatus'], 'not-tested')
        probed = self.invoke('probe', source=output, yurisEncoding=dict(source='sjis-tunnel', mapping=mapping))
        self.assertIn('scene.ybn', probed['probe']['requiredFiles'])
        copied = self.root / 'copy'
        self.invoke('build', source=output, outputDirectory=str(copied),
                    yurisEncoding=dict(source='sjis-tunnel', output='sjis-tunnel', mapping=mapping))
        for path in output.iterdir():
            self.assertEqual(path.read_bytes(), (copied / path.name).read_bytes())
        restored = self.root / 'cp932'
        reverse_options = dict(source='sjis-tunnel', output='cp932', mapping=mapping)
        self.invoke('build', source=output, outputDirectory=str(restored), yurisEncoding=reverse_options,
                    translations={unit['id']: unit['sourceText'] for unit in before if unit['id'] in translations})
        self.assertEqual(self.units(source=restored), [public(unit) for unit in before])
        self.assertTrue(self.invoke('validate', source=output, outputDirectory=str(restored),
                                   yurisEncoding=reverse_options)['validation']['passed'])

    def test_python_tunnel_bytes_match_canonical_aliases_and_reserved_range(self):
        texts = ['简体汉语〜¥‖−¢£¬\ue000\uf8f0',
                 ''.join(chr(0xE000 + index) for index in (0, 7, 8, 55, 57, 58, 1797, 1798, 2726, SLOTS - 1)),
                 ''.join(chr(code) for code in range(1, 0x10000) if _native_bytes(chr(code)) is not None)]
        for number, text in enumerate(texts):
            with self.subTest(case=number):
                data = scenario([('WORD', [raw('元の文')])])
                (self.source / 'scene.ybn').write_bytes(data)
                unit = self.units()[0]
                tunnel = Tunnel() if number != 1 else Tunnel(
                    ''.join(chr(0xE000 + index) for index in range(SLOTS)).encode('utf-16-le'))
                expected = tunnel.encode(text)
                mapping = tunnel.serialize().decode('utf-16-le')
                output = self.root / ('cross-' + str(number))
                self.invoke('build', outputDirectory=str(output), translations={unit['id']: text},
                            yurisEncoding=dict(output='sjis-tunnel', mapping=mapping))
                actual = attribute_values((output / 'scene.ybn').read_bytes())[0]
                self.assertEqual(actual, expected)
                self.assertEqual(tunnel.decode(actual), text)
                units = self.units(source=output, yurisEncoding=dict(source='sjis-tunnel', mapping=mapping))
                self.assertEqual(units, [dict(unit, sourceText=text)])

    def test_tunnel_configuration_and_missing_mapping_rejected(self):
        (self.source / 'scene.ybn').write_bytes(scenario([('WORD', [raw('原文')])]))
        unit = self.units()[0]
        bad_options = [dict(output='other'), dict(source=None), dict(output='sjis-tunnel'),
                       dict(source='sjis-tunnel'), dict(mapping='汉'),
                       dict(output='sjis-tunnel', mapping='汉汉'),
                       dict(output='sjis-tunnel', mapping='\0'),
                       dict(output='sjis-tunnel', mapping='\ud800'),
                       dict(output='sjis-tunnel', mapping='😀'),
                       dict(output='sjis-tunnel', mapping=''.join(chr(0xE000 + i) for i in range(SLOTS + 1))),
                       dict(output='sjis-tunnel', mapping='', unexpected=True)]
        for options in bad_options:
            with self.subTest(options=str(options)[:100]):
                self.invoke('extract', success=False, yurisEncoding=options)
        self.invoke('extract', success=False, format='Renpy', yurisEncoding={})
        missing = self.invoke('build', success=False, outputDirectory=str(self.root / 'missing'),
                              translations={unit['id']: '汉语'},
                              yurisEncoding=dict(output='sjis-tunnel', mapping='汉'))
        self.assertIn('mapping', missing['diagnostic']['inner']['message'])
        # A request does not keep either the mapping or the codec from the preceding request.
        self.assertEqual(self.units(), [unit])
        self.assertEqual(self.units(yurisEncoding={}), [unit])
        self.invoke('build', success=False, outputDirectory=str(self.root / 'default'),
                    translations={unit['id']: '汉语'})

    def test_tunnel_input_rejects_missing_and_malformed_pairs(self):
        for raw_bytes in (b'\x81\x02', b'\x81\x09', b'\x81', b'\x81\0'):
            with self.subTest(raw_bytes=raw_bytes):
                (self.source / 'scene.ybn').write_bytes(scenario([('WORD', [(0, 0, raw_bytes)])]))
                self.invoke('extract', success=False,
                            yurisEncoding=dict(source='sjis-tunnel', mapping='汉'))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--executable', type=Path, required=True)
    arguments, remaining = parser.parse_known_args()
    EXECUTABLE = arguments.executable.resolve()
    unittest.main(argv=[__file__, *remaining])
