using System.Buffers.Binary;
using System.Text;
using System.Text.Json;
using AvgTranslator.Core.Development;

namespace AvgTranslator.Tests.Development;

public sealed class UnityTextReferenceInspectionTests
{
    private static JsonElement Inspect(byte[] metadata, byte[] pe, string query, string kind = "literal", int offset = 0, int limit = 4) =>
        JsonSerializer.SerializeToElement(UnityTextReferenceInspection.Inspect(metadata, pe, query, kind, offset, limit));

    [Fact]
    public void ShowsSharedConsumersAndDoesNotAttributeUnknownFunctionsToPreviousMethod()
    {
        var (metadata, pe) = Fixture();
        var result = Inspect(metadata, pe, "Shared");
        var literal = Assert.Single(result.GetProperty("literals").EnumerateArray());
        Assert.Equal(new[] { "UI.Text.Display", "UI.Text.GetBinding" }, literal.GetProperty("consumers").EnumerateArray()
            .Select(c => c.GetProperty("name").GetString()));
        Assert.Equal(1, literal.GetProperty("unmappedReferenceCount").GetInt32());
        Assert.Equal("candidate-references", result.GetProperty("status").GetString());
        Assert.False(result.GetProperty("more").GetBoolean());
    }

    [Fact]
    public void MethodQueryIncludesChainedFunctionFragmentsAndPagesTheWholeTextGroup()
    {
        var (metadata, pe) = Fixture();
        var first = Inspect(metadata, pe, "UI.Text.Display", "method", limit: 1);
        Assert.Equal(2, first.GetProperty("matched").GetInt32());
        Assert.True(first.GetProperty("more").GetBoolean());
        var next = Inspect(metadata, pe, "UI.Text.Display", "method", offset: 1, limit: 1);
        var literal = Assert.Single(next.GetProperty("literals").EnumerateArray());
        Assert.Equal("Visible only", literal.GetProperty("text").GetString());
        Assert.Equal(0, literal.GetProperty("unmappedReferenceCount").GetInt32());
        Assert.Equal("0x140001000", Assert.Single(literal.GetProperty("consumers").EnumerateArray()).GetProperty("methodVa").GetString());
        Assert.False(next.GetProperty("more").GetBoolean());
    }

    [Fact]
    public void MissingReferenceIsNotPresentedAsAnApprovedTranslation()
    {
        var (metadata, pe) = Fixture();
        pe.AsSpan(0x220, 7).Clear();
        var result = Inspect(metadata, pe, "Visible only");
        var literal = Assert.Single(result.GetProperty("literals").EnumerateArray());
        Assert.Empty(literal.GetProperty("consumers").EnumerateArray());
        Assert.Equal(0, literal.GetProperty("candidateReferenceCount").GetInt32());
        Assert.Contains("不能证明", result.GetProperty("notice").GetString());
    }

    [Fact]
    public void RejectsUnsupportedVersionsInvalidLiteralRangesAndAmbiguousModules()
    {
        var (metadata, pe) = Fixture();
        W(metadata, 4, 29);
        Assert.Throws<InvalidDataException>(() => Inspect(metadata, pe, "Shared"));
        W(metadata, 4, 31);
        W(metadata, 0x100, int.MaxValue);
        Assert.Throws<InvalidDataException>(() => Inspect(metadata, pe, "Shared"));
        (metadata, pe) = Fixture();
        pe.AsSpan(0x480, 24).CopyTo(pe.AsSpan(0x4b0));
        BinaryPrimitives.WriteUInt64LittleEndian(pe.AsSpan(0x4c0), 0x140002120);
        Assert.Throws<InvalidDataException>(() => Inspect(metadata, pe, "Shared"));
    }

    [Fact]
    public void RejectsUnboundedRequestsAndHonorsCancellation()
    {
        var (metadata, pe) = Fixture();
        Assert.Throws<ArgumentException>(() => Inspect(metadata, pe, ""));
        Assert.Throws<ArgumentException>(() => Inspect(metadata, pe, "Shared", limit: 100));
        Assert.Throws<OperationCanceledException>(() => UnityTextReferenceInspection.Inspect(metadata, pe, "Shared", ct: new CancellationToken(true)));
    }

    private static void W(byte[] b, int at, int value) => BinaryPrimitives.WriteInt32LittleEndian(b.AsSpan(at, 4), value);

