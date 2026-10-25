using System.Buffers.Binary;
using System.Text;
using AvgTranslator.Core;
using AvgTranslator.Core.Adapters;

namespace AvgTranslator.Tests.Adapters;

public sealed class Sfa16AdapterTests
{
    [Fact]
    public void HuffmanRoundtripsEveryByteAndSingleSymbol()
    {
        foreach (var data in new[] { Array.Empty<byte>(), Enumerable.Repeat((byte)42, 200).ToArray(),
                     Enumerable.Range(0, 256).Select(x => (byte)x).Concat(Enumerable.Repeat((byte)0, 1000)).ToArray() })
            Assert.Equal(data, AosHuffman.Decode(AosHuffman.Encode(data)));
    }

    [Fact]
    public void HuffmanRejectsTruncationAndExcessiveDecodedLength()
    {
        Assert.Throws<InvalidDataException>(() => AosHuffman.Decode([1, 2, 3]));
        var malformed = new byte[1000];
        malformed.AsSpan(4).Fill(255);
        Assert.Throws<InvalidDataException>(() => AosHuffman.Decode(malformed));
        BinaryPrimitives.WriteUInt32LittleEndian(malformed, uint.MaxValue);
        Assert.Throws<InvalidDataException>(() => AosHuffman.Decode(malformed));
    }

    [Fact]
    public void ArchiveRebuildPreservesHeaderGapsTailAndUnchangedPayloads()
    {
        var raw = CreateArchive(("one.dat", [1, 2, 3]), ("two.scr", [4, 5, 6]));
        var archive = AosArchive.Read(raw);
        Assert.Equal(raw, archive.Rebuild(new Dictionary<string, byte[]>()));
        var rebuilt = archive.Rebuild(new Dictionary<string, byte[]> { ["one.dat"] = [9, 8, 7, 6, 5] });
        var result = AosArchive.Read(rebuilt);
        Assert.Equal(new byte[] { 9, 8, 7, 6, 5 }, result.GetBytes(result.Entries[0]));
        Assert.Equal(new byte[] { 4, 5, 6 }, result.GetBytes(result.Entries[1]));
        Assert.Equal(raw.AsSpan(12, AosArchive.IndexOffset - 12).ToArray(), rebuilt.AsSpan(12, AosArchive.IndexOffset - 12).ToArray());
        Assert.Equal(raw[^3..], rebuilt[^3..]);
        Assert.Equal((byte)0xCC, rebuilt[result.Entries[1].Offset - 1]);
    }

    [Fact]
    public void ArchiveRejectsWrongVersionOverlapAndEscapingNames()
    {
        var raw = CreateArchive(("first.scr", [1, 2, 3]), ("second.scr", [4, 5, 6]));
        var version = raw.ToArray(); version[0] = 1;
        Assert.Throws<InvalidDataException>(() => AosArchive.Read(version));
        var overlap = raw.ToArray();
        BinaryPrimitives.WriteUInt32LittleEndian(overlap.AsSpan(AosArchive.IndexOffset + 40 + 32), 1);
        Assert.Throws<InvalidDataException>(() => AosArchive.Read(overlap));
        var escaped = raw.ToArray(); Encoding.ASCII.GetBytes("../oops.scr\0").CopyTo(escaped, AosArchive.IndexOffset);
        Assert.Throws<InvalidDataException>(() => AosArchive.Read(escaped));
    }

    [Fact]
    public void ExtractsNamesNarrativeAndExplicitChoicesWithoutCommandsOrComments()
    {
        const string script = "# 日本語のコメント\r\n[アキ]こんにちは。\\n次の行。\r\n地の文。\r\nbgoff\r\nwndoff # コメント\r\nbgon(\"背景\", 0)\r\nbtnset(1, \"slctwnd\", 0, 0, \"進む\", 50, 30)\r\n$select_text1 = \"戻る\"\r\n$selsub_text1 = \"確定\"\r\n^jp(label) # コメント\r\nvar %flag # コメント\r\n";
        var bytes = Sfa16Script.ShiftJis.GetBytes(script);
        var spans = Sfa16Script.Extract("sample.scr", bytes, Sfa16Script.ShiftJis);
        Assert.Equal(new[] { "アキ", "こんにちは。\\n次の行。", "地の文。", "進む", "戻る", "確定" }, spans.Select(x => x.Unit.SourceText));
        Assert.Equal(new[] { "name", "dialogue", "dialogue", "choice", "choice", "choice" }, spans.Select(x => x.Unit.Kind));
        Assert.Equal("アキ", spans[1].Unit.Speaker);
        Assert.Equal("line 7, choice", spans[3].Unit.Locator);
        Assert.Equal(bytes, Sfa16Script.Rebuild(bytes, spans, new Dictionary<string, string>(), chinese: false));
    }

