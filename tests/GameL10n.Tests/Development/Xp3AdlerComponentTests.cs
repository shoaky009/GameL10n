using GameL10n.Core.Development;

namespace GameL10n.Tests.Development;

public sealed class Xp3AdlerComponentTests
{
    [Fact]
    public Task FilterKeepsMemberPhaseAcrossIndependentBuffersAndRejectsInvalidRanges() => CheckAsync("""
        key = xp3.generate_key(0x12345678)
        assert key == reference_key(0x12345678)
        original = bytearray(range(97))
        whole = original.copy()
        xp3.apply_key_in_place(whole, 0, len(whole), key)
        pieces = []
        for start, end in ((0, 7), (7, 32), (32, 65), (65, 97)):
            part = original[start:end]
            xp3.apply_key_in_place(part, 0, len(part), key, member_offset=start)
            pieces.append(part)
        assert b''.join(pieces) == whole
        subrange = original.copy()
        xp3.apply_key_in_place(subrange, 7, 58, key)
        assert subrange[7:65] == whole[7:65] and subrange[:7] == original[:7]
        for offset, count, phase, bad_key in ((-1, 1, 0, key), (0, -1, 0, key),
                (96, 2, 0, key), (0, 1, -1, key), (0, 1, 0, key[:-1])):
            value = original.copy()
            try:
                xp3.apply_key_in_place(value, offset, count, bad_key, member_offset=phase)
                raise AssertionError('invalid filter range accepted')
            except ValueError:
                assert value == original
        """);

    [Fact]
    public Task ProbeRequiresTwoProtectedChecksumsAndRunsDirectlyAsBoundedJsonCli() => CheckAsync("""
        path = archive('matched.xp3', [('one.ks', b'first script', True),
                                     ('two.tjs', b'second script with a different Adler', True)],
                       split=7, compressed=True, compressed_index=True, pointer_block=True)
        before = path.read_bytes()
        result = xp3.probe_archive(path)
        assert result['status'] == 'matched', result
        assert result['counts']['verifiedProtected'] == 2
        assert result['runtimeTested'] is False and result['filterOnly'] is True
        assert result['rebuildRequiresPreservation'] is False
        one = xp3.probe_archive(path, ['one.ks'])
        assert one['status'] == 'needs-evidence' and one['counts']['verifiedProtected'] == 1
        assert xp3.probe_archive(path, ['one.ks', 'one.ks'])['status'] == 'rejected'
        missing = xp3.probe_archive(path, ['absent.ks'])
        assert missing['status'] == 'needs-evidence' and missing['counts']['missing'] == 1
        plain = archive('plain.xp3', [('a.ks', b'unprotected A', False), ('b.ks', b'unprotected B', False)])
        result = xp3.probe_archive(plain, ['a.ks', 'b.ks'])
        assert result['status'] == 'needs-evidence' and result['counts']['verifiedUnprotected'] == 2
        bad = archive('conflict.xp3', [('a.ks', b'first script', True), ('b.ks', b'changed', True)], bad_checksum=True)
        result = xp3.probe_archive(bad)
        assert result['status'] == 'rejected' and 'Adler32' in result['reason'], result
        cli = subprocess.run([sys.executable, '-B', str(component)], input=json.dumps(
            dict(operation='probe', input=str(path), sampleLimit=1)), text=True, capture_output=True)
        assert cli.returncode == 0 and not cli.stderr, (cli.stdout, cli.stderr)
        result = json.loads(cli.stdout)
        assert result['status'] == 'matched' and len(result['samples']) == 1
        assert 'first script' not in cli.stdout and 'second script' not in cli.stdout
        assert path.read_bytes() == before
        assert xp3.probe_archive(path, sample_limit=3)['status'] == 'rejected'
        many = archive('many.xp3', [(str(i) + '.ks', bytes([i]) * (i + 1), True) for i in range(7)])
        result = xp3.probe_archive(many)
        assert result['status'] == 'matched' and result['counts']['selected'] == 6
        assert len(result['samples']) == 2
        assert xp3.probe_archive(many, [str(i) + '.ks' for i in range(7)])['status'] == 'rejected'
        """);

    [Fact]
    public Task UnknownIndexAndMemberChunksRequirePreservationAndBoundedEvidence() => CheckAsync("""
        path = archive('unknown.xp3', [('one.ks', b'first script', True), ('two.ks', b'second script', True)], unknown=True)
        result = xp3.probe_archive(path)
        assert result['status'] == 'matched', result
        assert result['rebuildRequiresPreservation'] is True
        assert result['unknownIndexChunkCount'] == 12
        assert len(result['unknownIndexChunks']) == 8 and result['unknownIndexChunksTruncated']
        assert {item['scope'] for item in result['unknownIndexChunks']} == {'index', 'member'}
        with path.open('rb') as stream:
            index = xp3.read_index(stream)
            assert index.unknown_chunk_count == 12
            assert xp3.read_member(stream, index.entries[0]) == b'first script'
        """);

