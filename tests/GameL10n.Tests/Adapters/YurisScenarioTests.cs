using System.Buffers.Binary;
using System.Text;
using VNTextPatch.Shared;
using VNTextPatch.Shared.Scripts.Yuris;

namespace GameL10n.Tests.Adapters;

public sealed class YurisScenarioTests : IDisposable
{
    private readonly string root = Path.Combine(Path.GetTempPath(), "gamel10n-yuris-" + Guid.NewGuid().ToString("N"));
    private const uint Key = 0xB0062F41;
    private static readonly string[] Commands = ["WORD", "_", "RETURNCODE", "GOSUB", "CG", "MENU", "STOP"];

    public YurisScenarioTests()
    {
        Directory.CreateDirectory(root);
        Encoding.RegisterProvider(CodePagesEncodingProvider.Instance);
        using var writer = new BinaryWriter(File.Create(Path.Combine(root, "ysc.ybn")));
        writer.Write(0x4D435359);
        writer.Write(480);
        writer.Write(Commands.Length);
        writer.Write(0);
        foreach (var command in Commands)
        {
            writer.Write(Encoding.ASCII.GetBytes(command));
            writer.Write((byte)0);
            writer.Write((byte)0);
        }
    }

    [Fact]
    public void AttributeFreeEncryptedScriptIsEmptyAndRoundtripsEvenWhenParserIsReused()
    {
        var parser = new YurisScenarioScript();
        parser.Load(Location("text.ybn", Scenario()));
        Assert.NotEmpty(parser.GetStrings());
        // A real YSTB shape: one encrypted command and line record, no descriptors or values.
        var empty = Convert.FromHexString("59535442E00100000100000004000000000000000000000004000000000000004D2F06B0412F06B0");
        parser.Load(Location("empty.ybn", empty));
        Assert.Empty(parser.GetStrings());
        parser.WritePatched([], Location("patched.ybn"));
        Assert.Equal(empty, File.ReadAllBytes(Path.Combine(root, "patched.ybn")));
    }

    [Fact]
    public void SelectivePatchKeepsUnchangedControlsExpressionsAndNameBracketsByteExact()
    {
        var original = Scenario();
        var parser = new YurisScenarioScript();
        parser.Load(Location("text.ybn", original));
        var texts = parser.GetStrings().ToArray();
        Assert.Equal(new[] { "アキ", "こんにちは\r\n世界", "進む", "戻る" }, texts.Select(text => text.Text));
        Assert.Equal(texts.Select(text => text.Text), parser.GetStrings().Select(text => text.Text));
        parser.WritePatched(texts, Location("identity.ybn"));
        Assert.Equal(original, File.ReadAllBytes(Path.Combine(root, "identity.ybn")));

        texts[^1] = new("前に進む", texts[^1].Type);
        parser.WritePatched(texts, Location("patched.ybn"));
        var patched = File.ReadAllBytes(Path.Combine(root, "patched.ybn"));
        const int descriptorStart = 32 + 4 * 4;
        const int valueStart = descriptorStart + 4 * 12;
        var lastValueOffset = BinaryPrimitives.ReadInt32LittleEndian(Decrypt(original).AsSpan(descriptorStart + 3 * 12 + 8));
        Assert.Equal(original.AsSpan(descriptorStart, 3 * 12).ToArray(), patched.AsSpan(descriptorStart, 3 * 12).ToArray());
        Assert.Equal(original.AsSpan(valueStart, lastValueOffset).ToArray(), patched.AsSpan(valueStart, lastValueOffset).ToArray());
        parser.Load(Location("patched.ybn"));
        Assert.Equal(texts.Select(text => text.Text), parser.GetStrings().Select(text => text.Text));
    }

    [Theory]
    [InlineData(16, 1)]
    [InlineData(20, int.MaxValue)]
    public void MalformedSectionLengthsAreRejected(int offset, int value)
    {
        var data = Scenario();
        BinaryPrimitives.WriteInt32LittleEndian(data.AsSpan(offset), value);
        Assert.Throws<InvalidDataException>(() => new YurisScenarioScript().Load(Location("bad.ybn", data)));
    }

    private ScriptLocation Location(string name, byte[]? data = null)
    {
        var path = Path.Combine(root, name);
        if (data is not null) File.WriteAllBytes(path, data);
        return ScriptLocation.FromFilePath(path, "Yuris");
    }

    private static byte[] Scenario()
    {
        var sjis = Encoding.GetEncoding(932);
        byte[] Expression(string value)
        {
            var text = sjis.GetBytes(value);
            return [(byte)0x4D, (byte)text.Length, (byte)(text.Length >> 8), .. text];
        }
        byte[][] values = [ [.. sjis.GetBytes("アキ『こんにちは"), 0xEF, 0xF0], sjis.GetBytes("世界』"), Expression("'進む'"), Expression("\"戻る\"") ];
        using var stream = new MemoryStream();
        using var writer = new BinaryWriter(stream);
        writer.Write(0x42545359);
        writer.Write(480);
        writer.Write(4);
        writer.Write(16);
        writer.Write(48);
        writer.Write(values.Sum(value => value.Length));
        writer.Write(16);
        writer.Write(0);
        foreach (byte command in new byte[] { 0, 0, 5, 5 })
        { writer.Write(command); writer.Write((byte)1); writer.Write((short)0); }
        var offset = 0;
        for (var i = 0; i < values.Length; i++)
        {
            writer.Write((short)0); writer.Write((short)(i < 2 ? 0 : 3));
            writer.Write(values[i].Length); writer.Write(offset);
            offset += values[i].Length;
        }
        foreach (var value in values) writer.Write(value);
        for (var i = 0; i < 4; i++) writer.Write(i);
        return Decrypt(stream.ToArray()); // XOR is symmetric; each section starts at key byte zero.
    }

    private static byte[] Decrypt(byte[] original)
    {
        var bytes = original.ToArray();
        var start = 32;
        for (var field = 12; field < 28; field += 4)
        {
            var size = BinaryPrimitives.ReadInt32LittleEndian(bytes.AsSpan(field));
            for (var i = 0; i < size; i++) bytes[start + i] ^= (byte)(Key >> (i % 4 * 8));
            start += size;
        }
        return bytes;
    }

    public void Dispose() => Directory.Delete(root, true);
}
