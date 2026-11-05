using System.Buffers.Binary;
using System.Reflection.PortableExecutable;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json.Nodes;
using GameL10n.Core.Serialization;

namespace GameL10n.Core.Development;

/// <summary>Static import evidence. Byte-pattern references deliberately do not claim decoded instruction boundaries.</summary>
internal static class PeImportInspection
{
    private const int MaximumBytes = 64 * 1024 * 1024;
    private sealed record Import(string Library, string Name, int IatRva);

    internal static async Task<object> InspectAsync(string path, string query, int offset, int limit, CancellationToken ct)
    {
        await using var input = File.OpenRead(path);
        if (input.Length > MaximumBytes) throw new InvalidDataException("PE 分析文件超过 64 MiB，请选择实际游戏程序或相关 DLL。");
        var bytes = new byte[(int)input.Length];
        await input.ReadExactlyAsync(bytes, ct);
        return Inspect(bytes, query, offset, limit, ct);
    }

    internal static object Inspect(byte[] bytes, string query, int offset = 0, int limit = 20, CancellationToken ct = default)
    {
        if (bytes.Length > MaximumBytes || query.Length > 500 || offset is < 0 or > 8192 || limit is < 1 or > 40)
            throw new InvalidDataException("PE 查询超出允许范围。");
        using var reader = new PEReader(new MemoryStream(bytes, writable: false));
        var image = reader.PEHeaders;
        var header = image.PEHeader ?? throw new InvalidDataException("缺少 PE 可选头。");
        var pointerSize = header.Magic switch { PEMagic.PE32 => 4, PEMagic.PE32Plus => 8, _ => throw new InvalidDataException("未知 PE 位数。") };
        foreach (var section in image.SectionHeaders)
            if (section.PointerToRawData < 0 || section.SizeOfRawData < 0 || section.VirtualAddress < 0 || section.VirtualSize < 0
                || (long)section.PointerToRawData + section.SizeOfRawData > bytes.Length)
                throw new InvalidDataException("PE 节范围超出文件。");

        int FileOffset(long rva, int size)
        {
            if (rva < 0 || rva > int.MaxValue || size < 0) throw new InvalidDataException("无效的 PE RVA。");
            if (rva + size <= Math.Min(header.SizeOfHeaders, bytes.Length)) return (int)rva;
            foreach (var section in image.SectionHeaders)
            {
                var relative = rva - section.VirtualAddress;
                if (relative >= 0 && relative + size <= section.SizeOfRawData)
                    return checked(section.PointerToRawData + (int)relative);
            }
            throw new InvalidDataException($"PE RVA 0x{rva:X} 没有对应的文件数据。");
        }
        uint UInt32(long rva) => BinaryPrimitives.ReadUInt32LittleEndian(bytes.AsSpan(FileOffset(rva, 4), 4));
        string Ascii(long rva)
        {
            var value = new StringBuilder();
            for (int i = 0; i < 512; i++)
            {
                var character = bytes[FileOffset(rva + i, 1)];
                if (character == 0) return value.ToString();
                if (character is < 32 or > 126) throw new InvalidDataException("PE 导入名称不是有效 ASCII 文本。");
                value.Append((char)character);
            }
            throw new InvalidDataException("PE 导入名称过长或缺少结束符。");
        }

