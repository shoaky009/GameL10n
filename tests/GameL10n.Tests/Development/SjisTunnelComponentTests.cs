using System.Text;
using System.Text.Json;
using GameL10n.Core.Development;

namespace GameL10n.Tests.Development;

public sealed class SjisTunnelComponentTests : IDisposable
{
    private readonly EngineArchiveFixture fixture = new();

    [Fact]
    public async Task ExistingMappingKeepsWireSlotsAndNativeJapaneseBytesWhenNewCharactersAreAdded()
    {
        await CheckApiAsync("""
            saved = '译〜\ue000'.encode('utf-16-le')
            tunnel = Tunnel(saved)
            assert tunnel.serialize() == saved
            assert tunnel.encode('\ue000译〜') == bytes.fromhex('8103 8101 8102')
            native = 'ASCII 日本語\t\r\n ,'
            assert tunnel.encode(native) == native.encode('cp932')
            assert tunnel.decode(tunnel.encode(native)) == native
            assert tunnel.encode('\u0378译') == bytes.fromhex('8104 8101')
            assert tunnel.serialize() == saved + '\u0378'.encode('utf-16-le')
            restored = Tunnel(tunnel.serialize())
            assert restored.decode(bytes.fromhex('8103 8101 8102 8104')) == '\ue000译〜\u0378'
            assert restored.encode('\ue000译〜\u0378') == bytes.fromhex('8103 8101 8102 8104')
            """);
    }

    [Fact]
    public async Task SlotPairsSkipControlBytesAndRoundTripAcrossBothLeadByteRanges()
    {
        await CheckApiAsync("""
            assert SLOTS == 3422
            assert pair(0) == bytes.fromhex('8101')
            assert pair(7) == bytes.fromhex('8108')
            assert pair(8) == bytes.fromhex('810b')
            assert pair(10) == bytes.fromhex('810e')
            assert pair(57) == bytes.fromhex('813f')
            assert pair(58) == bytes.fromhex('8201')
            assert pair(31 * 58 - 1) == bytes.fromhex('9f3f')
            assert pair(31 * 58) == bytes.fromhex('e001')
            assert pair(SLOTS - 1) == bytes.fromhex('fb3f')
            pairs = [pair(index) for index in range(SLOTS)]
            assert len(set(pairs)) == SLOTS
            assert all(0 < value[1] < 0x40 and value[1] not in AVOID for value in pairs)
            chars = ''.join(chr(0x4000 + index) for index in range(SLOTS))
            tunnel = Tunnel(chars.encode('utf-16-le'))
            assert tunnel.decode(b''.join(pairs)) == chars
            for lead in (0x81, 0x9f, 0xe0, 0xfb):
                for tail in AVOID:
                    rejects(lambda lead=lead, tail=tail: tunnel.decode(bytes((lead, tail))))
            for raw in (b'\0', b'\x81', b'\x81\0', b'\x81\x7f', b'\xfc\x01'):
                rejects(lambda raw=raw: tunnel.decode(raw))
            for index in (-1, SLOTS, True):
                rejects(lambda index=index: pair(index))
            """);
    }

    [Fact]
    public async Task InvalidMappingsUnrepresentableUnicodeAndCapacityFailWithoutReplacementOrPartialMapping()
    {
        await CheckApiAsync("""
            for raw in (b'x', b'\0\0', b'\x00\xd8', '𠮷'.encode('utf-16-le'),
                        '译译'.encode('utf-16-le'), b'\x01\x40' * (SLOTS + 1)):
                rejects(lambda raw=raw: Tunnel(raw))
            tunnel = Tunnel()
            for text in ('\u0378\0', '\u0378𠮷', '\u0378\ud800'):
                rejects(lambda text=text: tunnel.encode(text))
                assert tunnel.serialize() == b''
            # These CP932 aliases must use the tunnel, never silently turn into backslash or fullwidth tilde.
            assert tunnel.encode('¥〜') == bytes.fromhex('8101 8102')
            assert tunnel.decode(tunnel.encode('¥〜')) == '¥〜'
            assert tunnel.decode(b'\\') == '\\'
            filled = ''.join(chr(0x4000 + index) for index in range(SLOTS))
            full = Tunnel(filled.encode('utf-16-le'))
            before = full.serialize()
            rejects(lambda: full.encode('\u0378'))
            assert full.serialize() == before
            assert full.decode(pair(SLOTS - 1)) == filled[-1]
            almost = Tunnel(filled[:-1].encode('utf-16-le'))
            before = almost.serialize()
            rejects(lambda: almost.encode('\u0378\u0379'))
            assert almost.serialize() == before
            assert almost.encode('\u0378') == pair(SLOTS - 1)
            """);
    }

