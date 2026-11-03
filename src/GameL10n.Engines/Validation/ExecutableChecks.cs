using System.Buffers.Binary;
using System.Reflection.PortableExecutable;

namespace GameL10n.Core.Validation;

public sealed record ExecutableAnalysis(string FileName, string Architecture, ulong PreferredImageBase,
    bool DynamicBase, bool HasRelocations, IReadOnlyList<string> Constraints);

/// <summary>Host checks for PE loading hazards; passing these checks never establishes runtime compatibility.</summary>
public static class ExecutableChecks
{
    public static ExecutableAnalysis Analyze(string fileName, byte[] bytes)
    {
        var image = ReadImage(bytes);
        var header = image.PEHeader!;
        var dynamicBase = (header.DllCharacteristics & DllCharacteristics.DynamicBase) != 0;
        var relocations = ReadRelocations(bytes, image);
        var constraints = new List<string>
        {
            "静态结构检查、成功启动、中文显示与剧情运行必须分别验证，不能相互替代。",
            "中文启动程序和对应资源必须一起安装；生成目录通常不包含运行游戏所需的全部资源。"
        };
        if (dynamicBase)
            constraints.Add("EXE 启用 ASLR：改写绝对地址指令时必须同步处理重定位；新增代码使用位置无关寻址或提供正确的重定位项。不得通过关闭 ASLR 回避问题。");
        return new(fileName, image.CoffHeader.Machine.ToString(), header.ImageBase, dynamicBase,
            relocations.Count != 0, constraints);
    }

    public static ValidationResult Validate(byte[]? original, byte[] output)
    {
        var issues = new List<ValidationIssue>();
        try
        {
            var after = ReadImage(output);
            var header = after.PEHeader!;
            if (!IsExecutableRva(after, header.AddressOfEntryPoint))
                issues.Add(new("pe-entry-point", "程序入口不在已映射的可执行节内。"));
            var relocations = ReadRelocations(output, after);
            if (original is null) return new(issues.Count == 0, issues);
            var before = ReadImage(original);
            var originalHeader = before.PEHeader!;
            if ((originalHeader.DllCharacteristics & DllCharacteristics.DynamicBase) != 0 &&
                (header.DllCharacteristics & DllCharacteristics.DynamicBase) == 0)
                issues.Add(new("pe-aslr-disabled", "补丁关闭了原程序的 ASLR，不能作为重定位修复交付。"));

            // Native x86 instruction checks apply to modifications retaining the original layout.
            if (originalHeader.Magic != PEMagic.PE32 || header.Magic != PEMagic.PE32 ||
                before.CoffHeader.Machine != after.CoffHeader.Machine ||
                originalHeader.AddressOfEntryPoint != header.AddressOfEntryPoint)
                return new(issues.Count == 0, issues);
            foreach (var entry in ReadRelocations(original, before).Where(entry => entry.Type == 3))
            {
                var start = entry.Rva - 2;
                var oldOffset = FileOffset(before, original, start, 6);
                var newOffset = FileOffset(after, output, start, 6);
                if (oldOffset < 0 || newOffset < 0 || !IsExecutableRva(before, start) ||
                    original[oldOffset] != 0xFF || original[oldOffset + 1] != 0x15 ||
                    output[newOffset] is not (0xE8 or 0xE9) || output[newOffset + 5] != 0x90) continue;
                if (relocations.Any(relocation => relocation.Type != 0 && relocation.Rva < start + 6 &&
                    relocation.Rva + (relocation.Type == 10 ? 8 : 4) > start))
                    issues.Add(new("pe-relocation-overwrites-branch",
                        $"RVA 0x{start:X} 的绝对调用已改为相对跳转，但重定位仍会修改指令，随机地址加载时可能启动崩溃。"));
                var target = (long)start + 5 + BinaryPrimitives.ReadInt32LittleEndian(output.AsSpan(newOffset + 1));
                if (target < 0 || target > int.MaxValue || !IsExecutableRva(after, (int)target))
                    issues.Add(new("pe-branch-target", $"RVA 0x{start:X} 的新跳转目标不在已映射的可执行代码内。"));
            }
            if ((header.DllCharacteristics & DllCharacteristics.DynamicBase) != 0)
            {
                foreach (var section in after.SectionHeaders.Where(section =>
                    (section.SectionCharacteristics & SectionCharacteristics.MemExecute) != 0))
                {
                    var size = Math.Min(section.VirtualSize, section.SizeOfRawData);
                    for (var offset = 0; offset <= size - 6; offset++)
                    {
                        var rva = section.VirtualAddress + offset;
                        var position = FileOffset(after, output, rva, 6);
                        if (position < 0 || output[position] != 0xFF || output[position + 1] is not (0x15 or 0x25)) continue;
                        var oldPosition = FileOffset(before, original, rva, 6);
                        if (oldPosition < 0 || original.AsSpan(oldPosition, 6).ContainsAnyExcept((byte)0)) continue;
                        var address = BinaryPrimitives.ReadUInt32LittleEndian(output.AsSpan(position + 2));
                        if (address < header.ImageBase || address - header.ImageBase >= (ulong)header.SizeOfImage) continue;
                        if (!relocations.Any(entry => entry.Type == 3 && entry.Rva == rva + 2))
                            issues.Add(new("pe-new-absolute-address", $"新增代码 RVA 0x{rva:X} 使用固定的程序内地址，却没有对应重定位；ASLR 加载后地址将失效。"));
                    }
                }
            }
        }
        catch (Exception error) when (error is BadImageFormatException or InvalidDataException or OverflowException)
        {
            issues.Add(new("pe-invalid-image", "可执行文件结构无效：" + error.Message));
        }
        return new(issues.Count == 0, issues);
    }

