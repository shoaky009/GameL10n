using System.Buffers.Binary;
using System.Reflection.PortableExecutable;
using System.Security.Cryptography;
using System.Text;

namespace GameL10n.Core.Development;

/// <summary>On-demand IL2CPP investigation. Candidate references are not an automatic translation allowlist.</summary>
public static class UnityTextReferenceInspection
{
    private const int MaximumBytes = 128 * 1024 * 1024;
    private sealed record Reference(ulong Address, ulong Slot, ulong? MethodAddress);
    private sealed record Function(ulong Start, ulong End, ulong Owner);

    public static async Task<object> InspectAsync(string metadataPath, string assemblyPath, string query,
        string queryKind = "literal", int offset = 0, int limit = 4, CancellationToken ct = default)
    {
        // Only invoked by the analysis tool; normal discovery/translation never scans native code.
        var metadata = await ReadAsync(metadataPath, ct);
        var assembly = await ReadAsync(assemblyPath, ct);
        return Inspect(metadata, assembly, query, queryKind, offset, limit, ct);
    }

    private static async Task<byte[]> ReadAsync(string path, CancellationToken ct)
    {
        await using var stream = File.OpenRead(path);
        if (stream.Length is < 8 or > MaximumBytes) throw new InvalidDataException("Unity 分析文件必须在 8 字节到 128 MiB 之间。");
        var bytes = new byte[(int)stream.Length];
        await stream.ReadExactlyAsync(bytes, ct);
        return bytes;
    }

