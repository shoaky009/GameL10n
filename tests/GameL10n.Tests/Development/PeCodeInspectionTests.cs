using System.Buffers.Binary;
using System.Text.Json;
using GameL10n.Core.Development;

namespace GameL10n.Tests.Development;

public sealed class PeCodeInspectionTests
{
    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public void DecodesBothArchitecturesWithCorrectPeAddressMapping(bool x64)
    {
        if (!OperatingSystem.IsWindows() || !Environment.Is64BitProcess) return;
        var imageBase = x64 ? 0x140000000UL : 0x400000UL;
        using var result = JsonDocument.Parse(JsonSerializer.Serialize(PeCodeInspection.Inspect(
            PeImportInspectionTests.Fixture(x64), "va", [new(imageBase + 0x1000, 6), new(imageBase + 0x1010, 6)])));
        var ranges = result.RootElement.GetProperty("ranges");
        Assert.Equal(2, ranges.GetArrayLength());
        var instruction = Assert.Single(ranges[0].GetProperty("instructions").EnumerateArray());
        Assert.Equal("0x1000", instruction.GetProperty("rva").GetString());
        Assert.Equal(0x200, instruction.GetProperty("fileOffset").GetInt32());
        Assert.StartsWith("call ", instruction.GetProperty("instruction").GetString());
        Assert.Equal(6, ranges[0].GetProperty("decodedBytes").GetInt32());
        Assert.Contains(x64 ? "qword ptr [rip" : "dword ptr [", instruction.GetProperty("instruction").GetString());
        Assert.StartsWith("jmp ", Assert.Single(ranges[1].GetProperty("instructions").EnumerateArray()).GetProperty("instruction").GetString());
    }

    [Fact]
    public void ReportsDirectBranchTargetsAndCanContinueFromInstructionLimit()
    {
        if (!OperatingSystem.IsWindows() || !Environment.Is64BitProcess) return;
        var bytes = PeImportInspectionTests.Fixture(false);
        bytes[0x200] = 0xE8;
        BinaryPrimitives.WriteInt32LittleEndian(bytes.AsSpan(0x201), 11);
        bytes[0x205] = 0xC3;
        using var result = JsonDocument.Parse(JsonSerializer.Serialize(PeCodeInspection.Inspect(bytes, "file", [new(0x200, 6)], 1)));
        var range = result.RootElement.GetProperty("ranges")[0];
        Assert.Equal("instruction-limit", range.GetProperty("stopped").GetString());
        Assert.Equal("0x401005", range.GetProperty("nextAddress").GetString());
        Assert.Equal("0x401010", Assert.Single(range.GetProperty("instructions").EnumerateArray()).GetProperty("branchTarget").GetString());
        using var continuation = JsonDocument.Parse(JsonSerializer.Serialize(PeCodeInspection.Inspect(bytes, "rva", [new(0x1005, 1)])));
        Assert.Equal("ret", Assert.Single(continuation.RootElement.GetProperty("ranges")[0].GetProperty("instructions").EnumerateArray()).GetProperty("instruction").GetString());
    }

    [Fact]
    public void CompactListingPreservesEveryInstructionAndAddressMapping()
    {
        if (!OperatingSystem.IsWindows() || !Environment.Is64BitProcess) return;
        var bytes = PeImportInspectionTests.Fixture(false);
        bytes[0x200] = 0xE8;
        BinaryPrimitives.WriteInt32LittleEndian(bytes.AsSpan(0x201), 11);
        bytes.AsSpan(0x205, 95).Fill(0x90);
        var detailed = JsonSerializer.Serialize(PeCodeInspection.Inspect(bytes, "file", [new(0x200, 100)]));
        var compact = JsonSerializer.Serialize(PeCodeInspection.Inspect(bytes, "file", [new(0x200, 100)], compact: true));
        using var full = JsonDocument.Parse(detailed);
        using var small = JsonDocument.Parse(compact);
        var fullRange = full.RootElement.GetProperty("ranges")[0];
        var compactRange = small.RootElement.GetProperty("ranges")[0];
        foreach (var name in new[] { "va", "rva", "fileOffset", "decodedBytes", "nextAddress", "stopped" })
            Assert.Equal(fullRange.GetProperty(name).ToString(), compactRange.GetProperty(name).ToString());
        var expected = fullRange.GetProperty("instructions").EnumerateArray().Select(line =>
            $"{line.GetProperty("va").GetString()} {line.GetProperty("Bytes").GetString()} {line.GetProperty("instruction").GetString()}");
        Assert.Equal(expected, compactRange.GetProperty("instructions").GetString()!.Split('\n'));
        Assert.Contains("call 0x401010", compactRange.GetProperty("instructions").GetString());
        Assert.True(compact.Length < detailed.Length / 2);
    }

    [Fact]
    public void InvalidRangeDoesNotDiscardOtherRangesAndNeverDecodesVirtualPadding()
    {
        if (!OperatingSystem.IsWindows() || !Environment.Is64BitProcess) return;
        using var result = JsonDocument.Parse(JsonSerializer.Serialize(PeCodeInspection.Inspect(
            PeImportInspectionTests.Fixture(false), "rva", [new(0x1000, 6), new(0x2000, 16)])));
        var ranges = result.RootElement.GetProperty("ranges");
        Assert.True(ranges[0].TryGetProperty("instructions", out _));
        Assert.True(ranges[1].TryGetProperty("error", out _));
        Assert.Throws<InvalidDataException>(() => PeCodeInspection.Inspect(PeImportInspectionTests.Fixture(false), "rva", [new(0x1000, 4097)]));
    }
}
