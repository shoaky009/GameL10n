using System;
using System.IO;
using System.Linq;
using System.Reflection.PortableExecutable;
using System.Security.Cryptography;

// Profiles identify executable content, never a game title or installation path.
// A different runtime build needs its own reviewed profile; offsets must not be guessed.
static class RuntimePatch
{
    const string CodeSha256 = "002309409cea45ffa79458c7f868a59465f734ceb6bd7fe0cc7a1304ca34d216";
    const string CodePrefixSha256 = "e50ebe9f6f9ee2f649177e5bbfc43286039b99b91736eabdc63639824a97ddce";
    public static readonly int[] RangeLimits = {
        0x1865f, 0x189b0, 0x18b8e, 0x18bfb, 0x18c5a, 0x2707f, 0x27476, 0x310fe,
        0x3f0d9, 0x3f17c, 0x3f4ec, 0x3f642, 0x3f6c7, 0x3f812, 0x3f897, 0x3f9d4,
        0x3fe49, 0x3fed3, 0x3ff63, 0x447d3, 0x81ff2, 0x82287, 0x83a8c, 0x842fa,
        0x846f4, 0x847a4, 0x8492a, 0x84e33, 0x84f54, 0x84fe3, 0x85c61, 0xb166f,
        0xb17df, 0xfa2a0, 0x10e748, 0x127061, 0x1544f9, 0x159099, 0x159698, 0x15973a,
        0x1599ac, 0x1599e2, 0x159dc4, 0x159ec2, 0x15a245, 0x15a5e5, 0x15af41, 0x15b0a9,
        0x16f4f4, 0x16f95c, 0x16fa07, 0x16fd1c, 0x16fd52, 0x1702c2, 0x1703b1, 0x170519,
        0x1ba646, 0x1bd67f, 0x2542fb, 0x265327, 0x2737b4, 0x273ea5, 0x2744d4, 0x279bc6,
        0x279be8, 0x279c31, 0x279c59, 0x281d60, 0x281ff0, 0x282110, 0x2821c6, 0x282280,
        0x28250e, 0x2826ee, 0x282891, 0x282a57, 0x282adf, 0x283bcf, 0x283fad, 0x2845da,
        0x28484a, 0x284f8e, 0x292a20, 0x295155, 0x296ad0, 0x296b63, 0x297115, 0x2978a6,
        0x29791d, 0x2d42ac, 0x2d601a, 0x2d6a2d, 0x2d6a91, 0x2d6b59, 0x324c91, 0x328d62
    };

    // Probe receives only the first 64 KiB. The complete code-section hash is required
    // at build time; game-specific icons/version resources do not identify the runtime.
    public static bool MatchesHeader(string path)
    {
        using var stream = File.OpenRead(path);
        if (stream.Length < 65536) return false;
        var header = new byte[65536];
        stream.ReadExactly(header);
        try
        {
            var code = CodeSection(header);
            return code.PointerToRawData >= 0 && code.PointerToRawData <= header.Length - 32768 &&
                Convert.ToHexStringLower(SHA256.HashData(header.AsSpan(code.PointerToRawData, 32768))) == CodePrefixSha256;
        }
        catch (BadImageFormatException) { return false; }
        catch (InvalidDataException) { return false; }
    }

    static SectionHeader CodeSection(byte[] bytes)
    {
        using var reader = new PEReader(new MemoryStream(bytes));
        var headers = reader.PEHeaders;
        var sections = headers.SectionHeaders.Where(s => s.Name == ".text").ToArray();
        if (headers.CoffHeader.Machine != Machine.I386 || headers.PEHeader?.ImageBase != 0x400000 ||
            sections.Length != 1 || sections[0].VirtualAddress != 0x1000 ||
            sections[0].VirtualSize != 3893792 || sections[0].SizeOfRawData != 3894272 ||
            (sections[0].SectionCharacteristics & SectionCharacteristics.MemExecute) == 0)
            throw new InvalidDataException("此 System4 运行程序布局尚无已验证的中文修补规则。");
        return sections[0];
    }

    public static int RequireOriginal(byte[] bytes)
    {
        var code = CodeSection(bytes);
        if (code.PointerToRawData < 0 || code.PointerToRawData > bytes.Length - code.SizeOfRawData ||
            Convert.ToHexStringLower(SHA256.HashData(bytes.AsSpan(code.PointerToRawData, code.SizeOfRawData))) != CodeSha256)
            throw new InvalidDataException("此 System4 运行程序版本尚无已验证的中文修补规则，请继续智能适配以分析该版本。");
        return code.PointerToRawData;
    }

    public static byte[] Build(byte[] original)
    {
        int codeOffset = RequireOriginal(original);
        var result = (byte[])original.Clone();
        // 95 inlined Shift-JIS lead-byte tests and the FNL glyph-index range test.
        // E0..EF becomes E0..F9. Instruction sizes, branches and PE layout are unchanged.
        foreach (int referenceOffset in RangeLimits)
        {
            int offset = referenceOffset - 0x400 + codeOffset;
            if (result[offset] != 0x0f) throw new InvalidDataException("运行程序的码位检查指令不匹配。");
            result[offset] = 0x19;
        }
        return result;
    }
}
