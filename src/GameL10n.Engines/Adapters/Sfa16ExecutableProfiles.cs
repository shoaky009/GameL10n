using System.Security.Cryptography;

namespace GameL10n.Core.Adapters;

internal sealed record Sfa16ExecutableProfile(
    string OriginalHash, string GameId, string GameTitle, string Variant,
    int CaveOffset, int CaveAddress, int VirtualSizeOffset, uint OriginalVirtualSize,
    IReadOnlyList<int> FontCalls, IReadOnlyList<Sfa16BytePatch> LeadBytes,
    IReadOnlyList<Sfa16RelocationPatch> FontCallRelocations,
    string OriginalCallHex, string StubHex,
    int CodeAddress, int CodeOffset);

internal sealed record Sfa16BytePatch(int Address, byte OldValue, byte NewValue);
internal sealed record Sfa16RelocationPatch(int Offset, ushort OriginalValue);

/// <summary>Exact executable builds supported by the shared SFA16 patching algorithm.</summary>
internal static class Sfa16ExecutableProfiles
{
    public static IReadOnlyList<Sfa16ExecutableProfile> Known { get; } =
    [
        new("3DE457FBD05CAFF77391CF4E817C8D182699B0B07F0343D1A5FBAEE0AD9B742B",
            "dark-blue-box", "DARK BLUE", "BOX / sfa16.exe 3DE457FB",
            0x7CB90, 0x47D790, 0x218, 0x7C78B,
            [0x40C551, 0x40EE05, 0x418D99, 0x41A881, 0x41B739, 0x41E319, 0x41F279, 0x4218D9, 0x4233D9, 0x428412],
            [new(0x40F3BE, 0xA0, 0xFF), new(0x40F86C, 0xA0, 0xFF), new(0x456239, 0x9F, 0xFE)],
            [new(0xACC8E, 0x3553), new(0xACE34, 0x3E07), new(0xAD0AA, 0x3D9B), new(0xAD1BE, 0x3883),
             new(0xAD20A, 0x373B), new(0xAD346, 0x331B), new(0xAD3AE, 0x327B), new(0xAD4F4, 0x38DB),
             new(0xAD5FC, 0x33DB), new(0xADA10, 0x3414)],
            "FF1568E04700", "8B442404C6401786C7401C53696D5366C74020756EC6402200E80000000058FFA0BA080000",
            0x401000, 0x400)
    ];

    public static Sfa16ExecutableProfile? Match(byte[] data)
    {
        var hash = Convert.ToHexString(SHA256.HashData(data));
        return Known.FirstOrDefault(profile => profile.OriginalHash == hash);
    }
}