    private static PEHeaders ReadImage(byte[] bytes)
    {
        using var reader = new PEReader(new MemoryStream(bytes, writable: false));
        var headers = reader.PEHeaders;
        if (headers.PEHeader is null) throw new InvalidDataException("缺少 PE 可选头。");
        foreach (var section in headers.SectionHeaders)
            if (section.PointerToRawData < 0 || section.SizeOfRawData < 0 ||
                (long)section.PointerToRawData + section.SizeOfRawData > bytes.Length)
                throw new InvalidDataException("PE 节超出文件边界。");
        return headers;
    }

    private static bool IsExecutableRva(PEHeaders image, int rva) => image.SectionHeaders.Any(section =>
        (section.SectionCharacteristics & SectionCharacteristics.MemExecute) != 0 && rva >= section.VirtualAddress &&
        (long)rva < (long)section.VirtualAddress + Math.Min(section.VirtualSize, section.SizeOfRawData));

    private static int FileOffset(PEHeaders image, byte[] bytes, int rva, int length)
    {
        foreach (var section in image.SectionHeaders)
        {
            var relative = (long)rva - section.VirtualAddress;
            var position = section.PointerToRawData + relative;
            if (relative >= 0 && relative + length <= section.SizeOfRawData && position >= 0 && position + length <= bytes.Length)
                return (int)position;
        }
        return -1;
    }

    private static List<(int Rva, int Type)> ReadRelocations(byte[] bytes, PEHeaders image)
    {
        var directory = image.PEHeader!.BaseRelocationTableDirectory;
        var result = new List<(int, int)>();
        if (directory.Size == 0) return result;
        var start = FileOffset(image, bytes, directory.RelativeVirtualAddress, directory.Size);
        if (start < 0) throw new InvalidDataException("重定位表超出 PE 节范围。");
        var end = checked(start + directory.Size);
        for (var cursor = start; cursor < end;)
        {
            if (end - cursor < 8) throw new InvalidDataException("重定位块头被截断。");
            var page = BinaryPrimitives.ReadInt32LittleEndian(bytes.AsSpan(cursor));
            var size = BinaryPrimitives.ReadInt32LittleEndian(bytes.AsSpan(cursor + 4));
            if (page < 0 || size < 8 || (size & 1) != 0 || size > end - cursor)
                throw new InvalidDataException("重定位块尺寸无效。");
            for (var entry = cursor + 8; entry < cursor + size; entry += 2)
            {
                var value = BinaryPrimitives.ReadUInt16LittleEndian(bytes.AsSpan(entry));
                var type = value >> 12;
                if (type != 0) result.Add((checked(page + (value & 0x0FFF)), type));
            }
            cursor += size;
        }
        return result;
    }
}
