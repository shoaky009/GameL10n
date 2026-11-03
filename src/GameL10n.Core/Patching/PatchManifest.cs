namespace GameL10n.Core.Patching;

public sealed record PatchManifest(int SchemaVersion, IReadOnlyList<PatchFile> Files);

public sealed record PatchFile(string RelativePath, string Sha256, string? ExpectedOriginalSha256 = null);

public sealed record PatchInstallResult(
    int InstalledFiles,
    int UnchangedFiles,
    IReadOnlyList<string> BackupFiles);

public sealed record PatchRestoreResult(
    int RestoredFiles,
    int RemovedFiles,
    IReadOnlyList<string> Conflicts);