    private static (byte[] Metadata, byte[] Pe) Fixture()
    {
        var m = new byte[0x500];
        W(m, 0, unchecked((int)0xFAB11BAF)); W(m, 4, 31);
        W(m, 8, 0x100); W(m, 12, 16); W(m, 16, 0x110); W(m, 20, 18);
        W(m, 0x100, 6); W(m, 0x104, 0); W(m, 0x108, 12); W(m, 0x10c, 6);
        Encoding.UTF8.GetBytes("SharedVisible only").CopyTo(m, 0x110);
        int stringBytes = 0;
        int Name(string value)
        {
            int index = stringBytes;
            var bytes = Encoding.UTF8.GetBytes(value + "\0"); bytes.CopyTo(m, 0x300 + stringBytes); stringBytes += bytes.Length;
            return index;
        }
        int module = Name("Assembly-CSharp.dll"), type = Name("Text"), ns = Name("UI"), display = Name("Display"), binding = Name("GetBinding");
        W(m, 24, 0x300); W(m, 28, stringBytes);
        W(m, 160, 0x200); W(m, 164, 88);
        W(m, 0x200, type); W(m, 0x204, ns); W(m, 0x208, 10); W(m, 0x20c, -1); W(m, 0x210, -1);
        W(m, 0x224, 0); BinaryPrimitives.WriteUInt16LittleEndian(m.AsSpan(0x240), 2);
        W(m, 48, 0x260); W(m, 52, 72);
        W(m, 0x260, display); W(m, 0x278, 0x06000001); W(m, 0x284, binding); W(m, 0x29c, 0x06000002);
        W(m, 168, 0x400); W(m, 172, 40); W(m, 0x400, module); W(m, 0x408, 0); W(m, 0x40c, 1);

        var p = new byte[0xe00];
        void U16(int at, ushort value) => BinaryPrimitives.WriteUInt16LittleEndian(p.AsSpan(at), value);
        void U64(int at, ulong value) => BinaryPrimitives.WriteUInt64LittleEndian(p.AsSpan(at), value);
        void Text(int at, string value) => Encoding.ASCII.GetBytes(value).CopyTo(p, at);
        Text(0, "MZ"); W(p, 0x3c, 0x80); Text(0x80, "PE\0\0"); U16(0x84, 0x8664); U16(0x86, 2); U16(0x94, 0xf0); U16(0x96, 0x102);
        const int optional = 0x98;
        U16(optional, 0x20b); U64(optional + 24, 0x140000000); W(p, optional + 32, 0x1000); W(p, optional + 36, 0x200);
        W(p, optional + 56, 0x3000); W(p, optional + 60, 0x200); W(p, optional + 108, 16);
        W(p, optional + 112 + 24, 0x2300); W(p, optional + 116 + 24, 48);
        void Section(int at, string name, int rva, int file, int size, int flags)
        { Text(at, name); W(p, at + 8, size); W(p, at + 12, rva); W(p, at + 16, size); W(p, at + 20, file); W(p, at + 36, flags); }
        Section(0x188, ".text", 0x1000, 0x200, 0x200, 0x60000020);
        Section(0x1b0, ".data", 0x2000, 0x400, 0xa00, 0x40000040);
        Text(0x400, "Assembly-CSharp.dll\0"); U64(0x480, 0x140002000); W(p, 0x488, 2); U64(0x490, 0x140002100);
        U64(0x500, 0x140001000); U64(0x508, 0x140001040); U64(0x540, 0xa0000001); U64(0x548, 0xa0000003);
        void Reference(int at, int slot)
        { p[at] = 0x48; p[at + 1] = 0x8d; p[at + 2] = 0x0d; W(p, at + 3, slot - (0x1000 + at - 0x200 + 7)); }
        Reference(0x200, 0x2140); Reference(0x220, 0x2148); Reference(0x240, 0x2140); Reference(0x260, 0x2140);
        void Function(int at, int start, int end, int unwind)
        { W(p, at, start); W(p, at + 4, end); W(p, at + 8, unwind); }
        Function(0x700, 0x1000, 0x1020, 0x2380); Function(0x70c, 0x1020, 0x1030, 0x2390);
        Function(0x718, 0x1040, 0x1050, 0x2380); Function(0x724, 0x1060, 0x1070, 0x2380);
        p[0x780] = 1; p[0x790] = 0x21; Function(0x794, 0x1000, 0x1020, 0x2380);
        return (m, p);
    }
}