    [Fact]
    public Task ProbeRejectsBadSegmentBoundariesAndInflationOutsideDeclaredLength() => CheckAsync("""
        files = [('one.ks', b'first script', True), ('two.ks', b'second script', True)]
        for option in ('bad_range', 'bad_total', 'expanding_segment', 'trailing_zlib'):
            path = archive(option + '.xp3', files, **{option: True})
            result = xp3.probe_archive(path)
            assert result['status'] == 'rejected', (option, result)
        bomb = Path('oversized-index.xp3')
        bomb.write_bytes(xp3.MAGIC + struct.pack('<Q', 19) + b'\1'
                         + struct.pack('<QQ', 1, xp3.MAX_INDEX + 1) + b'x')
        result = xp3.probe_archive(bomb)
        assert result['status'] == 'rejected' and 'budget' in result['reason'], result
        """);

    private static async Task CheckAsync(string assertions)
    {
        using var fixture = new EngineArchiveFixture();
        await new AdapterComponentAssets().CopyAsync(["xp3-adler"], fixture.Root);
        var component = Path.Combine(fixture.Root, "src", "tools", "xp3_adler.py");
        var check = Path.Combine(fixture.Root, "check.py");
        await File.WriteAllTextAsync(check, FixtureScript + "\n" + assertions);
        var run = await new AdapterProcessRunner().RunAsync(new(fixture.Paths.Python,
            EngineReferenceToolkit.PythonArguments(fixture.Paths, check, [component]), fixture.Root), default);
        Assert.True(run.ExitCode == 0, run.StandardOutput + run.StandardError);
    }

    private const string FixtureScript = """
        import json, struct, subprocess, sys, zlib
        from pathlib import Path
        sys.dont_write_bytecode = True
        component = Path(sys.argv[1])
        sys.path.insert(0, str(component.parent))
        import xp3_adler as xp3

        def reference_key(adler):
            # Independently spell the 31-bit seed as LSB-first bits; the table
            # still has 32 bytes and restarts at each 32-byte member boundary.
            bits = f'{(adler ^ 0x2F91DE55) & 0x7fffffff:031b}'[::-1]
            return bytes(sum(int(bits[(8*i+b) % 31]) << b for b in range(8)) for i in range(32))

        def chunk(tag, body):
            return tag + struct.pack('<Q', len(body)) + body

        def archive(name, files, *, split=None, compressed=False, compressed_index=False,
                    pointer_block=False, unknown=False, bad_checksum=False, bad_range=False,
                    bad_total=False, expanding_segment=False, trailing_zlib=False):
            output = bytearray(xp3.MAGIC + b'\0' * (29 if pointer_block else 8))
            index = chunk(b'cust', b'archive metadata') if unknown else b''
            for number, (member, plain, protected) in enumerate(files):
                checksum = zlib.adler32(plain) & 0xffffffff
                key = reference_key(checksum)
                filtered = bytes(value ^ key[i & 31] for i, value in enumerate(plain)) if protected else plain
                parts = [filtered] if split is None else [filtered[:split], filtered[split:]]
                segments, packed_total = b'', 0
                for part in parts:
                    use_zlib = compressed or expanding_segment or trailing_zlib
                    payload = zlib.compress(part) if use_zlib else part
                    if expanding_segment and number == 0:
                        payload = zlib.compress(b'x' * (1 << 20))
                    if trailing_zlib and number == 0:
                        payload += b'extra'
                    offset = 1 if bad_range and number == 0 else len(output)
                    segments += struct.pack('<IQQQ', int(use_zlib), offset, len(part), len(payload))
                    output.extend(payload)
                    packed_total += len(payload)
                encoded = member.encode('utf-16-le')
                info = struct.pack('<IQQH', 0x80000000 if protected else 0,
                    len(plain) + int(bad_total and number == 0), packed_total, len(encoded)//2) + encoded
                checksum ^= int(bad_checksum and number == 1)
                body = chunk(b'info', info) + chunk(b'segm', segments) + chunk(b'adlr', struct.pack('<I', checksum))
                if unknown and number == 0:
                    body += chunk(b'time', b'member metadata')
                index += chunk(b'File', body)
            if unknown:
                index += b''.join(chunk(b'more', b'') for _ in range(10))
            index_at = len(output)
            struct.pack_into('<Q', output, 11, 0x17 if pointer_block else index_at)
            if pointer_block:
                struct.pack_into('<Q', output, 0x20, index_at)
            if compressed_index:
                packed = zlib.compress(index)
                output.extend(b'\1' + struct.pack('<QQ', len(packed), len(index)) + packed)
            else:
                output.extend(b'\0' + struct.pack('<Q', len(index)) + index)
            path = Path(name)
            path.write_bytes(output)
            return path
        """;
}
