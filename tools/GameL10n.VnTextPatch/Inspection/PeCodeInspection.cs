using System.Globalization;
using System.Reflection.PortableExecutable;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text.Json.Nodes;
using GameL10n.Core.Serialization;

namespace GameL10n.Core.Development;

internal sealed record PeCodeRange(ulong Address, int Length);

/// <summary>Bounded linear decoding from explicit addresses, not a control-flow or execution proof.</summary>
internal static class PeCodeInspection
{
    private const int MaximumBytes = 64 * 1024 * 1024;
    private static readonly Lazy<Capstone> Decoder = new(() => new());

    internal static async Task<object> InspectAsync(string path, string addressKind, IReadOnlyList<PeCodeRange> ranges,
        int instructionLimit, CancellationToken ct, bool compact = false)
    {
        await using var file = File.OpenRead(path);
        if (file.Length > MaximumBytes) throw new InvalidDataException("PE 分析文件超过 64 MiB。");
        var bytes = new byte[(int)file.Length];
        await file.ReadExactlyAsync(bytes, ct);
        return Inspect(bytes, addressKind, ranges, instructionLimit, ct, compact);
    }

    internal static object Inspect(byte[] bytes, string addressKind, IReadOnlyList<PeCodeRange> ranges,
        int instructionLimit = 80, CancellationToken ct = default, bool compact = false)
    {
        if (bytes.Length > MaximumBytes || ranges.Count is < 1 or > 8
            || ranges.Any(range => range.Length is < 1 or > 4096) || ranges.Sum(range => range.Length) > 16384
            || instructionLimit is < 1 or > 200 || addressKind is not ("va" or "rva" or "file"))
            throw new InvalidDataException("请提供 1–8 个代码范围，每段 1–4096 字节、合计不超过 16384 字节，每段最多 200 条指令；addressKind 为 va/rva/file。");
        using var pe = new PEReader(new MemoryStream(bytes, writable: false));
        var image = pe.PEHeaders;
        var header = image.PEHeader ?? throw new InvalidDataException("缺少 PE 可选头。");
        var bitness = image.CoffHeader.Machine switch
        {
            Machine.I386 when header.Magic == PEMagic.PE32 => 32,
            Machine.Amd64 when header.Magic == PEMagic.PE32Plus => 64,
            _ => throw new InvalidDataException("代码分析当前支持 x86/x64 PE，不能用此解码器解释其他架构。")
        };
        foreach (var section in image.SectionHeaders)
            if (section.PointerToRawData < 0 || section.SizeOfRawData < 0 || section.VirtualAddress < 0 || section.VirtualSize < 0
                || (long)section.PointerToRawData + section.SizeOfRawData > bytes.Length)
                throw new InvalidDataException("PE 节范围超出文件。");

        var output = new List<object>();
        var failedRanges = 0;
        foreach (var range in ranges)
        {
            ct.ThrowIfCancellationRequested();
            try
            {
                if (addressKind == "va" && range.Address < header.ImageBase)
                    throw new InvalidDataException("虚拟地址小于 PE imageBase。");
                var position = addressKind == "va" ? range.Address - header.ImageBase : range.Address;
                var found = image.SectionHeaders.Where(section =>
                {
                    var start = (ulong)(addressKind == "file" ? section.PointerToRawData : section.VirtualAddress);
                    return position >= start && position - start < (ulong)section.SizeOfRawData;
                }).ToArray();
                if (found.Length != 1) throw new InvalidDataException("地址没有唯一的 PE 节文件映射，可能是零填充区域或错误地址类型。");
                var section = found[0];
                if (!section.SectionCharacteristics.HasFlag(SectionCharacteristics.MemExecute))
                    throw new InvalidDataException("所选地址不在可执行节；请先确认代码位置。");
                var delta = position - (ulong)(addressKind == "file" ? section.PointerToRawData : section.VirtualAddress);
                var rva = checked((ulong)section.VirtualAddress + delta);
                var offset = checked(section.PointerToRawData + (int)delta);
                var address = checked(header.ImageBase + rva);
                var available = Math.Min(range.Length, section.SizeOfRawData - (int)delta);
                var decoded = Decoder.Value.Decode(bytes.AsSpan(offset, available).ToArray(), bitness, address, instructionLimit, ct);
                var consumed = decoded.Sum(line => line.Size);
                // Range bases retain all address mappings; repeating them for every instruction crowds out useful context.
                object instructions = compact
                    ? string.Join('\n', decoded.Select(line => $"{Hex(line.Address)} {line.Bytes} {line.Mnemonic} {line.Operands}".TrimEnd()))
                    : decoded.Select(line => new JsonObject
                    {
                        ["va"] = Hex(line.Address),
                        ["rva"] = Hex(line.Address - header.ImageBase),
                        ["fileOffset"] = offset + (long)(line.Address - address),
                        ["bytes"] = line.Bytes,
                        ["instruction"] = line.Mnemonic + (line.Operands.Length == 0 ? "" : " " + line.Operands),
                        ["branchTarget"] = (line.Mnemonic is "call" or "jmp" || line.Mnemonic.StartsWith('j'))
                            && line.Operands.StartsWith("0x", StringComparison.Ordinal)
                            && ulong.TryParse(line.Operands.AsSpan(2), NumberStyles.HexNumber, CultureInfo.InvariantCulture, out var target) ? Hex(target) : null
                    }).ToArray();
                output.Add(new JsonObject
                {
                    ["requestedAddress"] = Hex(range.Address),
                    ["addressKind"] = addressKind,
                    ["section"] = section.Name,
                    ["va"] = Hex(address),
                    ["rva"] = Hex(rva),
                    ["fileOffset"] = offset,
                    ["requestedBytes"] = range.Length,
                    ["availableBytes"] = available,
                    ["decodedBytes"] = consumed,
                    ["stopped"] = consumed == available ? "range-end" : decoded.Count >= instructionLimit ? "instruction-limit" : "undecodable-or-truncated",
                    ["nextAddress"] = Hex(address + (ulong)consumed),
                    ["instructions"] = EngineJson.Node(instructions, EngineJson.Web)
                });
            }
            catch (Exception error) when (error is InvalidDataException or OverflowException)
            {
                failedRanges++;
                output.Add(new JsonObject
                {
                    ["requestedAddress"] = Hex(range.Address),
                    ["addressKind"] = addressKind,
                    ["error"] = error.Message
                });
            }
        }
        return new JsonObject
        {
            ["architecture"] = bitness == 32 ? "x86" : "x64",
            ["imageBase"] = Hex(header.ImageBase),
            ["sha256"] = Convert.ToHexStringLower(SHA256.HashData(bytes)),
            ["decoder"] = "Capstone 5.0.6",
            ["ranges"] = EngineJson.Node((output).ToArray(), EngineJson.Web),
            ["instructionFormat"] = compact
                ? "Each line: VA HEX_BYTES INSTRUCTION. Branch targets are in operands. RVA = VA - imageBase; file offset = range.fileOffset + (VA - range.va). nextAddress is always VA."
                : "Per-instruction VA, RVA, file offset, bytes, instruction and direct branch target.",
            ["status"] = failedRanges == ranges.Count ? "failed" : "completed",
            ["failedRanges"] = failedRanges,
            ["error"] = failedRanges == ranges.Count ? "所选代码范围均无法分析，请核对地址类型及各范围的错误。" : null,
            ["scope"] = "从给定起点线性反汇编，正确处理 PE 地址映射；起点仍须有调用目标或其他证据支持。分支目标不代表可达性，未运行游戏，也未验证函数参数、动态调用或中文显示。"
        };
    }

