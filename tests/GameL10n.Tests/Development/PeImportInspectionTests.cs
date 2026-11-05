using System.Buffers.Binary;
using System.Text;
using System.Text.Json;
using GameL10n.Core.Development;

namespace GameL10n.Tests.Development;

public sealed class PeImportInspectionTests
{
    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public void ResolvesRealImportsAndArchitectureSpecificReferencesInsteadOfMatchingApiNameStrings(bool x64)
    {
        var bytes = Fixture(x64);
        using var json = JsonDocument.Parse(JsonSerializer.Serialize(PeImportInspection.Inspect(bytes, "Glyph")));
        var root = json.RootElement;
        Assert.False(root.GetProperty("partial").GetBoolean());
        var import = Assert.Single(root.GetProperty("imports").EnumerateArray());
        Assert.Equal("GetGlyphOutlineA", import.GetProperty("name").GetString());
        Assert.Equal(0x1180, import.GetProperty("iatRva").GetInt32());
        Assert.Equal(0x380, import.GetProperty("iatFileOffset").GetInt32());
        Assert.Equal(2, import.GetProperty("candidateReferenceCount").GetInt32());
        Assert.Equal(new[] { 0x1000, 0x1010 }, import.GetProperty("candidateReferences").EnumerateArray()
            .Select(site => site.GetProperty("rva").GetInt32()));
        using var absent = JsonDocument.Parse(JsonSerializer.Serialize(PeImportInspection.Inspect(bytes, "TextOutA")));
        Assert.Empty(absent.RootElement.GetProperty("imports").EnumerateArray());
    }

    [Fact]
    public void RejectsUnmappedImportNamesAndKeepsOutputBounded()
    {
        var bytes = Fixture(false);
        BinaryPrimitives.WriteUInt32LittleEndian(bytes.AsSpan(0x30c), 0x7ffff000);
        Assert.Throws<InvalidDataException>(() => PeImportInspection.Inspect(bytes, ""));
        using var result = JsonDocument.Parse(JsonSerializer.Serialize(PeImportInspection.Inspect(Fixture(false), "", offset: 1)));
        Assert.Empty(result.RootElement.GetProperty("imports").EnumerateArray());
        Assert.False(result.RootElement.GetProperty("more").GetBoolean());
    }

    internal static byte[] Fixture(bool x64)
    {
        var bytes = new byte[0x600];
        void U16(int offset, ushort value) => BinaryPrimitives.WriteUInt16LittleEndian(bytes.AsSpan(offset), value);
        void U32(int offset, uint value) => BinaryPrimitives.WriteUInt32LittleEndian(bytes.AsSpan(offset), value);
        void Text(int offset, string value) => Encoding.ASCII.GetBytes(value).CopyTo(bytes, offset);
        Text(0, "MZ"); U32(0x3c, 0x80); Text(0x80, "PE\0\0");
        U16(0x84, (ushort)(x64 ? 0x8664 : 0x14c)); U16(0x86, 1);
        int optionalSize = x64 ? 0xf0 : 0xe0;
        U16(0x94, (ushort)optionalSize); U16(0x96, 0x102);
        const int optional = 0x98;
        U16(optional, (ushort)(x64 ? 0x20b : 0x10b)); U32(optional + 16, 0x1000);
        if (x64) BinaryPrimitives.WriteUInt64LittleEndian(bytes.AsSpan(optional + 24), 0x140000000);
        else U32(optional + 28, 0x400000);
        U32(optional + 32, 0x1000); U32(optional + 36, 0x200);
        U32(optional + 56, 0x2000); U32(optional + 60, 0x200); U16(optional + 68, 3);
        int directories = optional + (x64 ? 112 : 96);
        U32(directories - 4, 16); U32(directories + 8, 0x1100); U32(directories + 12, 40);
        int section = optional + optionalSize;
        Text(section, ".text"); U32(section + 8, 0x400); U32(section + 12, 0x1000);
        U32(section + 16, 0x400); U32(section + 20, 0x200); U32(section + 36, 0x60000020);
        U32(0x300, 0x1160); U32(0x30c, 0x1150); U32(0x310, 0x1180);
        Text(0x350, "GDI32.dll\0"); U32(0x360, 0x11a0); U32(0x380, 0x11a0);
        Text(0x3a2, "GetGlyphOutlineA\0"); Text(0x440, "TextOutA\0");
        foreach (int position in new[] { 0x200, 0x210 })
        {
            bytes[position] = 0xff; bytes[position + 1] = position == 0x200 ? (byte)0x15 : (byte)0x25;
            U32(position + 2, x64 ? (uint)(0x1180 - (position - 0x200 + 0x1000 + 6)) : 0x401180);
        }
        return bytes;
    }
}