        var imports = new List<Import>();
        var warnings = new List<string>();
        var directory = header.ImportTableDirectory;
        bool partial = false, terminated = directory.Size == 0;
        for (int descriptor = 0; directory.Size > 0 && descriptor < Math.Min(256, directory.Size / 20); descriptor++)
        {
            ct.ThrowIfCancellationRequested();
            long rva = (long)directory.RelativeVirtualAddress + descriptor * 20;
            var lookup = UInt32(rva);
            var timestamp = UInt32(rva + 4);
            var forwarder = UInt32(rva + 8);
            var name = UInt32(rva + 12);
            var iat = UInt32(rva + 16);
            if ((lookup | timestamp | forwarder | name | iat) == 0) { terminated = true; break; }
            if (name == 0 || iat == 0) throw new InvalidDataException("PE 导入描述符缺少名称或地址表。");
            var library = Ascii(name);
            if (lookup == 0 && timestamp != 0)
            {
                partial = true;
                if (warnings.Count < 8) warnings.Add(library + " 缺少原始名称表且已有绑定地址，未猜测函数名称。");
                continue;
            }
            if (lookup == 0) lookup = iat;
            bool thunkTerminated = false;
            for (int index = 0; index < 8192; index++)
            {
                var position = FileOffset((long)lookup + index * pointerSize, pointerSize);
                ulong entry = pointerSize == 4 ? BinaryPrimitives.ReadUInt32LittleEndian(bytes.AsSpan(position, 4))
                    : BinaryPrimitives.ReadUInt64LittleEndian(bytes.AsSpan(position, 8));
                if (entry == 0) { thunkTerminated = true; break; }
                var ordinalFlag = pointerSize == 4 ? 0x80000000UL : 0x8000000000000000UL;
                if ((entry & ordinalFlag) == 0 && entry > int.MaxValue) throw new InvalidDataException("PE 导入名称 RVA 无效。");
                var function = (entry & ordinalFlag) != 0 ? "#" + (entry & 0xffff) : Ascii((long)entry + 2);
                long slot = (long)iat + index * pointerSize;
                _ = FileOffset(slot, pointerSize);
                imports.Add(new(library, function, checked((int)slot)));
                if (imports.Count >= 8192) break;
            }
            if (!thunkTerminated) partial = true;
            if (imports.Count >= 8192) break;
        }
        partial |= !terminated;
        if (!terminated) warnings.Add("导入表未在扫描上限内结束，结果不完整。");
        if (header.DelayImportTableDirectory.Size > 0)
        {
            partial = true;
            warnings.Add("存在延迟导入表，本次只解析普通导入表。");
        }
        var queries = query.Split(',', StringSplitOptions.TrimEntries | StringSplitOptions.RemoveEmptyEntries);
        var matches = imports.Where(item => queries.Length == 0 || queries.Any(term =>
            item.Name.Contains(term, StringComparison.OrdinalIgnoreCase) || item.Library.Contains(term, StringComparison.OrdinalIgnoreCase))).ToArray();
        var selected = matches.Skip(offset).Take(limit).ToArray();
        var sites = selected.Select(item => item.IatRva).Distinct().ToDictionary(rva => rva, _ => new List<object>());
        var counts = sites.Keys.ToDictionary(rva => rva, _ => 0);
        bool x86 = image.CoffHeader.Machine == Machine.I386 && pointerSize == 4;
        bool x64 = image.CoffHeader.Machine == Machine.Amd64 && pointerSize == 8;
        if (sites.Count > 0 && (x86 || x64))
        {
            foreach (var section in image.SectionHeaders.Where(section => (section.SectionCharacteristics & SectionCharacteristics.MemExecute) != 0))
            {
                var size = Math.Min(section.VirtualSize, section.SizeOfRawData);
                for (int index = 0; index <= size - 6; index++)
                {
                    if ((index & 0x3fff) == 0) ct.ThrowIfCancellationRequested();
                    int position = section.PointerToRawData + index;
                    if (bytes[position] != 0xff || bytes[position + 1] is not (0x15 or 0x25)) continue;
                    long rva = (long)section.VirtualAddress + index;
                    long target = x86
                        ? (long)BinaryPrimitives.ReadUInt32LittleEndian(bytes.AsSpan(position + 2, 4)) - (long)header.ImageBase
                        : rva + 6 + BinaryPrimitives.ReadInt32LittleEndian(bytes.AsSpan(position + 2, 4));
                    if (target < 0 || target > int.MaxValue || !sites.TryGetValue((int)target, out var references)) continue;
                    counts[(int)target]++;
                    if (references.Count >= 4) continue;
                    var start = Math.Max(0, index - 8);
                    var end = Math.Min(size, index + 16);
                    references.Add(new JsonObject
                    {
                        ["rva"] = rva,
                        ["fileOffset"] = position,
                        ["kind"] = bytes[position + 1] == 0x15 ? "call-pattern" : "jump-pattern",
                        ["contextRva"] = (long)section.VirtualAddress + start,
                        ["contextHex"] = Convert.ToHexString(bytes.AsSpan(section.PointerToRawData + start, end - start))
                    });
                }
            }
        }
        return new JsonObject
        {
            ["architecture"] = image.CoffHeader.Machine.ToString(),
            ["imageBase"] = header.ImageBase,
            ["sha256"] = Convert.ToHexStringLower(SHA256.HashData(bytes)),
            ["totalImports"] = imports.Count,
            ["totalMatches"] = matches.Length,
            ["offset"] = offset,
            ["more"] = offset + selected.Length < matches.Length,
            ["nextOffset"] = offset + selected.Length < matches.Length ? (int?)(offset + selected.Length) : null,
            ["partial"] = partial,
            ["imports"] = EngineJson.Array(selected.Select(item => new JsonObject
            {
                ["library"] = item.Library,
                ["name"] = item.Name,
                ["iatRva"] = item.IatRva,
                ["iatFileOffset"] = FileOffset(item.IatRva, pointerSize),
                ["candidateReferences"] = EngineJson.Node((sites[item.IatRva]).ToArray(), EngineJson.Web),
                ["candidateReferenceCount"] = counts[item.IatRva]
            })),
            ["warnings"] = EngineJson.Node((warnings).ToArray(), EngineJson.Web),
            ["scope"] = "普通 PE 导入表为结构证据；调用位置仅匹配可执行节中的 x86/x64 FF15/FF25 字节模式，尚未确认指令边界、可达性和实际执行。其他寻址方式及动态解析不在引用扫描范围内；零命中不代表函数未使用。请围绕候选位置做有界反汇编，验证参数、字符遍历和字体路径，不能据此宣称游戏中文显示通过。"
        };
    }
}