    private static string Hex(ulong value) => "0x" + value.ToString("X", CultureInfo.InvariantCulture);
    private sealed record Decoded(ulong Address, int Size, string Bytes, string Mnemonic, string Operands);

    private sealed class Capstone
    {
        private readonly Open open;
        private readonly Disassemble disassemble;
        private readonly Free free;
        private readonly Close close;
        private readonly Error error;

        internal Capstone()
        {
            if (!OperatingSystem.IsWindows() || RuntimeInformation.ProcessArchitecture != Architecture.X64)
                throw new PlatformNotSupportedException("内置反汇编工具需要 Windows x64 宿主；可分析 x86 和 x64 游戏。");
            var path = Path.Combine(AppContext.BaseDirectory, "capstone.dll");
            if (!File.Exists(path)) throw new FileNotFoundException("缺少内置反汇编库，请使用完整发行包。", path);
            using var input = File.OpenRead(path);
            if (Convert.ToHexStringLower(SHA256.HashData(input)) != "651096794b3c3c2a1df24345e22844a0311fca56a4f74ea1cfa29107983760e1")
                throw new InvalidDataException("内置 Capstone 校验失败。");
            var library = NativeLibrary.Load(path);
            T Load<T>(string name) where T : Delegate => Marshal.GetDelegateForFunctionPointer<T>(NativeLibrary.GetExport(library, name));
            try
            {
                open = Load<Open>("cs_open"); disassemble = Load<Disassemble>("cs_disasm");
                free = Load<Free>("cs_free"); close = Load<Close>("cs_close"); error = Load<Error>("cs_errno");
                _ = Load<Version>("cs_version")(out var major, out var minor);
                if (major != 5 || minor != 0 || Marshal.SizeOf<Instruction>() != 248)
                    throw new InvalidDataException("Capstone ABI 不兼容。");
            }
            catch { NativeLibrary.Free(library); throw; }
            // The pinned module stays loaded for the application's lifetime; each decoder handle is per call.
        }