    [Fact]
    public void InsertingDialogueKeepsExistingIdentityAndOnlyChangesNeighborContext()
    {
        const string before = "[アキ]一番。\r\n二番。\r\n三番。\r\n四番。\r\n";
        const string after = "[アキ]一番。\r\n追加。\r\n二番。\r\n三番。\r\n四番。\r\n";
        var original = Sfa16Script.Extract("scene.scr", Sfa16Script.ShiftJis.GetBytes(before), Sfa16Script.ShiftJis);
        var changed = Sfa16Script.Extract("scene.scr", Sfa16Script.ShiftJis.GetBytes(after), Sfa16Script.ShiftJis);
        foreach (var old in original)
            Assert.Equal(old.Unit.Id, changed.Single(s => s.Unit.SourceText == old.Unit.SourceText).Unit.Id);
        Assert.Equal(original[^1].Unit.Context, changed[^1].Unit.Context);
        Assert.NotEqual(original[2].Unit.Context, changed[3].Unit.Context);
    }

    [Fact]
    public void Cp936RebuildPreservesNonTextBytesAndHandlesBracketTrailByte()
    {
        const string script = "# 日本語のコメント\r\n[アキ]こんにちは。\r\nbtnset(1, \"slctwnd\", 0, 0, \"進む\", 50, 30) # 日本語\r\n";
        var source = Sfa16Script.ShiftJis.GetBytes(script);
        var spans = Sfa16Script.Extract("sample.scr", source, Sfa16Script.ShiftJis);
        var trailBracket = Enumerable.Range(0x4E00, 0x9FFF - 0x4E00).Select(x => ((char)x).ToString())
            .First(x => { try { var bytes = Sfa16Script.Chinese.GetBytes(x); return bytes.Length == 2 && bytes[1] == ']'; } catch (EncoderFallbackException) { return false; } });
        var translations = new Dictionary<string, string>
        {
            [spans[0].Unit.Id] = trailBracket + "名字",
            [spans[1].Unit.Id] = "这是翻译。",
            [spans[2].Unit.Id] = "前进"
        };
        var rebuilt = Sfa16Script.Rebuild(source, spans, translations, chinese: true);
        var recovered = Sfa16Script.Extract("sample.scr", rebuilt, Sfa16Script.Chinese);
        Assert.Equal(translations.Values, recovered.Select(x => x.Unit.SourceText));
        Assert.Equal(source.AsSpan(0, spans[0].Offset).ToArray(), rebuilt.AsSpan(0, recovered[0].Offset).ToArray());
        Assert.True(rebuilt.AsSpan().EndsWith(Sfa16Script.ShiftJis.GetBytes(" # 日本語\r\n")));
    }

    [Theory]
    [InlineData("dialogue", "一行\\n二行", "第一行第二行")]
    [InlineData("dialogue", "原文", "#command")]
    [InlineData("choice", "原文", "\"inject\"")]
    [InlineData("name", "原文", "[name]")]
    [InlineData("dialogue", "原文", "line\nbreak")]
    public void RejectsChangedControlTokensAndScriptInjection(string kind, string source, string translated)
    {
        var unit = new TranslationUnit("test", "sample.scr", "line 1", source, "test", Kind: kind);
        Assert.Throws<InvalidDataException>(() => Sfa16Script.ValidateText(unit, translated));
    }

    [Fact]
    public void RejectsUnrepresentableChineseWithoutReplacementCharacters()
    {
        var source = Sfa16Script.ShiftJis.GetBytes("こんにちは。\r\n");
        var spans = Sfa16Script.Extract("sample.scr", source, Sfa16Script.ShiftJis);
        var translations = new Dictionary<string, string> { [spans[0].Unit.Id] = "表情😀" };
        var issue = Assert.Single(new Sfa16AosV2Adapter().ValidateTranslation(spans[0].Unit, translations[spans[0].Unit.Id]));
        Assert.Equal("unsupported-character", issue.Code);
        Assert.Contains("😀", issue.Message);
        Assert.Contains("U+1F600", issue.Message);
        Assert.Throws<InvalidDataException>(() => Sfa16Script.Rebuild(source, spans, translations, chinese: true));
    }