    [Fact]
    public async Task JsonCheckIsReadOnlyCompactAndExplicitAboutWhatRoundTripDoesNotVerify()
    {
        var component = await CopyComponentAsync();
        var mapping = Path.Combine(fixture.Root, "sjis_ext.bin");
        var saved = Encoding.Unicode.GetBytes("译");
        await File.WriteAllBytesAsync(mapping, saved);
        var files = Directory.GetFiles(fixture.Root, "*", SearchOption.AllDirectories).Order().ToArray();
        var run = await InvokeAsync(component, new { operation = "check", texts = new[] { "译〜 ASCII 日本語", "〜" }, mappingInput = mapping });
        Assert.True(run.ExitCode == 0, run.StandardOutput + run.StandardError);
        using var json = JsonDocument.Parse(run.StandardOutput);
        var result = json.RootElement;
        Assert.True(result.GetProperty("success").GetBoolean());
        Assert.Equal("ok", result.GetProperty("status").GetString());
        Assert.Equal(1, result.GetProperty("neededSlots").GetInt32());
        Assert.Equal(3420, result.GetProperty("remainingSlots").GetInt32());
        Assert.All(result.GetProperty("checks").EnumerateObject(), check => Assert.True(check.Value.GetBoolean()));
        Assert.False(result.GetProperty("runtimeTested").GetBoolean());
        Assert.Contains("character traversal", result.GetProperty("limit").GetString());
        Assert.Contains("font", result.GetProperty("limit").GetString());
        Assert.Contains("side-loading", result.GetProperty("limit").GetString());
        Assert.False(result.TryGetProperty("mapping", out _));
        Assert.False(result.TryGetProperty("texts", out _));
        Assert.False(result.TryGetProperty("bytes", out _));
        Assert.True(run.StandardOutput.Length < 1000);
        Assert.Equal(saved, await File.ReadAllBytesAsync(mapping));
        Assert.Equal(files, Directory.GetFiles(fixture.Root, "*", SearchOption.AllDirectories).Order().ToArray());

        run = await InvokeAsync(component, new { operation = "verify", texts = new[] { "\0" } });
        Assert.Equal(1, run.ExitCode);
        using var invalid = JsonDocument.Parse(run.StandardOutput);
        Assert.Equal("rejected", invalid.RootElement.GetProperty("status").GetString());
        Assert.False(invalid.RootElement.GetProperty("checks").GetProperty("characters").GetBoolean());
        Assert.False(invalid.RootElement.GetProperty("runtimeTested").GetBoolean());

        await File.WriteAllBytesAsync(mapping, Enumerable.Range(0x4000, 3422)
            .SelectMany(code => new[] { (byte)code, (byte)(code >> 8) }).ToArray());
        run = await InvokeAsync(component, new { operation = "check", texts = new[] { "\u0378" }, mappingInput = mapping });
        Assert.Equal(1, run.ExitCode);
        using var exhausted = JsonDocument.Parse(run.StandardOutput);
        Assert.False(exhausted.RootElement.GetProperty("checks").GetProperty("capacity").GetBoolean());
        Assert.Equal(1, exhausted.RootElement.GetProperty("neededSlots").GetInt32());
        Assert.Equal(0, exhausted.RootElement.GetProperty("remainingSlots").GetInt32());
    }

    private async Task CheckApiAsync(string check)
    {
        var component = await CopyComponentAsync();
        var script = Path.Combine(fixture.Root, "check.py");
        await File.WriteAllTextAsync(script, """
            import importlib.util, sys
            spec = importlib.util.spec_from_file_location('sjis_tunnel', sys.argv[1])
            module = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(module)
            Tunnel, pair, SLOTS, AVOID = module.Tunnel, module.pair, module.SLOTS, module.AVOID
            def rejects(action):
                try:
                    action()
                except ValueError:
                    return
                raise AssertionError('Expected explicit rejection')
            """ + "\n" + check + "\nprint('sjis-tunnel-ok')\n");
        var run = await new AdapterProcessRunner().RunAsync(new(fixture.Paths.Python,
            EngineReferenceToolkit.PythonArguments(fixture.Paths, script, [component]), fixture.Root), default);
        Assert.True(run.ExitCode == 0, run.StandardOutput + run.StandardError);
        Assert.Equal("sjis-tunnel-ok", run.StandardOutput.Trim());
    }

    private async Task<string> CopyComponentAsync()
    {
        var release = new List<AdapterReleaseFile>();
        await new AdapterComponentAssets().CopyAsync(["sjis-tunnel"], fixture.Root, releaseFiles: release);
        Assert.Equal("tools/sjis_tunnel.py", Assert.Single(release).Path);
        return Path.Combine(fixture.Root, "src", "tools", "sjis_tunnel.py");
    }

    private Task<AdapterProcessResult> InvokeAsync(string component, object request) =>
        new AdapterProcessRunner().RunAsync(new(fixture.Paths.Python,
            EngineReferenceToolkit.PythonArguments(fixture.Paths, component, []), fixture.Root,
            JsonSerializer.Serialize(request)), default);

    public void Dispose() => fixture.Dispose();
}
