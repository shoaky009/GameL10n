"""Checks for bounded file tools, process diagnostics and archive-result retention."""
import json
from pathlib import Path
import runpy
import tempfile
import unittest

HOST = runpy.run_path(str(Path(__file__).with_name('host.py')))
EXECUTE = HOST['execute']


class HostToolsTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / 'work/nested').mkdir(parents=True)

    def execute(self, operation, **arguments):
        return EXECUTE(dict(operation=operation, root=str(self.root), workspace=str(self.root), **arguments))

    def test_directory_scope_and_pagination_keep_original_root_relative_paths(self):
        for name in ('a.txt', 'b.txt', 'c.txt'):
            (self.root / 'work/nested' / name).write_text(name, encoding='utf-8')
        first = self.execute('list_files', path='work/nested', limit=2)
        second = self.execute('list_files', path='work/nested', offset=first['nextOffset'], limit=2)
        self.assertTrue(first['more'])
        self.assertFalse(second['more'])
        self.assertEqual({item['path'] for item in first['files'] + second['files']},
                         {'work/nested/a.txt', 'work/nested/b.txt', 'work/nested/c.txt'})

    def test_file_defaults_byte_bounds_and_hex_match_the_contract(self):
        (self.root / 'data.bin').write_bytes(bytes((0, 0xab, 0xff)))
        result = self.execute('read_file', path='data.bin', offset=1)
        self.assertEqual(result, dict(bytes=3, offset=1, length=2, encoding='hex', more=False, content='ABFF'))
        with self.assertRaises(ValueError):
            self.execute('read_file', path='data.bin', offset=4)
        with self.assertRaises(ValueError):
            self.execute('read_file', path='data.bin', length=32769)

    def test_python_probes_resolve_selected_resource_aliases_without_copies(self):
        game = self.root / 'game'
        game.mkdir()
        backup = game / 'scene.bin.bak'
        backup.write_text('original', encoding='utf8')
        result = self.execute('run', script='work/inspect.py',
            resourceDirectory=str(game), resourcePaths={'scene.bin': str(backup)},
            arguments=['resources/scene.bin'], stdin=json.dumps({'input': 'resources/scene.bin'}),
            content="""import json,sys
from pathlib import Path
assert Path(sys.argv[1]) == resource_paths['scene.bin']
assert Path(json.load(sys.stdin)['input']).read_text(encoding='utf8') == 'original'
print('original')
""")
        self.assertTrue(result['success'], result)
        self.assertEqual(result['stdout'].strip(), 'original')
        self.assertFalse((self.root / 'resources/scene.bin').exists())
        self.assertEqual(backup.read_text(encoding='utf8'), 'original')

    def test_text_search_is_case_insensitive_and_lossy_byte_offsets_stay_unknown(self):
        (self.root / 'work/source.py').write_text('print("HELLO")', encoding='utf-8')
        result = self.execute('search_text', path='work', query='hello', workspaceRoot=True)
        self.assertEqual(result['matches'][0]['byteOffset'], 7)
        self.assertEqual(result['matches'][0]['encoding'], 'utf8')
        (self.root / 'lossy.dat').write_bytes(b'A\x81')
        lossy = self.execute('search_text', path='lossy.dat', query='a')
        self.assertEqual(lossy['byteOffsets'], [None])
        self.assertEqual(lossy['encoding'], 'cp932')

    def test_utf16_read_and_common_script_directory_search(self):
        (self.root / 'work/nested/config.tjs').write_bytes('AB字体CD'.encode('utf-16-le'))
        (self.root / 'work/nested/scene.ast').write_bytes('XY対話ZW'.encode('utf-16-be'))
        read_le = self.execute('read_file', path='work/nested/config.tjs', encoding='utf16le')
        self.assertEqual(read_le['content'], 'AB字体CD')
        self.assertEqual(read_le['encoding'], 'utf16le')
        read_be = self.execute('read_file', path='work/nested/scene.ast', encoding='utf16be')
        self.assertEqual(read_be['content'], 'XY対話ZW')
        self.assertEqual(read_be['encoding'], 'utf16be')
        found_le = self.execute('search_text', path='work/nested', query='字体', encoding='utf16le')
        self.assertEqual(len(found_le['matches']), 1)
        self.assertEqual(found_le['matches'][0]['path'], 'work/nested/config.tjs')
        self.assertEqual(found_le['matches'][0]['byteOffset'], 4)
        self.assertEqual(found_le['matches'][0]['encoding'], 'utf16le')
        found_be = self.execute('search_text', path='work/nested', query='対話', encoding='utf16be')
        self.assertEqual(len(found_be['matches']), 1)
        self.assertEqual(found_be['matches'][0]['path'], 'work/nested/scene.ast')
        self.assertEqual(found_be['matches'][0]['byteOffset'], 4)
        self.assertEqual(found_be['matches'][0]['encoding'], 'utf16be')

    def test_bound_inputs_keep_original_reads_and_paths_separate_from_installed_files(self):
        game = self.root / 'game'
        source = self.root / 'source'
        (game / 'scripts').mkdir(parents=True)
        source.mkdir()
        (game / 'scripts/one.txt').write_text('installed translation', encoding='utf-8')
        (game / 'scripts/addon.txt').write_text('patch only', encoding='utf-8')
        (source / 'one.bin').write_text('original one', encoding='utf-8')
        (source / 'two.bin').write_text('original two', encoding='utf-8')
        inputs = dict(root=str(game), inputFiles={'scripts/one.txt': str(source / 'one.bin'),
                                                'scripts/two.txt': str(source / 'two.bin')},
                      excludedFiles=['scripts/addon.txt'])
        listed = EXECUTE(dict(operation='list_files', path='scripts', **inputs))
        self.assertEqual({item['path'] for item in listed['files']}, {'scripts/one.txt', 'scripts/two.txt'})
        read = EXECUTE(dict(operation='read_file', path='scripts/ONE.txt', encoding='utf8', **inputs))
        self.assertEqual(read['content'], 'original one')
        found = EXECUTE(dict(operation='search_text', path='scripts', query='original', encoding='utf8', **inputs))
        self.assertEqual({item['path'] for item in found['matches']}, {'scripts/one.txt', 'scripts/two.txt'})
        with self.assertRaises(ValueError):
            EXECUTE(dict(operation='read_file', path='scripts/addon.txt', **inputs))
        installed = EXECUTE(dict(operation='read_file', root=str(game), path='scripts/one.txt', encoding='utf8'))
        self.assertEqual(installed['content'], 'installed translation')
        self.assertEqual((game / 'scripts/one.txt').read_text(encoding='utf-8'), 'installed translation')

    def test_python_probe_accepts_working_directory_arguments_and_stdin(self):
        result = self.execute('run', script='work/probe.py',
                              content='import pathlib,sys;print(pathlib.Path.cwd().name,sys.argv[1],sys.stdin.read())',
                              workingDirectory='work/nested', arguments=['arg'], stdin='译文')
        self.assertTrue(result['success'])
        self.assertEqual(result['stdout'].strip(), 'nested arg 译文')
        with self.assertRaises(ValueError):
            self.execute('run', script='work/probe.py', module='python.engines.example')

    def test_nonzero_exit_and_output_limit_preserve_diagnostics(self):
        failed = self.execute('run', script='work/fail.py', content='import sys;print("diagnostic",file=sys.stderr);sys.exit(3)')
        self.assertFalse(failed['success'])
        self.assertEqual(failed['exitCode'], 3)
        self.assertIn('diagnostic', failed['stderr'])
        large = self.execute('run', script='work/large.py', content='print("x"*(2<<20))')
        self.assertFalse(large['success'])
        self.assertIn('1 MiB', large['error'])
        self.assertLessEqual(len(large['stdout'].encode()) + len(large['stderr'].encode()), 1 << 20)

    def test_probe_discovers_written_samples_without_explicit_selection(self):
        (self.root / 'work/nested/read.scn').write_bytes(b'read only')
        result = self.execute('run', script='work/decode.py', workingDirectory='work/nested',
                              sampleExtensions=['.ks', '.scn'], content='''
from pathlib import Path
Path('read.scn').read_bytes()
Path('台詞.ks').write_bytes(b'sample')
Path('report.json').write_text('{}')
Path('../../src').mkdir(exist_ok=True)
Path('../../src/ignored.ks').write_bytes(b'not a working sample')
Path('removed.ks').write_bytes(b'deleted')
Path('removed.ks').unlink()
print('unchanged stdout')
''')
        self.assertTrue(result['success'], result)
        self.assertEqual(result['writtenFiles'], ['work/nested/台詞.ks'])
        self.assertFalse(result['writtenFilesPartial'])
        self.assertEqual(result['stdout'].strip(), 'unchanged stdout')
        self.assertEqual(list((self.root / 'work').glob('.sample-files-*')), [])
        self.assertEqual(list((self.root / 'work').glob('.resource-inputs-*')), [])

    def test_atomic_sample_replacement_and_failed_probe_keep_written_paths(self):
        result = self.execute('run', script='work/decode.py', sampleExtensions=['.scn'], content='''
from pathlib import Path
Path('work/scene.tmp').write_bytes(b'PSB sample')
Path('work/scene.tmp').replace('work/scene.scn')
raise ValueError('later experiment failed')
''')
        self.assertFalse(result['success'])
        self.assertEqual(result['writtenFiles'], ['work/scene.scn'])
        self.assertIn('later experiment failed', result['stderr'])

    def test_memory_samples_are_registered_from_nested_working_directories(self):
        result = self.execute('run', script='work/decode.py', workingDirectory='work/nested', content='''
assert save_sample('work/samples/scene.bin', memoryview(b'PSB sample')) == 'work/samples/scene.bin'
save_sample('work/samples/scene.bin', bytearray(b'new sample'))
raise ValueError('later experiment failed')
''')
        self.assertFalse(result['success'])
        self.assertEqual(result['registeredSamples'], ['work/samples/scene.bin'])
        self.assertEqual((self.root / 'work/samples/scene.bin').read_bytes(), b'new sample')
        self.assertEqual(list((self.root / 'work').glob('.sample-files-*')), [])
        self.assertEqual(list((self.root / 'work').glob('.resource-inputs-*')), [])

    def test_memory_samples_reject_invalid_scope_and_excessive_inputs_before_writing(self):
        for expression in ("save_sample('../outside.ks', b'x')",
                           "save_sample('resources/scene.ks', b'x')",
                           "save_sample('work/../scene.ks', b'x')",
                           "save_sample('work/sample.ks', 'text')",
                           "save_sample('work/sample.ks', b'x' * ((64 << 20) + 1))"):
            with self.subTest(expression=expression):
                result = self.execute('run', script='work/invalid.py', content=expression)
                self.assertFalse(result['success'])
                self.assertEqual(result['registeredSamples'], [])
        self.assertFalse((self.root / 'work/sample.ks').exists())
        result = self.execute('run', script='work/many.py', content='''
for i in range(13):
    save_sample(f'work/sample-{i}.ks', b'x')
''')
        self.assertFalse(result['success'])
        self.assertEqual(len(result['registeredSamples']), 12)
        self.assertFalse((self.root / 'work/sample-12.ks').exists())

    def test_written_sample_tracking_is_bounded(self):
        result = self.execute('run', script='work/many.py', sampleExtensions=['.ks'], content='''
from pathlib import Path
for index in range(140):
    Path(f'work/{index}.ks').write_bytes(b'sample')
''')
        self.assertTrue(result['success'])
        self.assertEqual(len(result['writtenFiles']), 128)
        self.assertTrue(result['writtenFilesPartial'])

    def test_experiment_report_returns_bounded_observations_and_retains_full_local_details(self):
        result = self.execute('run', script='work/report.py', questionId='archive-filter',
                              reportFile='work/reports/report.json', workingDirectory='work/nested', content='''
import json, os
from pathlib import Path
report = dict(status='completed', tested=24, matched=2, summary='结论' * 2000,
              failures=['failure' * 1000] * 20, details=list(range(20000)), passed=True)
Path(os.environ['GAMEL10N_REPORT_FILE']).write_text(json.dumps(report), encoding='utf-8')
print(os.environ['GAMEL10N_QUESTION_ID'])
''')
        report = result['report']
        self.assertTrue(result['success'])
        self.assertEqual(result['questionId'], 'archive-filter')
        self.assertEqual((report['tested'], report['matched']), (24, 2))
        self.assertEqual(report['source'], 'script-observation')
        self.assertFalse(report['validation'])
        self.assertLess(len(json.dumps(report, ensure_ascii=False).encode('utf-8')), 6144)
        self.assertNotIn('passed', report)
        self.assertEqual(len(report['failures']), 4)
        self.assertEqual(report['failureCount'], 20)
        data = (self.root / report['file']).read_bytes()
        self.assertEqual(report['sha256'], __import__('hashlib').sha256(data).hexdigest())
        self.assertEqual(len(json.loads(data)['details']), 20000)
        self.assertEqual(len(result['scriptSha256']), 64)

    def test_missing_stale_or_malformed_report_does_not_erase_process_diagnostics(self):
        for data in (None, '{invalid', '{"status":"completed"}'):
            with self.subTest(data=data):
                path = self.root / 'work/result.json'
                path.unlink(missing_ok=True)
                if data is not None:
                    path.write_text(data, encoding='utf-8')
                result = self.execute('run', script='work/stale.py', reportFile='work/result.json',
                                      content='import sys;print("original diagnostic",file=sys.stderr);sys.exit(3)')
                self.assertFalse(result['success'])
                self.assertEqual(result['exitCode'], 3)
                self.assertIn('original diagnostic', result['stderr'])
                self.assertIn('reportError', result)
                self.assertNotIn('report', result)
                if data is None:
                    self.assertIn('脚本执行失败', result['reportError'])
        missing = self.execute('run', script='work/missing.py', reportFile='work/absent.json',
                               content="print('PAC analysis results')")
        self.assertFalse(missing['success'])
        self.assertEqual(missing['phase'], 'report')
        self.assertTrue(missing['executionSucceeded'])
        self.assertEqual(missing['exitCode'], 0)
        self.assertEqual(missing['stdout'].strip(), 'PAC analysis results')
        self.assertEqual(missing['error'], '脚本执行成功，但未生成声明的报告：work/absent.json。')
        self.assertEqual(missing['reportError'], missing['error'])
        self.assertEqual(missing['reportContract']['file'], 'work/absent.json')
        self.assertIn('write_report', missing['nextAction'])
        self.assertIn('省略 reportFile', missing['nextAction'])

    def test_stdout_only_probe_needs_no_report(self):
        result = self.execute('run', script='work/stdout.py', content="print('PAC analysis results')")
        self.assertTrue(result['success'])
        self.assertTrue(result['executionSucceeded'])
        self.assertEqual(result['stdout'].strip(), 'PAC analysis results')
        self.assertNotIn('reportError', result)
        self.assertNotIn('reportContract', result)

    def test_report_helper_writes_actual_findings_without_imports_from_nested_directory(self):
        result = self.execute('run', script='work/report-helper.py', reportFile='work/reports/helper.json',
                              workingDirectory='work/nested', content='''
write_report(status='partial', tested=4, matched=1, summary='仍有候选未检查', details={'offset': 128})
''')
        self.assertTrue(result['success'])
        self.assertTrue(result['executionSucceeded'])
        self.assertEqual(result['report']['status'], 'partial')
        self.assertFalse(result['report']['validation'])
        saved = json.loads((self.root / 'work/reports/helper.json').read_text(encoding='utf-8'))
        self.assertEqual(saved['details'], {'offset': 128})

    def test_report_helper_does_not_infer_success_or_accept_invalid_fields(self):
        for content, phase in (("write_report(status='failed', summary='样本不匹配')", 'run'),
                               ("write_report(status='completed', tested=True)", 'report'),
                               ("write_report(summary='missing status')", 'run')):
            with self.subTest(content=content):
                result = self.execute('run', script='work/invalid-helper.py',
                                      reportFile='work/helper.json', content=content)
                self.assertFalse(result['success'])
                self.assertEqual(result['phase'], phase)
        missing = self.execute('run', script='work/no-report-path.py',
                               content="write_report(status='completed')")
        self.assertFalse(missing['success'])
        self.assertIn('reportFile', missing['stderr'])

    def test_failed_probe_keeps_partial_report_without_granting_verification(self):
        result = self.execute('run', script='work/partial.py', reportFile='work/partial.json', content='''
import os, sys
from pathlib import Path
Path(os.environ['GAMEL10N_REPORT_FILE']).write_text('{"status":"partial","tested":4,"matched":1}')
print('later candidate failed', file=sys.stderr)
sys.exit(3)
''')
        self.assertFalse(result['success'])
        self.assertEqual(result['exitCode'], 3)
        self.assertEqual(result['report']['status'], 'partial')
        self.assertFalse(result['report']['validation'])

    def test_report_scope_and_question_id_are_checked_before_execution(self):
        for args in (dict(reportFile='../escaped.json'), dict(reportFile='src/report.json'),
                     dict(reportFile='work/report.txt'), dict(questionId='bad/id'), dict(questionId=True)):
            with self.subTest(args=args), self.assertRaises(ValueError):
                self.execute('run', script='work/should-not-write.py', content='raise RuntimeError("executed")', **args)
        self.assertFalse((self.root / 'work/should-not-write.py').exists())

    def test_invalid_report_fields_and_oversized_reports_reject_the_observation(self):
        for data in ('{}', '[]', '{"status":"passed"}', '{"status":"completed","tested":true}',
                     '{"status":"completed","tested":1,"matched":2}', '{"status":"completed","summary":[]}',
                     '{"status":"completed","failures":[1]}', '{"status":"completed","tested":NaN}',
                     ' ' * ((8 << 20) + 1)):
            with self.subTest(data=data[:80]):
                result = self.execute('run', script='work/invalid-report.py', reportFile='work/invalid.json',
                                      stdin=data, content='''
import os, sys
from pathlib import Path
Path(os.environ['GAMEL10N_REPORT_FILE']).write_text(sys.stdin.read(), encoding='utf-8')
''')
                self.assertFalse(result['success'])
                self.assertEqual(result['phase'], 'report')
                self.assertNotIn('report', result)

    def test_timeout_retains_output_and_explicit_run_phase(self):
        result = self.execute('run', script='work/slow.py', timeoutSeconds=1,
                              content='import time;print("started",flush=True);time.sleep(5)')
        self.assertFalse(result['success'])
        self.assertTrue(result['timedOut'])
        self.assertEqual(result['phase'], 'run')
        self.assertIn('started', result['stdout'])

    def test_archive_tool_keeps_full_manifest_and_bounds_summary(self):
        components = self.root / 'components'
        components.mkdir()
        (components / 'engine_archive.py').write_text(
            'def execute(request): return dict(success=True,entries=[dict(name=str(i)) for i in range(20)])', encoding='utf-8')
        manifest = self.root / 'work/archive/manifest.json'
        result = self.execute('archive_component', components=str(components), library=str(self.root),
                              request=dict(component='archive.rpa3', responseFile=str(manifest)))
        self.assertEqual(len(result['entries']), 12)
        self.assertEqual(len(json.loads(manifest.read_text(encoding='utf-8'))['entries']), 20)

    def test_launch_inspection_reads_locale_profile_cp932_and_known_error_log_tail(self):
        (self.root / 'game.exe').write_bytes(b'fixture, never execute')
        (self.root / 'game.exe.le.config').write_text(
            '<LEConfig><Profiles><Profile><Location>ja-JP</Location><RunAsAdmin>false</RunAsAdmin>'
            '<RunWithSuspend>TRUE</RunWithSuspend></Profile></Profiles></LEConfig>', encoding='utf-8')
        (self.root / 'start.cmd').write_bytes('echo 日本語'.encode('cp932'))
        (self.root / 'error.log').write_bytes(('older\n' * 1000 + '最後のエラー\n').encode('cp932'))
        (self.root / 'save.log').write_text('private save content', encoding='utf-8')
        result = self.execute('inspect_game_launch')
        self.assertEqual(result['localeProfiles'][0]['location'], 'ja-JP')
        self.assertTrue(result['localeProfiles'][0]['executableExists'])
        self.assertFalse(result['localeProfiles'][0]['runAsAdmin'])
        self.assertTrue(result['localeProfiles'][0]['runWithSuspend'])
        self.assertEqual(result['launchScripts'][0]['content'], 'echo 日本語')
        self.assertTrue(result['launchScripts'][0]['encoding'].startswith('cp932'))
        self.assertEqual(len(result['errorLogs']), 1)
        self.assertTrue(result['errorLogs'][0]['truncated'])
        self.assertTrue(result['errorLogs'][0]['content'].endswith('最後のエラー\n'))

    def test_launch_inspection_rejects_dtd_limits_evidence_and_decodes_utf16_bom(self):
        (self.root / 'bad.exe.le.config').write_bytes(
            b'<!DOCTYPE LEConfig [<!ENTITY value "secret">]><LEConfig><Profiles><Profile>'
            b'<Location>&value;</Location></Profile></Profiles></LEConfig>')
        for index in range(5):
            (self.root / f'{index}.ps1').write_text('Write-Output "中文"', encoding='utf-16')
        result = self.execute('inspect_game_launch')
        self.assertIn('DTD', result['localeProfiles'][0]['error'])
        self.assertIsNone(result['localeProfiles'][0]['location'])
        self.assertEqual(len(result['launchScripts']), 4)
        self.assertEqual(result['launchScripts'][0]['content'], 'Write-Output "中文"')
        self.assertTrue(result['partial'])

    def test_launch_inspection_records_link_errors_without_aborting(self):
        outside = tempfile.TemporaryDirectory()
        self.addCleanup(outside.cleanup)
        target_exe = Path(outside.name) / 'real.exe'
        target_exe.write_bytes(b'fixture')
        target_bat = Path(outside.name) / 'real.bat'
        target_bat.write_text('echo hi', encoding='utf-8')
        try:
            (self.root / 'linked.exe').symlink_to(target_exe)
            (self.root / 'linked.bat').symlink_to(target_bat)
        except OSError:
            self.skipTest('Symbolic links require local OS permission')
        (self.root / 'linked.exe.le.config').write_text(
            '<LEConfig><Profiles><Profile><Location>ja-JP</Location></Profile></Profiles></LEConfig>',
            encoding='utf-8',
        )
        result = self.execute('inspect_game_launch')
        self.assertEqual(len(result['localeProfiles']), 1)
        self.assertFalse(result['localeProfiles'][0]['executableExists'])
        self.assertTrue(result['localeProfiles'][0]['error'])
        sample = HOST['launch_text_sample'](self.root, 'linked.bat', None)
        self.assertEqual(sample['encoding'], 'unknown')
        self.assertTrue(sample['error'])


if __name__ == '__main__':
    unittest.main()
