using GameL10n.Core.Development;

namespace GameL10n.Tests.Development;

public sealed class KaguyaTblstrComponentTests
{
    [Fact]
    public async Task RebuildPreservesNonzeroPaddingAndUntouchedRecordsAcrossAlignmentChanges()
    {
        await RunCheckAsync("""
            source, index = table([(0, '短'), (2, '名'), (0, '終わり')])
            records = module.read_table(source, index)
            path = 'ARC/TBLSTR.ARC'
            original_units = module.units(path, records)
            assert [u['id'] for u in original_units] == [
                path + '#group:0/field:0/type:0',
                path + '#group:0/field:1/type:2',
                path + '#group:1/field:0/type:0']
            assert [u['kind'] for u in original_units] == ['dialogue', 'name', 'dialogue']
            assert module.rebuild(path, records, {}, cp932) == (source, index)
            replacement = '長い文章を追加しました。'
            patched, patched_index = module.rebuild(path, records, {original_units[0]['id']: replacement}, cp932)
            after = module.read_table(patched, patched_index)
            assert after[0].text == replacement and len(patched) > len(source)
            assert [r.raw for r in after[1:]] == [r.raw for r in records[1:]]
            assert after[1].raw[-1] == 0x7d
            assert after[2].offset != records[2].offset
            assert [u['id'] for u in module.units(path, after)] == [u['id'] for u in original_units]
            assert struct.unpack('<III', patched_index) == (3, 4, after[2].offset)

            ascii_source, ascii_index = table([(0, 'A')])
            ascii_records = module.read_table(ascii_source, ascii_index)
            try:
                module.rebuild(path, ascii_records, {module.unit_id(path, ascii_records[0]): '中文测试'}, cp932)
            except UnicodeEncodeError:
                pass
            else:
                raise AssertionError('CP932 must reject unsupported Chinese before runtime assumptions')
            custom, custom_index = module.rebuild(path, ascii_records,
                {module.unit_id(path, ascii_records[0]): '中文测试'}, lambda text: text.encode('utf-8'))
            assert module.read_table(custom, custom_index, lambda raw: raw.decode('utf-8'))[0].text == '中文测试'
            print('kaguya-check-ok')
            """);
    }

    [Fact]
    public async Task ProbeRejectsLongTailsBadIndexesAndEncodingAndReportsReadOnlyCompactEvidence()
    {
        await RunCheckAsync("""
            import subprocess

            source, index = table([(0, '短'), (2, '名'), (0, '終わり')])
            arc = Path('table.arc'); ari = Path('table.ari')
            arc.write_bytes(source); ari.write_bytes(index)

            def cli(request):
                run = subprocess.run([sys.executable, '-I', '-B', str(component)],
                    input=json.dumps(request), capture_output=True, text=True, timeout=10)
                return run.returncode, json.loads(run.stdout)

            request = dict(operation='probe', input=str(arc), index=str(ari))
            exit_code, result = cli(request)
            assert exit_code == 0 and result['status'] == 'matched', result
            assert result['checks'] == dict(structure=True, index=True, encoding=True, byteRoundtrip=True)
            assert result['counts'] == dict(records=3, primary=2, types={'0': 2, '2': 1, '3': 0})
            assert len(result['samples']) == 2 and all(len(s['text']) <= 100 for s in result['samples'])
            assert result['cp932Chinese']['canEncode'] is False and result['runtimeTested'] is False
            assert arc.read_bytes() == source and ari.read_bytes() == index
            _, quiet = cli(dict(request, sampleLimit=0))
            assert quiet['status'] == 'matched' and quiet['samples'] == []
            _, one_sample = cli(dict(request, sampleLimit=1))
            assert one_sample['status'] == 'matched' and len(one_sample['samples']) == 1

            # A NUL followed by six bytes is not minimal four-byte alignment,
            # even if all extra bytes are zero and the old parser accepted it.
            extra_tail = struct.pack('<III', 20, 0, 8) + b'\xbe\0' + bytes(6)
            bad_index = struct.pack('<III', 3, 4, 8)
            bad_encoding = struct.pack('<III', 16, 0, 4) + b'\x7e\0\x7d\x7d'
            unknown_type = struct.pack('<III', 16, 4, 4) + b'\xbe\0\x7d\x7d'
            for broken_arc, broken_ari, failed_check in [
                (extra_tail, struct.pack('<II', 2, 4), 'structure'),
                (unknown_type, struct.pack('<II', 2, 4), 'structure'),
                (source, bad_index, 'index'),
                (bad_encoding, struct.pack('<II', 2, 4), 'encoding')]:
                arc.write_bytes(broken_arc); ari.write_bytes(broken_ari)
                exit_code, rejected = cli(request)
                assert exit_code != 0 and rejected['status'] == 'rejected', rejected
                assert rejected['checks'][failed_check] is False, rejected
                assert arc.read_bytes() == broken_arc and ari.read_bytes() == broken_ari

            exit_code, missing = cli(dict(operation='probe', input=str(arc)))
            assert exit_code != 0 and missing['status'] == 'needs-evidence'
            assert missing['checks']['structure'] is None and missing['runtimeTested'] is False
            assert {p.name for p in Path('.').iterdir()} == {'library.zip', 'check.py', 'src', 'table.arc', 'table.ari'}
            print('kaguya-check-ok')
            """);
    }

    private static async Task RunCheckAsync(string body)
    {
        using var fixture = new EngineArchiveFixture();
        await new AdapterComponentAssets().CopyAsync(["kaguya-tblstr"], fixture.Root);
        var component = Path.Combine(fixture.Root, "src", "tools", "kaguya_tblstr.py");
        var check = Path.Combine(fixture.Root, "check.py");
        await File.WriteAllTextAsync(check, Prelude + "\n" + body);
        var run = await new AdapterProcessRunner().RunAsync(new(fixture.Paths.Python,
            EngineReferenceToolkit.PythonArguments(fixture.Paths, check, [component]), fixture.Root), default);
        Assert.True(run.ExitCode == 0, run.StandardOutput + run.StandardError);
        Assert.Equal("kaguya-check-ok", run.StandardOutput.Trim());
    }

    private const string Prelude = """
        from pathlib import Path
        import importlib.util, json, struct, sys
        sys.dont_write_bytecode = True
        component = Path(sys.argv[1])
        spec = importlib.util.spec_from_file_location('kaguya_tblstr', component)
        module = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = module
        spec.loader.exec_module(module)
        cp932 = lambda text: text.encode('cp932')

        def table(items):
            data = bytearray(4)
            offsets = []
            for kind, text in items:
                if kind == 0:
                    offsets.append(len(data))
                payload = bytes(value ^ 255 for value in cp932(text)) + b'\0'
                payload += b'\x7d' * (-len(payload) % 4)
                data.extend(struct.pack('<II', kind, len(payload)) + payload)
            struct.pack_into('<I', data, 0, len(data))
            return bytes(data), struct.pack('<' + 'I' * (len(offsets) + 1), len(offsets) + 1, *offsets)
        """;
}
