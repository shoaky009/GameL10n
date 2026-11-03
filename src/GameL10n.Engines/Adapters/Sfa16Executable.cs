using System.Buffers.Binary;
using GameL10n.Core.Validation;

namespace GameL10n.Core.Adapters;

internal static class Sfa16Executable
{
    public const string FileName = "sfa16.zh.exe";

    public static bool IsOriginal(byte[] data) => Sfa16ExecutableProfiles.Match(data) is not null;

    public static byte[] Patch(byte[] original)
    {
        var profile = Sfa16ExecutableProfiles.Match(original)
            ?? throw new NotSupportedException("This SFA16 executable build has no verified patch profile; no executable was patched.");
        var patched = original.ToArray();
        var stub = Convert.FromHexString(profile.StubHex);
        var originalCall = Convert.FromHexString(profile.OriginalCallHex);
        if (BinaryPrimitives.ReadUInt32LittleEndian(patched.AsSpan(profile.VirtualSizeOffset)) != profile.OriginalVirtualSize ||
            patched.AsSpan(profile.CaveOffset, stub.Length).ContainsAnyExcept((byte)0))
            throw new InvalidDataException("The expected executable code padding is unavailable.");
        foreach (var address in profile.FontCalls)
        {
            var offset = FileOffset(profile, address);
            if (!patched.AsSpan(offset, 6).SequenceEqual(originalCall)) throw new InvalidDataException("Unexpected font call signature.");
            patched[offset] = 0xE8;
            BinaryPrimitives.WriteInt32LittleEndian(patched.AsSpan(offset + 1), profile.CaveAddress - address - 5);
            patched[offset + 5] = 0x90;
        }
        foreach (var patch in profile.LeadBytes)
        {
            if (patched[FileOffset(profile, patch.Address)] != patch.OldValue) throw new InvalidDataException("Unexpected lead-byte classifier signature.");
            patched[FileOffset(profile, patch.Address)] = patch.NewValue;
        }
        // The original indirect calls contain absolute IAT addresses. Their HIGHLOW
        // relocations would corrupt the new relative CALL operands under ASLR.
        foreach (var relocation in profile.FontCallRelocations)
        {
            if (BinaryPrimitives.ReadUInt16LittleEndian(patched.AsSpan(relocation.Offset)) != relocation.OriginalValue)
                throw new InvalidDataException("Unexpected font-call relocation signature.");
            BinaryPrimitives.WriteUInt16LittleEndian(patched.AsSpan(relocation.Offset), (ushort)(relocation.OriginalValue & 0x0FFF));
        }
        // stdcall wrapper: select GB2312_CHARSET and SimSun, then tail-call GDI.
        // CALL/POP obtains the runtime address; the IAT jump is position-independent.
        stub.CopyTo(patched, profile.CaveOffset);
        BinaryPrimitives.WriteUInt32LittleEndian(patched.AsSpan(profile.VirtualSizeOffset),
            (uint)(profile.CaveAddress - profile.CodeAddress + stub.Length));
        var validation = ExecutableChecks.Validate(original, patched);
        if (!validation.Passed)
            throw new InvalidDataException("Executable loading checks failed: " + string.Join("; ", validation.Issues.Select(issue => issue.Message)));
        return patched;
    }

    public static bool IsPatched(byte[] data) => Sfa16ExecutableProfiles.Known.Any(profile => IsPatched(data, profile));

    private static bool IsPatched(byte[] data, Sfa16ExecutableProfile profile)
    {
        var stub = Convert.FromHexString(profile.StubHex);
        if (data.Length <= profile.CaveOffset + stub.Length || !data.AsSpan(profile.CaveOffset, stub.Length).SequenceEqual(stub)) return false;
        if (BinaryPrimitives.ReadUInt32LittleEndian(data.AsSpan(profile.VirtualSizeOffset)) !=
            (uint)(profile.CaveAddress - profile.CodeAddress + stub.Length)) return false;
        var restored = data.ToArray();
        var originalCall = Convert.FromHexString(profile.OriginalCallHex);
        foreach (var address in profile.FontCalls)
        {
            var offset = FileOffset(profile, address);
            if (data[offset] != 0xE8 || data[offset + 5] != 0x90 ||
                BinaryPrimitives.ReadInt32LittleEndian(data.AsSpan(offset + 1)) != profile.CaveAddress - address - 5) return false;
            originalCall.CopyTo(restored, offset);
        }
        foreach (var patch in profile.LeadBytes)
        {
            if (data[FileOffset(profile, patch.Address)] != patch.NewValue) return false;
            restored[FileOffset(profile, patch.Address)] = patch.OldValue;
        }
        foreach (var relocation in profile.FontCallRelocations)
        {
            if (data.Length - relocation.Offset < 2 ||
                BinaryPrimitives.ReadUInt16LittleEndian(data.AsSpan(relocation.Offset)) != (relocation.OriginalValue & 0x0FFF)) return false;
            BinaryPrimitives.WriteUInt16LittleEndian(restored.AsSpan(relocation.Offset), relocation.OriginalValue);
        }
        restored.AsSpan(profile.CaveOffset, stub.Length).Clear();
        BinaryPrimitives.WriteUInt32LittleEndian(restored.AsSpan(profile.VirtualSizeOffset), profile.OriginalVirtualSize);
        return Sfa16ExecutableProfiles.Match(restored) == profile && ExecutableChecks.Validate(restored, data).Passed;
    }

    private static int FileOffset(Sfa16ExecutableProfile profile, int address) => address - profile.CodeAddress + profile.CodeOffset;
}
