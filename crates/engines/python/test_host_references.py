"""Reference-tool regression checks against the pinned catalog and bounded synthetic guides.

Run with target/runtime/python/python.exe -I -X utf8 crates/engines/python/test_host_references.py.
"""
import hashlib
import json
from pathlib import Path
import runpy
import tempfile
import unittest
import zipfile


ROOT = Path(__file__).resolve().parents[3]
HOST = runpy.run_path(str(Path(__file__).with_name('host.py')))
EXECUTE = HOST['execute']


class ReferenceToolsTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        pin = json.loads((ROOT / 'third_party/engine-tools/current.json').read_text(encoding='utf-8-sig'))
        cls.revision = pin['revision']
        cls.library = ROOT / 'third_party/engine-tools/references' / cls.revision / 'library.zip'
        (ROOT / 'target').mkdir(exist_ok=True)
        cls.temporary = tempfile.TemporaryDirectory(prefix='reference-tests-', dir=ROOT / 'target')
        cls.addClassCleanup(cls.temporary.cleanup)
        cls.directory = Path(cls.temporary.name)

    def call(self, operation='search_engine_references', **args):
        return EXECUTE(dict(library=str(self.library), revision=self.revision, operation=operation, **args))

    def fixture(self, engines, files=None):
        archive = self.directory / (self.id().rsplit('.', 1)[-1] + '.zip')
        with zipfile.ZipFile(archive, 'w', compression=zipfile.ZIP_DEFLATED) as source:
            source.writestr('catalog/engines.json', json.dumps(dict(engines=engines), ensure_ascii=False))
            for path, content in (files or {}).items():
                source.writestr(path, content)
        return str(archive)

    def engine(self, identity, **extra):
        return dict(id=identity, title=identity, aliases=[], document=f'engines/{identity}.md',
                    python_references=[], **extra)

    def test_real_multi_term_search_returns_alicesoft_only(self):
        result = self.call(query='alice ain system4')
        self.assertEqual(result['status'], 'queried')
        self.assertEqual(result['totalMatches'], 1)
        self.assertEqual([item['id'] for item in result['matches']], ['alicesoft'])
        match = result['matches'][0]
        self.assertTrue(match['documentUrl'].endswith(self.revision + '/engines/alicesoft.md'))
        self.assertEqual(match['publicSymbols']['python/archives/ain.py'], ['unpack', 'repack'])
        self.assertEqual(len(match['sourceUrls']), 2)
        self.assertEqual(len(result['contentSha256']), 64)
        self.assertTrue(result['reused'])
        self.assertFalse(result['more'])
        self.assertLess(len(json.dumps(result, ensure_ascii=False)), 6000)
        self.assertNotIn('engines', result)
        self.assertNotIn('paths', result)

    def test_real_aliases_and_engine_guidance_are_preserved(self):
        for query, identity in [('system4', 'alicesoft'), ('System4.0', 'alicesoft'),
                                ('AIN', 'alicesoft'), ('Hxv4', 'kirikiri'), ('PSB SCN', 'kirikiri')]:
            with self.subTest(query=query):
                result = self.call(query=query)
                self.assertEqual(result['matches'][0]['id'], identity)
        result = self.call(query='Hxv4')
        self.assertEqual(result['matches'][0]['readySupport']['component'], 'kirikiri-scn-text')
        self.assertIn('Kirikiri 脚本编码', [item['topic'] for item in result['guidance']])
        self.assertIn('中文编码与字符映射', [item['topic'] for item in self.call(query='system4')['guidance']])

    def test_no_match_and_generic_queries_remain_bounded(self):
        for query in ['doesnotexist12345', 'archive engine python format scripts references tools']:
            with self.subTest(query=query):
                result = self.call(query=query)
                self.assertEqual(result['matches'], [])
                self.assertEqual(result['totalMatches'], 0)
                self.assertFalse(result['more'])
                self.assertTrue(result['hint'])
                self.assertLess(len(json.dumps(result, ensure_ascii=False)), 3000)

    def test_valkyria_guidance_is_available_on_search_and_direct_inspection_only(self):
        result = self.call(query='Valkyria SDT')
        self.assertEqual(result['matches'][0]['id'], 'valkyria')
        self.assertIn(HOST['VALKYRIA_GUIDANCE'], [item['note'] for item in result['guidance']])
        inspected = self.call(operation='inspect_engine_reference', engine='valkyria', length=256)
        self.assertEqual(inspected['runtimeGuidance'], HOST['VALKYRIA_GUIDANCE'])
        unrelated = self.call(query='kirikiri')
        self.assertNotIn(HOST['VALKYRIA_GUIDANCE'], [item['note'] for item in unrelated['guidance']])

    def test_invalid_queries_use_original_validation(self):
        for query in ['', '  ', 'x', '---', 'a' * 129]:
            with self.subTest(query=query), self.assertRaises(ValueError):
                self.call(query=query)
        with self.assertRaisesRegex(ValueError, 'Missing string argument: query'):
            self.call()

    def test_siglus_uses_bundled_capability_without_catalog(self):
        for query in ['Siglus', 'SiglusEngine', 'Scene.pck']:
            with self.subTest(query=query):
                result = EXECUTE(dict(operation='search_engine_references', query=query))
                self.assertEqual(result['status'], 'bundled')
                self.assertEqual(result['capability']['id'], 'engine.siglus')
                self.assertIn('siglus_ssu/GEI.py', result['capability']['sources'])

    def test_ranking_exact_alias_title_terms_weights_and_top_six(self):
        items = [self.engine(name) for name in ['only', 'x', 'alicesoft', 'alice', 'ain', 'y', 'z', 'alice-ain']]
        changes = dict(only=dict(aliases=['ain']), x=dict(title='alice ain module'),
                       alicesoft=dict(python_references=['python/engines/ain.py']),
                       alice=dict(aliases=['ain']), ain=dict(aliases=['alice']),
                       y=dict(title='alice ain'), z=dict(aliases=['alice ain']))
        for item in items:
            item.update(changes.get(item['id'], {}))
        archive = self.fixture(items)
        result = EXECUTE(dict(operation='search_engine_references', library=archive, query='alice ain'))
        self.assertEqual([item['id'] for item in result['matches']], ['alice-ain', 'z', 'y', 'ain', 'alice', 'alicesoft'])
        self.assertEqual(result['totalMatches'], 8)
        self.assertTrue(result['more'])

    def test_real_engine_id_defaults_to_guide_and_retains_apis(self):
        result = self.call('inspect_engine_reference', engine='alicesoft')
        with zipfile.ZipFile(self.library) as source:
            guide = source.read('engines/alicesoft.md').decode('utf-8-sig')
        self.assertEqual(result['path'], 'engines/alicesoft.md')
        self.assertEqual(result['content'], guide)
        self.assertEqual(result['publicSymbols']['python/engines/alicesoft.py'],
                         ['AinText', 'AinTable', 'cipher', 'read_ain', 'rewrite_ain'])
        self.assertEqual(result['pythonModules'], ['python/archives/ain.py', 'python/engines/alicesoft.py'])
        self.assertEqual(result['revision'], self.revision)
        self.assertFalse(result['more'])
        self.assertLess(len(json.dumps(result, ensure_ascii=False)), 6000)
        self.assertEqual(self.call('inspect_engine_reference', engine='ALICESOFT')['engine'], 'ALICESOFT')
        for identity in ['alice', 'ain', 'System4', '']:
            with self.subTest(identity=identity), self.assertRaisesRegex(ValueError, '精确引擎 ID'):
                self.call('inspect_engine_reference', engine=identity)

    def test_default_page_utf16_offsets_context_and_query_miss(self):
        content = 'A' * 300 + '😀' + 'Needle' + 'B' * 20000
        archive = self.fixture([self.engine('sample')], {'engines/sample.md': content})
        request = dict(operation='inspect_engine_reference', library=archive, engine='sample')
        result = EXECUTE(request)
        self.assertEqual(result['totalCharacters'], 20308)
        self.assertEqual(result['nextOffset'], 3200)
        self.assertTrue(result['more'])
        self.assertEqual(EXECUTE(dict(request, length=8000))['nextOffset'], 8000)
        result = EXECUTE(dict(request, query='needle', length=1000))
        self.assertEqual((result['offset'], result['nextOffset']), (62, 1062))
        self.assertTrue(result['queryFound'])
        self.assertIn('Needle', result['content'])
        following = EXECUTE(dict(request, offset=1062, length=16000))
        self.assertEqual((following['offset'], following['nextOffset']), (1062, 17062))
        self.assertEqual(len(following['content']), 16000)
        missed = EXECUTE(dict(request, query='absent', offset=100, length=20))
        self.assertEqual((missed['content'], missed['offset'], missed['nextOffset'], missed['more']), ('', 100, 120, True))
        self.assertFalse(missed['queryFound'])
        end = EXECUTE(dict(request, offset=20308))
        self.assertEqual(end['content'], '')
        self.assertFalse(end['more'])

    def test_patch_writers_are_offered_only_for_matching_engines_and_work_without_source_archives(self):
        for identity, module in [('kirikiri', 'python.archives.xp3'), ('artemis', 'python.archives.pfs')]:
            with self.subTest(engine=identity):
                result = self.call('inspect_engine_reference', engine=identity, length=1)
                self.assertEqual(result['patchDelivery']['module'], module)
                self.assertTrue(result['patchDelivery']['condition'])
        self.assertNotIn('patchDelivery', self.call('inspect_engine_reference', engine='alicesoft', length=1))
        result = self.call('run', workspace=str(self.directory), script='work/patch-writers.py', content='''
from python.archives import xp3, pfs
files = [('scenario/changed.ks', '中文'.encode('utf-8'))]
xp3_patch = xp3.build(files, filter_name='none')
assert [(entry.name, entry.data) for entry in xp3.extract(xp3_patch, filter_name='none')] == files
pfs_patch = pfs.build(files, version=8, name_encoding='utf-8')
assert [(entry.name, entry.data) for entry in pfs.extract(pfs_patch)] == files
print('standalone patches verified')
''')
        self.assertTrue(result['success'], result['stderr'])
        self.assertIn('standalone patches verified', result['stdout'])

    def test_pagination_rejects_invalid_bounds_and_types(self):
        for args in [dict(offset=-1), dict(offset=99999999), dict(length=0), dict(length=16001),
                     dict(length='16'), dict(length=True), dict(offset=1.5), dict(query='x' * 257)]:
            with self.subTest(args=args), self.assertRaises(ValueError):
                self.call('inspect_engine_reference', engine='alicesoft', **args)

    def test_explicit_paths_are_bounded_and_validated(self):
        result = self.call('inspect_engine_reference', engine='alicesoft', path='python/archives/ain.py', length=20)
        self.assertEqual(len(result['content']), 20)
        self.assertTrue(result['more'])
        for path in ['../engines/alicesoft.md', '/engines/alicesoft.md', 'python\\archives\\ain.py',
                     'other/source.py', 'engines/../alicesoft.md', '']:
            with self.subTest(path=path), self.assertRaises(ValueError):
                self.call('inspect_engine_reference', engine='alicesoft', path=path)
        with self.assertRaisesRegex(FileNotFoundError, '工具库中没有该文件'):
            self.call('inspect_engine_reference', engine='alicesoft', path='engines/absent.md')
        archive = self.fixture([self.engine('large')], {'engines/large.md': b'x' * ((4 << 20) + 1)})
        with self.assertRaisesRegex(ValueError, '更具体的参考文件'):
            EXECUTE(dict(operation='inspect_engine_reference', library=archive, engine='large', length=1))

    def test_search_metadata_limits_do_not_truncate_public_api_lists(self):
        item = self.engine('sample')
        modules = [f'python/engines/sample{i}.py' for i in range(20)]
        symbols = {modules[0]: [f'function{i}' for i in range(45)]}
        item.update(title='t' * 301, aliases=[str(i) for i in range(20)], python_references=modules,
                    public_symbols=symbols, scope='s' * 1001, evidence='e' * 1001)
        archive = self.fixture([item], {'engines/sample.md': 'guide'})
        result = EXECUTE(dict(operation='search_engine_references', library=archive, query='sample'))['matches'][0]
        self.assertEqual(result['title'], 't' * 300 + '\n[truncated]')
        self.assertEqual(len(result['aliases']), 12)
        self.assertEqual(len(result['sourceUrls']), 16)
        self.assertEqual(result['publicSymbols'], symbols)
        inspected = EXECUTE(dict(operation='inspect_engine_reference', library=archive, engine='sample'))
        self.assertEqual(inspected['pythonModules'], modules)
        self.assertEqual(inspected['publicSymbols'], symbols)

    def test_unavailable_index_returns_status_and_retains_document_hash(self):
        archive = self.fixture([], {'catalog/other.json': '{}'})
        invalid = '{"engines": false}'
        with zipfile.ZipFile(archive, 'w') as source:
            source.writestr('catalog/engines.json', invalid)
        result = EXECUTE(dict(operation='search_engine_references', library=archive, query='alicesoft'))
        self.assertEqual(result['status'], 'unavailable')
        self.assertEqual(result['matches'], [])
        self.assertEqual(result['contentSha256'], hashlib.sha256(invalid.encode()).hexdigest())
        self.assertTrue(result['error'])
        self.assertIsNone(result['hint'])

    def test_downloaded_catalog_uses_same_ranking_and_preserves_cache_metadata(self):
        content = json.dumps(dict(engines=[self.engine('alicesoft')]))
        metadata = dict(url=HOST['REFERENCE_ROOT'] + 'catalog/engines.json',
                        retrievedAt='2026-11-06T00:00:00Z', reused=True, stale=True,
                        contentSha256=hashlib.sha256(content.encode()).hexdigest())
        result = EXECUTE(dict(operation='search_engine_references', query='alice system4',
                              referenceIndexContent=content, referenceIndexMetadata=metadata))
        self.assertEqual([item['id'] for item in result['matches']], ['alicesoft'])
        for name in ['contentSha256', 'retrievedAt', 'reused', 'stale']:
            self.assertEqual(result[name], metadata[name])
        self.assertEqual(result['indexUrl'], metadata['url'])
        self.assertEqual(result['matches'][0]['documentUrl'], HOST['REFERENCE_ROOT'] + 'engines/alicesoft.md')
        unavailable = EXECUTE(dict(operation='search_engine_references', query='alice', referenceIndexError='offline'))
        self.assertEqual(unavailable['status'], 'unavailable')
        self.assertEqual(unavailable['error'], 'offline')
        self.assertEqual(unavailable['matches'], [])
        self.assertIsNone(unavailable['contentSha256'])