    internal static object Inspect(byte[] metadata, byte[] assembly, string query, string queryKind = "literal",
        int offset = 0, int limit = 4, CancellationToken ct = default)
    {
        if (string.IsNullOrWhiteSpace(query) || query.Length > 256 || offset < 0 || limit is < 1 or > 8
            || queryKind is not ("literal" or "method")) throw new ArgumentException("请指定文字或方法名，查询每页最多 8 条。");
        if (metadata.Length is < 176 or > MaximumBytes || assembly.Length is < 64 or > MaximumBytes)
            throw new InvalidDataException("Unity 分析文件范围无效。");
        int I(int at) => BinaryPrimitives.ReadInt32LittleEndian(metadata.AsSpan(at, 4));
        if (I(0) != unchecked((int)0xFAB11BAF) || I(4) != 31)
            throw new InvalidDataException("原生文字引用分析目前仅支持未加密的 IL2CPP v31 元数据。");
        (int Start, int Count) Table(int header, int stride)
        {
            int start = I(header), size = I(header + 4);
            if (start < 176 || size < 0 || size % stride != 0 || start + (long)size > metadata.Length)
                throw new InvalidDataException("IL2CPP 表范围无效：" + header);
            return (start, size / stride);
        }
        var strings = Table(24, 1);
        var literalTable = Table(8, 8);
        var pool = Table(16, 1);
        var types = Table(160, 88);
        var methods = Table(48, 36);
        var images = Table(168, 40);
        if (literalTable.Count > 500_000 || types.Count > 200_000 || methods.Count > 2_000_000 || images.Count > 2048)
            throw new InvalidDataException("IL2CPP 表数量超出分析范围。");
        var utf8 = new UTF8Encoding(false, true);
        string S(int index)
        {
            if ((uint)index >= strings.Count) throw new InvalidDataException("IL2CPP 名称索引越界。");
            int start = strings.Start + index, end = Array.IndexOf(metadata, (byte)0, start, Math.Min(strings.Count - index, 4096));
            if (end < 0) throw new InvalidDataException("IL2CPP 名称未终止。");
            return utf8.GetString(metadata, start, end - start);
        }
        var literals = new string[literalTable.Count];
        long decodedBytes = 0;
        for (int i = 0; i < literals.Length; i++)
        {
            ct.ThrowIfCancellationRequested();
            int p = literalTable.Start + i * 8, length = I(p), at = I(p + 4);
            if (length < 0 || at < 0 || at + (long)length > pool.Count) throw new InvalidDataException("IL2CPP 文案范围无效。");
            if ((decodedBytes += length) > MaximumBytes) throw new InvalidDataException("IL2CPP 文案总量超出分析范围。");
            literals[i] = utf8.GetString(metadata, pool.Start + at, length);
        }

        using var pe = new PEReader(new MemoryStream(assembly, writable: false));
        var headers = pe.PEHeaders;
        if (headers.CoffHeader.Machine != Machine.Amd64 || headers.PEHeader?.Magic != PEMagic.PE32Plus)
            throw new InvalidDataException("原生文字引用分析目前支持 Windows x64 GameAssembly；其他架构请使用匹配的分析工具。");
        ulong imageBase = headers.PEHeader.ImageBase;
        var sections = headers.SectionHeaders.ToArray();
        foreach (var section in sections)
            if (section.PointerToRawData < 0 || section.SizeOfRawData < 0 || section.VirtualAddress < 0
                || section.PointerToRawData + (long)section.SizeOfRawData > assembly.Length)
                throw new InvalidDataException("PE 节范围无效。");
        bool Executable(ulong address) => address >= imageBase && sections.Any(s =>
            s.SectionCharacteristics.HasFlag(SectionCharacteristics.MemExecute) && address - imageBase >= (ulong)s.VirtualAddress
            && address - imageBase - (ulong)s.VirtualAddress < (ulong)s.SizeOfRawData);
        int At(ulong address, int size = 1)
        {
            if (address < imageBase || size < 0) return -1;
            ulong rva = address - imageBase;
            foreach (var section in sections)
                if (rva >= (ulong)section.VirtualAddress && rva - (ulong)section.VirtualAddress + (ulong)size <= (ulong)section.SizeOfRawData)
                    return checked(section.PointerToRawData + (int)(rva - (ulong)section.VirtualAddress));
            return -1;
        }
        ulong U64(int at) => BinaryPrimitives.ReadUInt64LittleEndian(assembly.AsSpan(at, 8));
        uint U32(int at) => BinaryPrimitives.ReadUInt32LittleEndian(assembly.AsSpan(at, 4));
        var dataSections = sections.Where(s => !s.SectionCharacteristics.HasFlag(SectionCharacteristics.MemExecute)).ToArray();
        var byValue = new Dictionary<int, int>();
        for (int i = 0; i < types.Count; i++)
            if (!byValue.TryAdd(I(types.Start + i * 88 + 8), types.Start + i * 88)) throw new InvalidDataException("IL2CPP 类型索引重复。");
        string TypeName(int row, int depth = 0)
        {
            if (depth > 16) throw new InvalidDataException("IL2CPP 嵌套类型关系无效。");
            string name = S(I(row));
            if (I(row + 12) >= 0 && byValue.TryGetValue(I(row + 12), out int parent)) return TypeName(parent, depth + 1) + "+" + name;
            string ns = S(I(row + 4));
            return ns.Length == 0 ? name : ns + "." + name;
        }
        var moduleNames = new Dictionary<ulong, string>();
        var imageMethods = new Dictionary<string, List<(int Row, string Name)>>(StringComparer.Ordinal);
        var visitedTypes = new HashSet<int>();
        for (int i = 0; i < images.Count; i++)
        {
            ct.ThrowIfCancellationRequested();
            int p = images.Start + i * 40, start = I(p + 8), count = I(p + 12);
            if (start < 0 || count < 0 || start + (long)count > types.Count) throw new InvalidDataException("IL2CPP 镜像类型范围无效。");
            string name = S(I(p));
            var rows = new List<(int, string)>();
            for (int t = start; t < start + count; t++)
            {
                if (!visitedTypes.Add(t)) throw new InvalidDataException("IL2CPP 镜像类型范围重叠。");
                int row = types.Start + t * 88, first = I(row + 36), total = BinaryPrimitives.ReadUInt16LittleEndian(metadata.AsSpan(row + 64, 2));
                if (total > 0 && (first < 0 || first + (long)total > methods.Count)) throw new InvalidDataException("IL2CPP 方法范围无效。");
                string type = TypeName(row);
                for (int m = 0; m < total; m++)
                {
                    int method = methods.Start + (first + m) * 36, token = I(method + 24);
                    if ((token & unchecked((int)0xff000000)) != 0x06000000 || (token & 0xffffff) == 0)
                        throw new InvalidDataException("IL2CPP 方法 token 无效。");
                    rows.Add(((token & 0xffffff) - 1, type + "." + S(I(method))));
                }
            }
            if (!imageMethods.TryAdd(name, rows)) throw new InvalidDataException("IL2CPP 镜像名称重复。");
            var needle = utf8.GetBytes(name + "\0");
            foreach (var section in dataSections)
                for (int at = section.PointerToRawData, end = at + section.SizeOfRawData; at < end;)
                {
                    int found = assembly.AsSpan(at, end - at).IndexOf(needle);
                    if (found < 0) break;
                    at += found;
                    moduleNames[imageBase + (ulong)(section.VirtualAddress + at - section.PointerToRawData)] = name;
                    at += needle.Length;
                }
        }
        var modules = new Dictionary<string, HashSet<ulong>>(StringComparer.Ordinal);
        foreach (var section in dataSections)
            for (int at = section.PointerToRawData; at + 24 <= section.PointerToRawData + section.SizeOfRawData; at += 8)
            {
                if ((at & 0xffff) == 0) ct.ThrowIfCancellationRequested();
                if (!moduleNames.TryGetValue(U64(at), out var name)) continue;
                int count = (int)U32(at + 8);
                var rows = imageMethods[name];
                if (count <= 0 || rows.Count == 0 || count != rows.Max(m => m.Row) + 1 || At(U64(at + 16), checked(count * 8)) < 0) continue;
                if (!modules.TryGetValue(name, out var candidates)) modules[name] = candidates = [];
                candidates.Add(U64(at + 16));
            }
        var methodNames = new SortedDictionary<ulong, List<string>>();
        foreach (var (name, candidates) in modules.Where(p => p.Value.Count == 1))
        {
            int array = At(candidates.Single());
            foreach (var (row, methodName) in imageMethods[name])
            {
                ulong address = U64(array + row * 8);
                if (!Executable(address)) continue;
                if (!methodNames.TryGetValue(address, out var names)) methodNames[address] = names = [];
                names.Add(methodName);
            }
        }
        if (methodNames.Count == 0) throw new InvalidDataException("未匹配到 IL2CPP 方法注册表，请确认元数据和 GameAssembly 来自同一版本，或使用对应工具分析自定义布局。");
        var methodAddresses = methodNames.Keys.ToArray();
        var slots = new Dictionary<ulong, int>();
        foreach (var section in dataSections)
            for (int at = section.PointerToRawData; at + 8 <= section.PointerToRawData + section.SizeOfRawData; at += 8)
            {
                if ((at & 0xffff) == 0) ct.ThrowIfCancellationRequested();
                ulong value = U64(at);
                if ((value & 0xffffffffe0000001UL) == 0xa0000001UL && ((value & 0x1ffffffe) >> 1) < (ulong)literals.Length)
                    slots[imageBase + (ulong)(section.VirtualAddress + at - section.PointerToRawData)] = (int)((value & 0x1ffffffe) >> 1);
            }
        ulong Owner(uint start, uint unwind, int depth = 0)
        {
            if (depth > 16) throw new InvalidDataException("PE unwind 链过深或循环。");
            int at = At(imageBase + (unwind & ~1U), 4);
            if (at < 0) return 0;
            if ((assembly[at] >> 3 & 4) == 0) return imageBase + start;
            int chainOffset = (4 + assembly[at + 2] * 2 + 3) & ~3;
            int chain = At(imageBase + (unwind & ~1U) + (ulong)chainOffset, 12);
            return chain < 0 ? 0 : Owner(U32(chain), U32(chain + 8), depth + 1);
        }
        var exception = headers.PEHeader.ExceptionTableDirectory;
        int exceptionAt = At(imageBase + (ulong)exception.RelativeVirtualAddress, exception.Size);
        if (exception.Size < 0 || exception.Size % 12 != 0 || exceptionAt < 0) throw new InvalidDataException("PE unwind 表无效。");
        var functions = new List<Function>();
        for (int i = 0; i < exception.Size; i += 12)
        {
            int at = exceptionAt + i;
            ulong start = imageBase + U32(at), end = imageBase + U32(at + 4);
            if (start >= end || !Executable(start) || !Executable(end - 1)) throw new InvalidDataException("PE 函数范围无效。");
            functions.Add(new(start, end, Owner(U32(at), U32(at + 8))));
        }
        functions.Sort((a, b) => a.Start.CompareTo(b.Start));
        var starts = functions.Select(f => f.Start).ToArray();
        var references = new Dictionary<int, List<Reference>>();
        foreach (var section in sections.Where(s => s.SectionCharacteristics.HasFlag(SectionCharacteristics.MemExecute)))
            for (int at = section.PointerToRawData; at + 7 <= section.PointerToRawData + section.SizeOfRawData; at++)
            {
                if ((at & 0xffff) == 0) ct.ThrowIfCancellationRequested();
                if (assembly[at] is not (0x48 or 0x4c) || assembly[at + 1] is not (0x8b or 0x8d) || (assembly[at + 2] & 0xc7) != 5) continue;
                ulong address = imageBase + (ulong)(section.VirtualAddress + at - section.PointerToRawData);
                ulong target = unchecked((ulong)((long)address + 7 + BinaryPrimitives.ReadInt32LittleEndian(assembly.AsSpan(at + 3, 4))));
                if (!slots.TryGetValue(target, out int index)) continue;
                int m = Preceding(methodAddresses, address), f = Preceding(starts, address);
                ulong? method = m >= 0 && f >= 0 && address < functions[f].End && functions[f].Owner != 0
                    && (methodAddresses[m] == functions[f].Owner || methodAddresses[m] >= functions[f].Start) ? methodAddresses[m] : null;
                if (!references.TryGetValue(index, out var sites)) references[index] = sites = [];
                sites.Add(new(address, target, method));
            }
        var matchedMethods = queryKind == "method" ? methodNames.Where(p => p.Value.Any(n => n.Contains(query, StringComparison.OrdinalIgnoreCase)))
            .Select(p => p.Key).ToHashSet() : [];
        var matches = Enumerable.Range(0, literals.Length).Where(i => queryKind == "literal"
            ? literals[i].Contains(query, StringComparison.OrdinalIgnoreCase)
            : references.TryGetValue(i, out var sites) && sites.Any(r => r.MethodAddress is { } a && matchedMethods.Contains(a))).ToArray();
        var page = matches.Skip(offset).Take(limit).Select(index =>
        {
            var sites = references.GetValueOrDefault(index) ?? [];
            var consumers = sites.Where(r => r.MethodAddress.HasValue).GroupBy(r => r.MethodAddress!.Value)
                .SelectMany(g => methodNames[g.Key].Select(name => new { name, methodVa = Hex(g.Key), referenceVa = Hex(g.First().Address), slotVa = Hex(g.First().Slot) }))
                .OrderBy(c => queryKind == "method" && c.name.Contains(query, StringComparison.OrdinalIgnoreCase) ? 0 : 1).ThenBy(c => c.name, StringComparer.Ordinal).ToArray();
            return new { index, text = literals[index].Length > 1200 ? literals[index][..1200] : literals[index], textTruncated = literals[index].Length > 1200,
                candidateReferenceCount = sites.Count, consumerCount = consumers.Length, consumers = consumers.Take(12).ToArray(), consumersTruncated = consumers.Length > 12,
                unmappedReferenceCount = sites.Count(r => r.MethodAddress is null),
                unmappedReferenceVas = sites.Where(r => r.MethodAddress is null).Take(3).Select(r => Hex(r.Address)).ToArray() };
        }).ToArray();
        return new { status = "candidate-references", format = "il2cpp", version = 31, architecture = "x64", queryKind,
            metadataSha256 = Convert.ToHexStringLower(SHA256.HashData(metadata)), assemblySha256 = Convert.ToHexStringLower(SHA256.HashData(assembly)),
            guidance = UnitySchemaInspection.Guidance,
            mappedModules = modules.Count(p => p.Value.Count == 1), totalModules = images.Count, mappedMethods = methodNames.Count,
            matchedMethodAddresses = matchedMethods.Count, matched = matches.Length, offset, more = offset + page.Length < matches.Length,
            nextOffset = offset + page.Length, literals = page,
            notice = "仅识别标准 v31 模块注册表及 x64 RIP 相对 mov/lea 候选字节模式；未完整反汇编、未解析全部泛型/间接引用，也未执行游戏。无引用或仅显示方法命中不能证明可安全替换。",
            nextAction = "用 method 查询检查整组显示函数及其辅助函数，再用 inspect_pe_code 核对调用和用途。检查共用词的其他消费者，区分显示文字与枚举/输入/查找键；未知或截断的引用需继续核实。只将审核过的文字加入对应版本适配器，不改通用翻译规则。" };
    }

    private static int Preceding(ulong[] values, ulong address)
    {
        int index = Array.BinarySearch(values, address);
        return index >= 0 ? index : ~index - 1;
    }
    private static string Hex(ulong address) => "0x" + address.ToString("X");
}