    [Theory]
    [InlineData("「ん～ふふっ♪」", "「嗯～呵呵♪」", "「嗯～呵呵～」")]
    [InlineData("「ん～ふふっ♪」", "「ん～ふふっ♪」", "「ん～ふふっ～」")]
    [InlineData("名前・名前", "名字・名字", "名字·名字")]
    public void ValidationAndRebuildUseSameDisplaySubstitutionsWithoutChangingTranslation(string original, string translated, string expected)
    {
        var source = Sfa16Script.ShiftJis.GetBytes(original + "\r\n");
        var spans = Sfa16Script.Extract("scene.scr", source, Sfa16Script.ShiftJis);
        var unit = Assert.Single(spans).Unit;
        var translations = new Dictionary<string, string> { [unit.Id] = translated };
        Assert.Empty(new Sfa16AosV2Adapter().ValidateTranslation(unit, translated));
        var rebuilt = Sfa16Script.Rebuild(source, spans, translations, chinese: true);
        Assert.Equal(expected, Assert.Single(Sfa16Script.Extract("scene.scr", rebuilt, Sfa16Script.Chinese)).Unit.SourceText);
        Assert.Equal(translated, translations[unit.Id]);
        Assert.Equal(original, unit.SourceText);
        Assert.True(rebuilt.AsSpan().EndsWith("\r\n"u8));
    }

    [Fact]
    public void RejectsUnknownExecutableWithoutAnyPatch()
    {
        var exe = new byte[800_000];
        Assert.False(Sfa16Executable.IsOriginal(exe));
        Assert.False(Sfa16Executable.IsPatched(exe));
        Assert.Throws<NotSupportedException>(() => Sfa16Executable.Patch(exe));
    }

    [Fact]
    public async Task AdapterNoOpReproducesArchiveAndRejectsUnknownIds()
    {
        using var directory = new TestDirectory();
        var source = Path.Combine(directory.Path, "source");
        var patch = Path.Combine(directory.Path, "patch");
        Directory.CreateDirectory(source);
        var raw = CreateArchive(("sample.scr", AosHuffman.Encode(Sfa16Script.ShiftJis.GetBytes("[アキ]こんにちは。\r\n"))), ("other.dat", [0xDE, 0xAD]));
        await File.WriteAllBytesAsync(Path.Combine(source, "scr.aos"), raw);
        var adapter = new Sfa16AosV2Adapter();
        var extracted = await adapter.ExtractAsync(source);
        var result = await adapter.BuildPatchAsync(source, extracted.Units.ToDictionary(x => x.Id, x => x.SourceText), patch);
        Assert.Equal(new[] { "scr.aos" }, result.Files);
        Assert.Null(result.LaunchExecutable);
        Assert.Equal(raw, await File.ReadAllBytesAsync(Path.Combine(patch, "scr.aos")));
        var validation = await adapter.ValidatePatchAsync(source, patch);
        Assert.True(validation.Passed);
        Assert.Equal("not-tested", validation.RuntimeStatus);
        await Assert.ThrowsAsync<InvalidDataException>(() => adapter.BuildPatchAsync(source,
            new Dictionary<string, string> { ["unknown"] = "翻译" }, Path.Combine(directory.Path, "invalid")));
        Assert.False(Directory.Exists(Path.Combine(directory.Path, "invalid")));
    }

    internal static byte[] CreateArchive(params (string Name, byte[] Data)[] entries)
    {
        var dataOffset = AosArchive.IndexOffset + entries.Length * 40 + 4;
        using var stream = new MemoryStream();
        stream.Write(new byte[dataOffset]);
        var offsets = new List<int>();
        foreach (var entry in entries)
        {
            stream.WriteByte(0xCC); // Deliberate inter-entry gaps must survive a rebuild.
            offsets.Add((int)stream.Position - dataOffset);
            stream.Write(entry.Data);
        }
        stream.Write([0xFA, 0xFB, 0xFC]);
        var raw = stream.ToArray();
        BinaryPrimitives.WriteUInt32LittleEndian(raw.AsSpan(4), (uint)dataOffset);
        BinaryPrimitives.WriteUInt32LittleEndian(raw.AsSpan(8), (uint)(entries.Length * 40));
        raw[200] = 0xEF; // Unknown header metadata.
        for (var i = 0; i < entries.Length; i++)
        {
            var index = AosArchive.IndexOffset + i * 40;
            Encoding.ASCII.GetBytes(entries[i].Name).CopyTo(raw, index);
            BinaryPrimitives.WriteUInt32LittleEndian(raw.AsSpan(index + 32), (uint)offsets[i]);
            BinaryPrimitives.WriteUInt32LittleEndian(raw.AsSpan(index + 36), (uint)entries[i].Data.Length);
        }
        return raw;
    }

    private sealed class TestDirectory : IDisposable
    {
        public string Path { get; } = System.IO.Path.Combine(System.IO.Path.GetTempPath(), "avg-adapter-test-" + Guid.NewGuid().ToString("N"));
        public void Dispose() { if (Directory.Exists(Path)) Directory.Delete(Path, true); }
    }
}
