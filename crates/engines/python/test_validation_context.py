"""Verification contexts reuse outer extractions without leaking into nested tools."""
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[3]
SPEC = importlib.util.spec_from_file_location('validation_host', ROOT / 'adapters/components/gamel10n_host.py')
HOST = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(HOST)


class ValidationContextTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.context_path = self.root / 'context.json'
        self.request = dict(operation='validate', sourceDirectory=str(self.root / 'source'),
                            outputDirectory=str(self.root / 'output'), resourceFiles=['scripts.dat'],
                            translations={'stable-id': '内联译文'}, validationContextFile=str(self.context_path))
        self.unit = dict(id='stable-id', resourcePath='scripts.dat', locator='record:1',
                         sourceText='原文', context='保留上下文', kind='dialogue')
        self.context = dict(schemaVersion=1, sourceDirectory=self.request['sourceDirectory'],
                            outputDirectory=self.request['outputDirectory'], resourceFiles=['scripts.dat'],
                            outputFiles=['scripts.dat', 'encoding.map'], originalUnits=[self.unit],
                            outputUnits=[dict(self.unit, sourceText='文件译文')])
        self.write_context()

    def write_context(self):
        self.context_path.write_text(json.dumps(self.context, ensure_ascii=False), encoding='utf-8')

    def test_absent_context_keeps_legacy_validation_available(self):
        self.assertIsNone(HOST.validation_context({}))

    def test_context_preserves_units_and_loads_current_translations_file(self):
        translations = self.root / 'translations.json'
        translations.write_text(json.dumps({'stable-id': '文件译文'}), encoding='utf-8-sig')
        context = HOST.validation_context(dict(self.request, translationsFile=str(translations)))
        self.assertEqual(context['originalUnits'], [self.unit])
        self.assertEqual(context['outputFiles'], ['scripts.dat', 'encoding.map'])
        self.assertEqual(context['translations'], {'stable-id': '文件译文'})
        self.assertEqual(HOST.validation_context(self.request)['translations'], {'stable-id': '内联译文'})

    def test_context_rejects_other_operations_versions_and_resource_bindings(self):
        for changes in (dict(operation='build'), dict(sourceDirectory=str(self.root / 'other-source')),
                        dict(outputDirectory=str(self.root / 'other-output')), dict(resourceFiles=['other.dat'])):
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                HOST.validation_context(dict(self.request, **changes))
        self.context['schemaVersion'] = 2
        self.write_context()
        with self.assertRaisesRegex(ValueError, '版本'):
            HOST.validation_context(self.request)

    def test_nested_tools_drop_outer_context_and_do_not_advertise_their_capability(self):
        request = dict(self.request, buildMode='overlay', outputFiles=['scripts.patch.dat'],
                       buildHints=dict(patchPath='scripts.patch.dat'),
                       toolPaths=dict(vnTextPatchCommand=['native-parser'], hostCommand=['native-host']))
        response = dict(protocolVersion=1, success=True, validationContextVersion=1)
        for invoke in (lambda: HOST.parser(request, 'Renpy', sourceDirectory='unpacked'),
                       lambda: HOST.invoke('component', request)):
            with self.subTest(invoke=invoke), patch.object(HOST.subprocess, 'run', return_value=
                    subprocess.CompletedProcess([], 0, json.dumps(response), '')) as run:
                result = invoke()
                payload = json.loads(run.call_args.kwargs['input'])
                for key in ('validationContextFile', 'buildMode', 'outputFiles', 'buildHints'):
                    self.assertNotIn(key, payload)
                self.assertNotIn('validationContextVersion', result)

    def test_overlay_context_binds_the_actual_patch_files(self):
        request = dict(self.request, buildMode='overlay', outputFiles=self.context['outputFiles'])
        self.assertIsNotNone(HOST.validation_context(request))
        with self.assertRaisesRegex(ValueError, '覆盖包清单'):
            HOST.validation_context(dict(request, outputFiles=['other.patch.dat']))


if __name__ == '__main__':
    unittest.main()
