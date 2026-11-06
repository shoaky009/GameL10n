"""Unchanged template builds retain bytes while changed scripts use the parser."""
import importlib.util
import json
from pathlib import Path
import pickle
import sys
import tempfile
import unittest
from unittest.mock import patch
import zlib


ROOT = Path(__file__).resolve().parents[3]
SPEC = importlib.util.spec_from_file_location(
    'template_under_test', Path(__file__).with_name('script_template.py'),
    submodule_search_locations=[str(ROOT / 'adapters/components')])
TEMPLATE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = TEMPLATE
SPEC.loader.exec_module(TEMPLATE)


def rpa_file(path, members):
    data, entries = bytearray(b' ' * 34), {}
    for name, content in members.items():
        entries[name] = [(len(data), len(content))]
        data.extend(content)
    offset = len(data)
    data.extend(zlib.compress(pickle.dumps(entries, protocol=3)))
    data[:34] = f'RPA-3.0 {offset:016x} 00000000\n'.encode('ascii')
    path.write_bytes(data)


class ScriptTemplateTest(unittest.TestCase):
    def test_validate_reuses_host_extractions_and_falls_back_without_context(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, output, package = root / 'source', root / 'output', root / 'package'
            source.mkdir()
            output.mkdir()
            (package / 'tools').mkdir(parents=True)
            config = dict(schemaVersion=1, templateVersion=1, templateId='fixture', format='Renpy',
                          archiveComponent=None, scriptExtensions=['.rpy'], resourcePatterns=['**/*.rpy'])
            (package / 'tools/adapter-template.json').write_text(json.dumps(config), encoding='utf-8')
            (source / 'script.rpy').write_text('原文', encoding='utf-8')
            (output / 'script.rpy').write_text('中文译文', encoding='utf-8')
            original = dict(id='script.rpy:0', resourcePath='script.rpy', locator='line:0',
                            sourceText='原文', context='fixture', kind='dialogue')
            modified = dict(original, sourceText='中文译文')
            context = dict(schemaVersion=1, sourceDirectory=str(source), outputDirectory=str(output),
                           resourceFiles=['script.rpy'], outputFiles=['script.rpy'],
                           originalUnits=[original], outputUnits=[modified])
            context_path = root / 'context.json'
            context_path.write_text(json.dumps(context), encoding='utf-8')
            request = dict(protocolVersion=1, operation='validate', sourceDirectory=str(source),
                           outputDirectory=str(output), adapterDirectory=str(package), resourceFiles=['script.rpy'],
                           translations={original['id']: modified['sourceText']})
            with (patch.object(TEMPLATE, 'containers', side_effect=AssertionError('duplicate unpack')),
                  patch.object(TEMPLATE, 'extract', side_effect=AssertionError('duplicate extraction'))):
                result = TEMPLATE.execute(dict(request, validationContextFile=str(context_path)))['validation']
                self.assertTrue(result['passed'], result)
                context['outputUnits'] = []
                context_path.write_text(json.dumps(context), encoding='utf-8')
                result = TEMPLATE.execute(dict(request, validationContextFile=str(context_path)))['validation']
                self.assertFalse(result['passed'])
                self.assertEqual({issue['code'] for issue in result['issues']}, {'count', 'text'})

            def native_parser(request, format, **options):
                text = (Path(options['sourceDirectory']) / 'script.rpy').read_text(encoding='utf-8')
                return dict(extraction=dict(units=[dict(original, sourceText=text)], warnings=[]))

            with patch.object(TEMPLATE, 'parser', side_effect=native_parser) as parser:
                self.assertTrue(TEMPLATE.execute(request)['validation']['passed'])
                self.assertEqual(parser.call_count, 2)
                (output / 'script.rpy').write_text('损坏的译文', encoding='utf-8')
                self.assertFalse(TEMPLATE.execute(request)['validation']['passed'])

    def test_original_build_skips_writers_but_changed_and_invalid_ids_keep_their_contracts(self):
        pin = json.loads((ROOT / 'third_party/engine-tools/current.json').read_text(encoding='utf-8-sig'))
        library = str(ROOT / 'third_party/engine-tools/references' / pin['revision'] / 'library.zip')
        with tempfile.TemporaryDirectory() as directory, patch.object(sys, 'path', [library, *sys.path]):
            root = Path(directory)

            def native_parser(request, format, **options):
                source = Path(options['sourceDirectory'])
                files = options['resourceFiles']
                texts = {file: (source / file).read_text(encoding='utf-8') for file in files}
                if options['operation'] == 'extract':
                    return dict(extraction=dict(units=[dict(id=file + ':0', resourcePath=file,
                        locator='line:0', sourceText=text, context='fixture', kind='dialogue')
                        for file, text in texts.items()], warnings=[]))
                self.assertEqual(options['operation'], 'build')
                self.assertFalse(options['translations'].keys() - {file + ':0' for file in files})
                output = Path(options['outputDirectory'])
                for file, text in texts.items():
                    path = output / file
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_text(options['translations'].get(file + ':0', text), encoding='utf-8')
                return dict(build=dict(files=files, warnings=[]))

            for packed in (True, False):
                with self.subTest(packed=packed):
                    case = root / ('packed' if packed else 'loose')
                    source, package = case / 'source', case / 'package'
                    source.mkdir(parents=True)
                    (package / 'tools').mkdir(parents=True)
                    config = dict(schemaVersion=1, templateVersion=1, templateId='fixture', format='Renpy',
                                  archiveComponent='archive.rpa3' if packed else None,
                                  scriptExtensions=['.rpy'], resourcePatterns=['**/*.rpa' if packed else '**/*.rpy'])
                    (package / 'tools/adapter-template.json').write_text(json.dumps(config), encoding='utf-8')
                    if packed:
                        rpa_file(source / 'first.rpa', {'script.rpy': '原文一'.encode('utf-8')})
                        rpa_file(source / 'second.rpa', {'script.rpy': '原文二'.encode('utf-8')})
                        rpa_file(source / 'media.rpa', {'picture.bin': bytes(range(256))})
                        files = ['first.rpa', 'second.rpa', 'media.rpa']
                    else:
                        files = ['first.rpy', 'second.rpy']
                        for file, text in zip(files, ('原文一', '原文二')):
                            (source / file).write_text(text, encoding='utf-8')
                    request = dict(protocolVersion=1, sourceDirectory=str(source), adapterDirectory=str(package),
                                   resourceFiles=files)
                    originals = {file: (source / file).read_bytes() for file in files}
                    with patch.object(TEMPLATE, 'parser', side_effect=native_parser) as parser, \
                            patch.object(TEMPLATE.archive, 'execute', wraps=TEMPLATE.archive.execute) as archive:
                        extracted = TEMPLATE.execute(dict(request, operation='extract'))
                        self.assertEqual(extracted['validationContextVersion'], 1)
                        units = extracted['extraction']['units']
                        self.assertEqual(len(units), 2)
                        for name, translations in [('empty', {}), ('identity', {u['id']: u['sourceText'] for u in units})]:
                            parser.reset_mock()
                            archive.reset_mock()
                            output = case / name
                            result = TEMPLATE.execute(dict(request, operation='build', outputDirectory=str(output),
                                                           translations=translations))
                            self.assertEqual(result['build']['files'], files)
                            self.assertTrue(all(call.kwargs['operation'] == 'extract' for call in parser.call_args_list))
                            self.assertTrue(all(call.args[0]['operation'] == 'extract' for call in archive.call_args_list))
                            self.assertEqual({file: (output / file).read_bytes() for file in files}, originals)
                            validation = TEMPLATE.execute(dict(request, operation='validate', outputDirectory=str(output),
                                                               translations=translations))['validation']
                            self.assertTrue(validation['passed'], validation)

                        parser.reset_mock()
                        archive.reset_mock()
                        output = case / 'changed'
                        translations = {units[0]['id']: '中文译文', units[1]['id']: units[1]['sourceText']}
                        result = TEMPLATE.execute(dict(request, operation='build', outputDirectory=str(output),
                                                       translations=translations))
                        builds = [call for call in parser.call_args_list if call.kwargs['operation'] == 'build']
                        self.assertEqual(len(builds), 1)
                        self.assertEqual(list(builds[0].kwargs['translations'].values()), ['中文译文'])
                        rebuilds = [call for call in archive.call_args_list if call.args[0]['operation'] == 'rebuild']
                        self.assertEqual(len(rebuilds), int(packed))
                        self.assertEqual(result['build']['files'], files)
                        self.assertNotEqual((output / files[0]).read_bytes(), originals[files[0]])
                        self.assertTrue(all((output / file).read_bytes() == originals[file] for file in files[1:]))
                        validation = TEMPLATE.execute(dict(request, operation='validate', outputDirectory=str(output),
                                                           translations=translations))['validation']
                        self.assertTrue(validation['passed'], validation)

                        parser.reset_mock()
                        archive.reset_mock()
                        output = case / 'unknown'
                        with self.assertRaisesRegex(ValueError, 'ID'):
                            TEMPLATE.execute(dict(request, operation='build', outputDirectory=str(output),
                                                  translations={'unknown': units[0]['sourceText']}))
                        self.assertFalse(output.exists())
                        self.assertTrue(all(call.kwargs['operation'] == 'extract' for call in parser.call_args_list))
                        self.assertTrue(all(call.args[0]['operation'] == 'extract' for call in archive.call_args_list))

                        with self.assertRaises(ValueError):
                            TEMPLATE.execute(dict(request, operation='build', outputDirectory=str(source), translations={}))
                        output = case / 'existing'
                        output.mkdir()
                        (output / files[0]).write_bytes(b'keep existing output')
                        with self.assertRaises(FileExistsError):
                            TEMPLATE.execute(dict(request, operation='build', outputDirectory=str(output), translations={}))
                        self.assertEqual((output / files[0]).read_bytes(), b'keep existing output')
                        self.assertEqual({file: (source / file).read_bytes() for file in files}, originals)


if __name__ == '__main__':
    unittest.main()