        internal List<Decoded> Decode(byte[] code, int bitness, ulong address, int limit, CancellationToken ct)
        {
            if (open(3, bitness == 32 ? 4 : 8, out var handle) != 0) throw new InvalidDataException("Capstone 无法创建 x86 解码器。");
            try
            {
                var count = disassemble(handle, code, (nuint)code.Length, address, (nuint)limit, out var instructions);
                try
                {
                    if (count > (nuint)limit) throw new InvalidDataException("反汇编指令数量超出限制。");
                    if (count == 0 && error(handle) != 0) throw new InvalidDataException("Capstone 解码失败。");
                    var output = new List<Decoded>((int)count);
                    for (var i = 0; i < (int)count; i++)
                    {
                        ct.ThrowIfCancellationRequested();
                        var item = Marshal.PtrToStructure<Instruction>(instructions + i * 248);
                        if (item.Size is < 1 or > 15 || item.Address < address || item.Address - address + item.Size > (ulong)code.Length)
                            throw new InvalidDataException("解码指令范围无效。");
                        output.Add(new(item.Address, item.Size, Convert.ToHexString(item.Bytes.AsSpan(0, item.Size)), item.Mnemonic, item.Operands));
                    }
                    return output;
                }
                finally { if (instructions != 0) free(instructions, count); }
            }
            finally { close(ref handle); }
        }

        [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Ansi)]
        private struct Instruction
        {
            public uint Id;
            public ulong Address;
            public ushort Size;
            [MarshalAs(UnmanagedType.ByValArray, SizeConst = 24)] public byte[] Bytes;
            [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)] public string Mnemonic;
            [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 160)] public string Operands;
            public nint Detail;
        }
        [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate int Open(int architecture, int mode, out nuint handle);
        [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate nuint Disassemble(nuint handle, [In] byte[] code, nuint length, ulong address, nuint count, out nint instructions);
        [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate void Free(nint instructions, nuint count);
        [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate int Close(ref nuint handle);
        [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate int Error(nuint handle);
        [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate int Version(out int major, out int minor);
    }
}