class BuiltinSourcesTest(unittest.TestCase):
    def test_format_index_and_utf16_paging_are_bounded(self):
        with tempfile.TemporaryDirectory() as directory:
            bundle = Path(directory) / 'sources.zip'
            with zipfile.ZipFile(bundle, 'w') as archive:
                for name in ['vendor/Scripts/Kirikiri/KirikiriKsScript.cs', 'vendor/Scripts/Kirikiri/KirikiriSocScript.cs',
                             'vendor/Scripts/Yuris/YurisScript.cs', 'vendor/Util/BinaryUtil.cs', 'worker/Worker.cs']:
                    archive.writestr(name, 'a😀b' + 'x' * 30000)
            request = dict(operation='builtin_sources', bundle=str(bundle), format='KirikiriKs',
                           parserType='VNTextPatch.Shared.Scripts.Kirikiri.KirikiriKsScript')
            index = EXECUTE(request)
            self.assertEqual(len(index['sourceFiles']), 4)
            self.assertFalse(any('Yuris' in entry['path'] for entry in index['sourceFiles']))
            page = EXECUTE(dict(request, path='vendor/Scripts/Kirikiri/KirikiriKsScript.cs', offset=1, length=2))
            self.assertEqual(page['content'], '😀')
            self.assertEqual(page['nextOffset'], 3)
            self.assertTrue(page['truncated'])
            for values in [dict(path='vendor/Scripts/Yuris/YurisScript.cs'), dict(path='worker/Worker.cs', length=24001)]:
                with self.assertRaises(ValueError):
                    EXECUTE(dict(request, **values))


if __name__ == '__main__':
    unittest.main()
