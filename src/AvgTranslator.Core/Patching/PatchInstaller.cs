using System.Security.Cryptography;
using System.Text.Json;

namespace AvgTranslator.Core.Patching;

/// <summary>Installs a manifest with immutable original backups and a resumable local journal.</summary>
public sealed class PatchInstaller
{
    public const string ManifestFilename = "patch-manifest.json";
    private const string JournalFilename = "patch-journal.json";
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web) { WriteIndented = true };

    /// <summary>Finds verified source bytes without changing the game or recovering its journal.</summary>
    public async Task<string> ResolveOriginalFileAsync(
        string gameDirectory, string relativePath, CancellationToken cancellationToken = default)
    {
        string gameRoot = PatchFileSafety.DirectoryRoot(gameDirectory);
        string relative = PatchFileSafety.NormalizeRelativePath(relativePath);
        string target = PatchFileSafety.UnderRoot(gameRoot, relative);
        string lockPath = PatchFileSafety.UnderRoot(gameRoot,
            PatchFileSafety.StateDirectoryName + "/install.lock", allowStateDirectory: true);
        // Open an existing lock only: source selection must not create state in the game.
        using FileStream? operationLock = File.Exists(lockPath)
            ? new FileStream(lockPath, FileMode.Open, FileAccess.Read, FileShare.None)
            : null;
        Journal journal = await LoadJournalAsync(gameRoot, cancellationToken);
        string? actual = await HashIfPresentAsync(target, cancellationToken);
        if (actual is null)
            throw new FileNotFoundException("The requested game source does not exist.", target);
        Entry? entry = journal.Entries.SingleOrDefault(e => e.RelativePath.Equals(relative, StringComparison.OrdinalIgnoreCase));
        if (entry is null || SameHash(actual, entry.OriginalSha256))
            return target;
        bool isInstalled = (entry.InstalledSha256 is not null && SameHash(actual, entry.InstalledSha256)) ||
                           (entry.PendingSha256 is not null && SameHash(actual, entry.PendingSha256));
        if (!isInstalled)
            return target; // An external game update becomes new analysis input; installation still detects the conflict.
        if (entry.OriginalSha256 is null || entry.BackupRelativePath is null)
            throw new InvalidDataException("The requested file was added by a patch and has no original source.");
        string backup = PatchFileSafety.UnderRoot(gameRoot, entry.BackupRelativePath);
        if (!File.Exists(backup) || !SameHash(await HashFileAsync(backup, cancellationToken), entry.OriginalSha256))
            throw new IOException($"The original source backup is missing or has changed: {entry.BackupRelativePath}");
        return backup;
    }

    public async Task<PatchInstallResult> InstallAsync(
        string gameDirectory, string patchDirectory, CancellationToken cancellationToken = default)
    {
        string gameRoot = PatchFileSafety.DirectoryRoot(gameDirectory);
        string patchRoot = PatchFileSafety.DirectoryRoot(patchDirectory);
        if (RootsOverlap(gameRoot, patchRoot))
            throw new InvalidDataException("The game and patch directories must be separate.");

        string manifestPath = PatchFileSafety.UnderRoot(patchRoot, ManifestFilename);
        PatchManifest manifest = await ReadJsonAsync<PatchManifest>(manifestPath, cancellationToken);
        if (manifest.SchemaVersion != 1 || manifest.Files is null)
            throw new InvalidDataException("Unsupported or incomplete patch manifest.");

        // Validate the complete payload before creating backups or changing game files.
        var payload = new List<PatchFile>();
        var targetPaths = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        foreach (PatchFile file in manifest.Files)
        {
            cancellationToken.ThrowIfCancellationRequested();
            if (file is null)
                throw new InvalidDataException("The manifest contains a null file entry.");
            string relative = PatchFileSafety.NormalizeRelativePath(file.RelativePath);
            if (!targetPaths.Add(relative))
                throw new InvalidDataException($"Duplicate patch target: {relative}");
            ValidateHash(file.Sha256);
            if (file.ExpectedOriginalSha256 is not null) ValidateHash(file.ExpectedOriginalSha256);
            string source = PatchFileSafety.UnderRoot(patchRoot, relative);
            string target = PatchFileSafety.UnderRoot(gameRoot, relative);
            if (Directory.Exists(target))
                throw new IOException($"The patch target is a directory: {relative}");
            if (!SameHash(await HashFileAsync(source, cancellationToken), file.Sha256))
                throw new InvalidDataException($"Patch checksum mismatch: {relative}");
            payload.Add(new PatchFile(relative, file.Sha256.ToLowerInvariant(), file.ExpectedOriginalSha256));
        }
        foreach (string target in targetPaths)
            if (targetPaths.Any(other => other.StartsWith(target + '/', StringComparison.OrdinalIgnoreCase)))
                throw new InvalidDataException($"A patch file is also used as a directory: {target}");

        using FileStream operationLock = AcquireLock(gameRoot);
        Journal journal = await LoadJournalAsync(gameRoot, cancellationToken);
        List<string> recoveryConflicts = await RecoverAsync(gameRoot, journal, cancellationToken);
        if (recoveryConflicts.Count != 0)
            throw new IOException($"An interrupted operation conflicts with current game files: {string.Join(", ", recoveryConflicts)}");
        if (journal.Entries.Any(entry => entry.BackupRelativePath is not null && targetPaths.Contains(entry.BackupRelativePath)))
            throw new InvalidDataException("A patch target would overwrite a recorded original backup.");
        var reservedPaths = new HashSet<string>(targetPaths, StringComparer.OrdinalIgnoreCase);
        foreach (Entry existing in journal.Entries)
            if (existing.BackupRelativePath is not null)
                reservedPaths.Add(existing.BackupRelativePath);

        int installed = 0;
        // Verify all source versions before installing any file.
        foreach (var file in payload.Where(file => file.ExpectedOriginalSha256 is not null))
        {
            var entry = journal.Entries.SingleOrDefault(e => e.RelativePath.Equals(file.RelativePath, StringComparison.OrdinalIgnoreCase));
            var baseline = entry?.OriginalSha256 ?? await HashIfPresentAsync(PatchFileSafety.UnderRoot(gameRoot, file.RelativePath), cancellationToken);
            if (!SameHash(baseline, file.ExpectedOriginalSha256))
                throw new IOException($"The game resource changed since this patch was generated: {file.RelativePath}");
        }
        int unchanged = 0;
        var backups = new List<string>();
        foreach (PatchFile file in payload)
        {
            cancellationToken.ThrowIfCancellationRequested();
            string source = PatchFileSafety.UnderRoot(patchRoot, file.RelativePath);
            string target = PatchFileSafety.UnderRoot(gameRoot, file.RelativePath);
            string? currentHash = await HashIfPresentAsync(target, cancellationToken);
            Entry? entry = journal.Entries.SingleOrDefault(e => e.RelativePath.Equals(file.RelativePath, StringComparison.OrdinalIgnoreCase));
            if (entry is not null && !SameHash(currentHash, entry.InstalledSha256))
                throw new IOException($"The installed file changed outside the installer: {file.RelativePath}");
            if (SameHash(currentHash, file.Sha256))
            {
                unchanged++;
                continue;
            }

            if (entry is null)
            {
                entry = new Entry
                {
                    RelativePath = file.RelativePath,
                    OriginalSha256 = currentHash,
                    OriginalReadOnly = currentHash is not null && (File.GetAttributes(target) & FileAttributes.ReadOnly) != 0,
                    BackupRelativePath = currentHash is null ? null : ChooseBackupPath(gameRoot, file.RelativePath, reservedPaths)
                };
                if (entry.BackupRelativePath is not null)
                    reservedPaths.Add(entry.BackupRelativePath);
                journal.Entries.Add(entry);
            }

            entry.PendingSha256 = file.Sha256;
            await SaveJournalAsync(gameRoot, journal);
            if (entry.OriginalSha256 is not null)
            {
                string backup = PatchFileSafety.UnderRoot(gameRoot, entry.BackupRelativePath!);
                if (!File.Exists(backup))
                {
                    if (!SameHash(currentHash, entry.OriginalSha256))
                        throw new IOException($"The original backup is missing: {entry.BackupRelativePath}");
                    await AtomicCopyAsync(target, backup, entry.OriginalSha256, overwrite: false, cancellationToken);
                    backups.Add(entry.BackupRelativePath!);
                }
                if (!SameHash(await HashFileAsync(backup, cancellationToken), entry.OriginalSha256))
                    throw new IOException($"Original backup checksum mismatch: {entry.BackupRelativePath}");
            }

            await EnsureCurrentHashAsync(target, currentHash, cancellationToken);
            await AtomicCopyAsync(source, target, file.Sha256, overwrite: true, cancellationToken, currentHash);
            entry.InstalledSha256 = file.Sha256;
            entry.PendingSha256 = null;
            await SaveJournalAsync(gameRoot, journal);
            installed++;
        }

        return new PatchInstallResult(installed, unchanged, backups);
    }

    public async Task<PatchRestoreResult> RestoreAsync(string gameDirectory, CancellationToken cancellationToken = default)
    {
        string gameRoot = PatchFileSafety.DirectoryRoot(gameDirectory);
        using FileStream operationLock = AcquireLock(gameRoot);
        Journal journal = await LoadJournalAsync(gameRoot, cancellationToken);
        List<string> conflicts = await RecoverAsync(gameRoot, journal, cancellationToken);
        int restored = 0;
        int removed = 0;
        foreach (Entry entry in journal.Entries.ToArray().Reverse())
        {
            cancellationToken.ThrowIfCancellationRequested();
            if (conflicts.Contains(entry.RelativePath, StringComparer.OrdinalIgnoreCase))
                continue;
            string target = PatchFileSafety.UnderRoot(gameRoot, entry.RelativePath);
            string? currentHash = await HashIfPresentAsync(target, cancellationToken);
            if (SameHash(currentHash, entry.OriginalSha256))
            {
                RestoreReadOnly(target, entry);
                journal.Entries.Remove(entry);
                await SaveJournalAsync(gameRoot, journal);
                continue;
            }
            if (!SameHash(currentHash, entry.InstalledSha256))
            {
                conflicts.Add(entry.RelativePath);
                continue;
            }

            string? backup = entry.BackupRelativePath is null ? null : PatchFileSafety.UnderRoot(gameRoot, entry.BackupRelativePath);
            if (entry.OriginalSha256 is not null &&
                (backup is null || !File.Exists(backup) ||
                 !SameHash(await HashFileAsync(backup, cancellationToken), entry.OriginalSha256)))
            {
                conflicts.Add(entry.RelativePath);
                continue;
            }

            entry.Restoring = true;
            await SaveJournalAsync(gameRoot, journal);
            await EnsureCurrentHashAsync(target, entry.InstalledSha256, cancellationToken);
            if (entry.OriginalSha256 is null)
            {
                PatchFileSafety.CheckPath(target);
                File.Delete(target);
                removed++;
            }
            else
            {
                await AtomicCopyAsync(backup!, target, entry.OriginalSha256, overwrite: true, cancellationToken, entry.InstalledSha256);
                RestoreReadOnly(target, entry);
                restored++;
            }
            journal.Entries.Remove(entry);
            await SaveJournalAsync(gameRoot, journal);
        }
        return new PatchRestoreResult(restored, removed, conflicts);
    }

    private static async Task<List<string>> RecoverAsync(string gameRoot, Journal journal, CancellationToken cancellationToken)
    {
        var conflicts = new List<string>();
        foreach (Entry entry in journal.Entries.ToArray())
        {
            cancellationToken.ThrowIfCancellationRequested();
            if (entry.PendingSha256 is null && !entry.Restoring)
                continue;
            string target = PatchFileSafety.UnderRoot(gameRoot, entry.RelativePath);
            string? actual = await HashIfPresentAsync(target, cancellationToken);
            if (entry.Restoring)
            {
                if (SameHash(actual, entry.OriginalSha256))
                {
                    RestoreReadOnly(target, entry);
                    journal.Entries.Remove(entry);
                }
                else if (SameHash(actual, entry.InstalledSha256))
                    entry.Restoring = false;
                else
                {
                    conflicts.Add(entry.RelativePath);
                    continue;
                }
            }
            else if (SameHash(actual, entry.PendingSha256))
            {
                entry.InstalledSha256 = entry.PendingSha256;
                entry.PendingSha256 = null;
            }
            else if ((entry.InstalledSha256 is not null && SameHash(actual, entry.InstalledSha256)) ||
                     (entry.InstalledSha256 is null && SameHash(actual, entry.OriginalSha256)))
            {
                entry.PendingSha256 = null;
                if (entry.InstalledSha256 is null)
                {
                    RestoreReadOnly(target, entry);
                    journal.Entries.Remove(entry);
                }
            }
            else
            {
                conflicts.Add(entry.RelativePath);
                continue;
            }
            await SaveJournalAsync(gameRoot, journal);
        }
        return conflicts;
    }

    private static FileStream AcquireLock(string gameRoot)
    {
        string directory = PatchFileSafety.UnderRoot(gameRoot, PatchFileSafety.StateDirectoryName, allowStateDirectory: true);
        Directory.CreateDirectory(directory);
        PatchFileSafety.CheckPath(directory);
        string lockPath = PatchFileSafety.UnderRoot(gameRoot, PatchFileSafety.StateDirectoryName + "/install.lock", allowStateDirectory: true);
        return new FileStream(lockPath, FileMode.OpenOrCreate, FileAccess.ReadWrite, FileShare.None);
    }

    private static async Task<Journal> LoadJournalAsync(string gameRoot, CancellationToken cancellationToken)
    {
        string path = JournalPath(gameRoot);
        if (!File.Exists(path))
            return new Journal();
        Journal journal = await ReadJsonAsync<Journal>(path, cancellationToken);
        if (journal.SchemaVersion != 1 || journal.Entries is null)
            throw new InvalidDataException("Unsupported or incomplete installation journal.");
        var paths = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        var backups = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        foreach (Entry entry in journal.Entries)
        {
            if (entry is null)
                throw new InvalidDataException("The installation journal contains a null entry.");
            entry.RelativePath = PatchFileSafety.NormalizeRelativePath(entry.RelativePath);
            PatchFileSafety.UnderRoot(gameRoot, entry.RelativePath);
            if (!paths.Add(entry.RelativePath))
                throw new InvalidDataException("Duplicate installation journal path.");
            if (entry.OriginalSha256 is not null)
            {
                ValidateHash(entry.OriginalSha256);
                if (entry.BackupRelativePath is null)
                    throw new InvalidDataException("An original file has no backup location.");
                entry.BackupRelativePath = PatchFileSafety.NormalizeRelativePath(entry.BackupRelativePath);
                if (!(entry.BackupRelativePath.Equals(entry.RelativePath + ".bak", StringComparison.OrdinalIgnoreCase) ||
                      (entry.BackupRelativePath.StartsWith(entry.RelativePath + '.', StringComparison.OrdinalIgnoreCase) &&
                       entry.BackupRelativePath.EndsWith(".bak", StringComparison.OrdinalIgnoreCase))) ||
                    !backups.Add(entry.BackupRelativePath))
                    throw new InvalidDataException("Invalid original backup location.");
                PatchFileSafety.UnderRoot(gameRoot, entry.BackupRelativePath);
            }
            else if (entry.BackupRelativePath is not null)
                throw new InvalidDataException("A newly added file cannot have an original backup.");
            if (entry.InstalledSha256 is not null) ValidateHash(entry.InstalledSha256);
            if (entry.PendingSha256 is not null) ValidateHash(entry.PendingSha256);
            if ((entry.InstalledSha256 is null && entry.PendingSha256 is null) ||
                (entry.Restoring && (entry.InstalledSha256 is null || entry.PendingSha256 is not null)))
                throw new InvalidDataException("Invalid pending operation in the installation journal.");
        }
        if (paths.Overlaps(backups))
            throw new InvalidDataException("The installation journal targets one of its own backups.");
        return journal;
    }

    private static string ChooseBackupPath(string gameRoot, string relativePath, HashSet<string> targetPaths)
    {
        string candidate = relativePath + ".bak";
        while (targetPaths.Contains(candidate) || Path.Exists(PatchFileSafety.UnderRoot(gameRoot, candidate)))
            candidate = relativePath + '.' + DateTime.UtcNow.ToString("yyyyMMddHHmmssfff") + '.' + Guid.NewGuid().ToString("N") + ".bak";
        return candidate;
    }

    private static async Task AtomicCopyAsync(string source, string target, string expectedHash, bool overwrite,
        CancellationToken cancellationToken, string? expectedCurrentHash = null)
    {
        PatchFileSafety.CheckPath(source);
        PatchFileSafety.CheckPath(target);
        string directory = Path.GetDirectoryName(target)!;
        Directory.CreateDirectory(directory);
        PatchFileSafety.CheckPath(target);
        string temporary = Path.Combine(directory, ".avg-translator-" + Guid.NewGuid().ToString("N") + ".tmp");
        FileAttributes? restoreAttributesOnFailure = null;
        try
        {
            await using (var input = new FileStream(source, FileMode.Open, FileAccess.Read, FileShare.Read, 81920, FileOptions.Asynchronous | FileOptions.SequentialScan))
            await using (var output = new FileStream(temporary, FileMode.CreateNew, FileAccess.Write, FileShare.None, 81920, FileOptions.Asynchronous))
            {
                await input.CopyToAsync(output, cancellationToken);
                await output.FlushAsync(cancellationToken);
                output.Flush(flushToDisk: true);
            }
            if (!SameHash(await HashFileAsync(temporary, cancellationToken), expectedHash))
                throw new InvalidDataException($"File checksum changed while copying: {source}");
            cancellationToken.ThrowIfCancellationRequested();
            PatchFileSafety.CheckPath(target);
            if (overwrite)
            {
                await EnsureCurrentHashAsync(target, expectedCurrentHash, cancellationToken);
                if (File.Exists(target))
                {
                    var attributes = File.GetAttributes(target);
                    if ((attributes & FileAttributes.ReadOnly) != 0)
                    {
                        // Backup and checksum validation have completed before removing the replacement barrier.
                        File.SetAttributes(target, attributes & ~FileAttributes.ReadOnly);
                        restoreAttributesOnFailure = attributes;
                    }
                }
            }
            File.Move(temporary, target, overwrite);
            restoreAttributesOnFailure = null;
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            throw new IOException($"无法写入文件：{target}。请关闭正在运行的游戏，并确认当前用户有目录写入权限。{error.Message}", error);
        }
        finally
        {
            try
            {
                if (restoreAttributesOnFailure is { } attributes && File.Exists(target))
                    File.SetAttributes(target, attributes);
            }
            finally { if (File.Exists(temporary)) File.Delete(temporary); }
        }
    }

    private static void RestoreReadOnly(string target, Entry entry)
    {
        if (entry.OriginalSha256 is not null && entry.OriginalReadOnly && File.Exists(target))
            File.SetAttributes(target, File.GetAttributes(target) | FileAttributes.ReadOnly);
    }

    private static async Task SaveJournalAsync(string gameRoot, Journal journal)
    {
        string target = JournalPath(gameRoot);
        string temporary = Path.Combine(Path.GetDirectoryName(target)!, "journal-" + Guid.NewGuid().ToString("N") + ".tmp");
        try
        {
            await using (var stream = new FileStream(temporary, FileMode.CreateNew, FileAccess.Write, FileShare.None, 4096, FileOptions.Asynchronous))
            {
                await JsonSerializer.SerializeAsync(stream, journal, JsonOptions);
                await stream.FlushAsync();
                stream.Flush(flushToDisk: true);
            }
            PatchFileSafety.CheckPath(target);
            File.Move(temporary, target, overwrite: true);
        }
        finally
        {
            if (File.Exists(temporary)) File.Delete(temporary);
        }
    }

    private static string JournalPath(string root) => PatchFileSafety.UnderRoot(root,
        PatchFileSafety.StateDirectoryName + '/' + JournalFilename, allowStateDirectory: true);

    private static async Task<T> ReadJsonAsync<T>(string path, CancellationToken cancellationToken)
    {
        PatchFileSafety.CheckPath(path);
        await using var stream = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read, 4096, FileOptions.Asynchronous);
        return await JsonSerializer.DeserializeAsync<T>(stream, JsonOptions, cancellationToken)
            ?? throw new InvalidDataException($"Empty JSON document: {path}");
    }

    private static async Task<string> HashFileAsync(string path, CancellationToken cancellationToken)
    {
        PatchFileSafety.CheckPath(path);
        await using var stream = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read, 81920, FileOptions.Asynchronous | FileOptions.SequentialScan);
        return Convert.ToHexStringLower(await SHA256.HashDataAsync(stream, cancellationToken));
    }

    private static async Task<string?> HashIfPresentAsync(string path, CancellationToken cancellationToken)
    {
        PatchFileSafety.CheckPath(path);
        if (Directory.Exists(path))
            throw new IOException($"A file path is occupied by a directory: {path}");
        return File.Exists(path) ? await HashFileAsync(path, cancellationToken) : null;
    }

    private static async Task EnsureCurrentHashAsync(string path, string? expectedHash, CancellationToken cancellationToken)
    {
        if (!SameHash(await HashIfPresentAsync(path, cancellationToken), expectedHash))
            throw new IOException($"The game file changed during the operation: {path}");
    }

    private static void ValidateHash(string hash)
    {
        if (hash is null || hash.Length != 64 || hash.Any(c => !Uri.IsHexDigit(c)))
            throw new InvalidDataException("A SHA-256 checksum must contain exactly 64 hexadecimal digits.");
    }

    private static bool SameHash(string? left, string? right) => string.Equals(left, right, StringComparison.OrdinalIgnoreCase);

    private static bool RootsOverlap(string first, string second) =>
        first.Equals(second, StringComparison.OrdinalIgnoreCase) ||
        first.StartsWith(second + Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase) ||
        second.StartsWith(first + Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase);

    private sealed class Journal
    {
        public Journal() { }
        public int SchemaVersion { get; set; } = 1;
        public List<Entry> Entries { get; set; } = [];
    }

    private sealed class Entry
    {
        public Entry() { }
        public string RelativePath { get; set; } = "";
        public string? OriginalSha256 { get; set; }
        public bool OriginalReadOnly { get; set; }
        public string? BackupRelativePath { get; set; }
        public string? InstalledSha256 { get; set; }
        public string? PendingSha256 { get; set; }
        public bool Restoring { get; set; }
    }
}
