using System.ComponentModel;
using System.Diagnostics;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using GameL10n.Core.Serialization;

namespace GameL10n.Core.Patching;

/// <summary>Installs a manifest with immutable original backups and a resumable local journal.</summary>
public sealed class PatchInstaller
{
    public const string ManifestFilename = "patch-manifest.json";
    private static readonly JsonSerializerOptions JsonOptions = new(CoreJson.Web) { WriteIndented = true };
    private readonly string projectDirectory;
    private readonly string sourceSnapshotsDirectory;

    public PatchInstaller(string projectDirectory, string? sourceSnapshotsDirectory = null)
    {
        this.projectDirectory = Path.TrimEndingDirectorySeparator(Path.GetFullPath(projectDirectory));
        this.sourceSnapshotsDirectory = sourceSnapshotsDirectory ?? Path.Combine(this.projectDirectory, "sources");
    }

    public string JournalPath => PatchFileSafety.UnderRoot(projectDirectory, "installation.json");

    /// <summary>Holds the game installation lock and imports any legacy record before repairs or installation.</summary>
    public async Task<IDisposable> OpenInstallationAsync(string gameDirectory, CancellationToken cancellationToken = default)
    {
        string gameRoot = PatchFileSafety.DirectoryRoot(gameDirectory);
        EnsureGameClosed(gameRoot, cancellationToken);
        var operationLock = AcquireLock(gameRoot, migrateLegacy: true);
        try
        {
            await LoadJournalAsync(gameRoot, cancellationToken, migrateLegacy: true);
            return operationLock;
        }
        catch { operationLock.Dispose(); throw; }
    }

    /// <summary>Rejects a running game or launcher before modifying its files or recovery journal.</summary>
    public static void EnsureGameClosed(string gameDirectory, CancellationToken cancellationToken = default)
    {
        cancellationToken.ThrowIfCancellationRequested();
        string root = PatchFileSafety.DirectoryRoot(gameDirectory);
        if (!Path.EndsInDirectorySeparator(root)) root += Path.DirectorySeparatorChar;
        var comparison = OperatingSystem.IsWindows() ? StringComparison.OrdinalIgnoreCase : StringComparison.Ordinal;
        var processes = Process.GetProcesses();
        try
        {
            foreach (var process in processes)
            {
                cancellationToken.ThrowIfCancellationRequested();
                string? executable;
                try { executable = process.MainModule?.FileName; }
                catch (Exception error) when (error is Win32Exception or InvalidOperationException or NotSupportedException)
                {
                    // Exited or inaccessible processes cannot be identified here. File/hash checks still guard writes.
                    continue;
                }
                if (executable?.StartsWith(root, comparison) == true)
                    throw new IOException("游戏或汉化启动器仍在运行：" + Path.GetFileName(executable)
                        + "。请关闭后重试，已完成的翻译和补丁会保留。");
            }
        }
        finally { foreach (var process in processes) process.Dispose(); }
    }

    /// <summary>Finds verified source bytes without changing the game or recovering its journal.</summary>
    public async Task<string> ResolveOriginalFileAsync(
        string gameDirectory, string relativePath, CancellationToken cancellationToken = default)
    {
        string gameRoot = PatchFileSafety.DirectoryRoot(gameDirectory);
        string relative = PatchFileSafety.NormalizeRelativePath(relativePath);
        string target = PatchFileSafety.UnderRoot(gameRoot, relative);
        using var operationLock = AcquireLock(gameRoot, migrateLegacy: false);
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
        return await FindOriginalCopyAsync(gameRoot, entry, cancellationToken)
            ?? throw MissingOriginal(entry);
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

        using var operationLock = await OpenInstallationAsync(gameRoot, cancellationToken);
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
            if (entry?.OriginalSha256 is not null)
            {
                var recovered = await EnsureOriginalBackupAsync(gameRoot, journal, entry, reservedPaths, cancellationToken);
                if (recovered is null) throw MissingOriginal(entry);
                if (recovered.Recovered) backups.Add(entry.BackupRelativePath!);
            }
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
                    OriginalCreationTimeUtc = currentHash is null ? null : File.GetCreationTimeUtc(target),
                    OriginalLastWriteTimeUtc = currentHash is null ? null : File.GetLastWriteTimeUtc(target),
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
                    await AtomicCopyAsync(target, backup, entry.OriginalSha256, overwrite: false, cancellationToken, preserveFileTimes: true);
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
        using var operationLock = await OpenInstallationAsync(gameRoot, cancellationToken);
        Journal journal = await LoadJournalAsync(gameRoot, cancellationToken);
        List<string> conflicts = await RecoverAsync(gameRoot, journal, cancellationToken);
        var reservedPaths = new HashSet<string>(journal.Entries.Select(entry => entry.RelativePath), StringComparer.OrdinalIgnoreCase);
        foreach (var entry in journal.Entries)
            if (entry.BackupRelativePath is not null) reservedPaths.Add(entry.BackupRelativePath);
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
                if (!await CompleteRestoreAsync(gameRoot, journal, entry, cancellationToken))
                    conflicts.Add(entry.RelativePath);
                continue;
            }
            if (!SameHash(currentHash, entry.InstalledSha256))
            {
                conflicts.Add(entry.RelativePath);
                continue;
            }

            var backup = entry.OriginalSha256 is null ? null :
                await EnsureOriginalBackupAsync(gameRoot, journal, entry, reservedPaths, cancellationToken);
            if (entry.OriginalSha256 is not null && backup is null)
            {
                conflicts.Add(entry.RelativePath);
                continue;
            }

            entry.Restoring = true;
            await SaveJournalAsync(gameRoot, journal);
            if (entry.OriginalSha256 is null)
            {
                await EnsureCurrentHashAsync(target, entry.InstalledSha256, cancellationToken);
                PatchFileSafety.CheckPath(target);
                File.Delete(target);
                removed++;
            }
            else
            {
                await MoveOriginalBackupAsync(backup!.Path, target, entry, cancellationToken);
                restored++;
            }
            if (!await CompleteRestoreAsync(gameRoot, journal, entry, cancellationToken))
                conflicts.Add(entry.RelativePath);
        }
        return new PatchRestoreResult(restored, removed, conflicts);
    }

    private static IOException MissingOriginal(Entry entry) => new(
        $"无法找到可用的原始文件：{entry.RelativePath}。游戏目录中的备份和本地原文快照均缺失或校验未通过，请恢复原版资源后重试。已保存的译文仍可继续使用。");

    private async Task<string?> FindOriginalCopyAsync(string gameRoot, Entry entry, CancellationToken cancellationToken)
    {
        string backup = PatchFileSafety.UnderRoot(gameRoot, entry.BackupRelativePath!);
        if (await MatchesOriginalAsync(backup, entry, cancellationToken)) return backup;

        // A renamed or older backup is usable only when its bytes match the recorded original.
        string target = PatchFileSafety.UnderRoot(gameRoot, entry.RelativePath);
        string directory = Path.GetDirectoryName(target)!;
        string prefix = Path.GetFileName(target) + '.';
        foreach (var candidate in Directory.EnumerateFiles(directory, "*.bak").Order(StringComparer.OrdinalIgnoreCase))
        {
            if (!Path.GetFileName(candidate).StartsWith(prefix, StringComparison.OrdinalIgnoreCase) ||
                candidate.Equals(backup, StringComparison.OrdinalIgnoreCase)) continue;
            if (await MatchesOriginalAsync(candidate, entry, cancellationToken)) return candidate;
        }

        if (sourceSnapshotsDirectory is not null && Directory.Exists(sourceSnapshotsDirectory))
        {
            string snapshots = PatchFileSafety.DirectoryRoot(sourceSnapshotsDirectory);
            foreach (var snapshot in Directory.EnumerateDirectories(snapshots).Order(StringComparer.OrdinalIgnoreCase))
            {
                string name = Path.GetFileName(snapshot);
                // Only committed content-addressed snapshots, never incomplete staging directories.
                if (name.Length != 64 || name.Any(c => !Uri.IsHexDigit(c))) continue;
                string candidate = PatchFileSafety.UnderRoot(snapshot, entry.RelativePath);
                if (await MatchesOriginalAsync(candidate, entry, cancellationToken)) return candidate;
            }
        }
        return null;
    }

    private static async Task<bool> MatchesOriginalAsync(string path, Entry entry, CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();
        return File.Exists(path) && SameHash(await HashFileAsync(path, cancellationToken), entry.OriginalSha256);
    }

    private sealed record OriginalBackup(string Path, bool Recovered);

    private async Task<OriginalBackup?> EnsureOriginalBackupAsync(string gameRoot, Journal journal, Entry entry,
        HashSet<string> reservedPaths, CancellationToken cancellationToken)
    {
        string recorded = PatchFileSafety.UnderRoot(gameRoot, entry.BackupRelativePath!);
        string? source = await FindOriginalCopyAsync(gameRoot, entry, cancellationToken);
        if (source is null) return null;
        if (source.Equals(recorded, StringComparison.OrdinalIgnoreCase)) return new(source, false);

        string relative = Path.GetRelativePath(gameRoot, source).Replace('\\', '/');
        if (!relative.StartsWith("../", StringComparison.Ordinal) && !Path.IsPathRooted(relative) &&
            !reservedPaths.Contains(relative) && relative.StartsWith(entry.RelativePath + '.', StringComparison.OrdinalIgnoreCase) &&
            relative.EndsWith(".bak", StringComparison.OrdinalIgnoreCase))
        {
            // Reuse an existing verified backup without duplicating it or modifying it.
            entry.BackupRelativePath = relative;
            reservedPaths.Add(relative);
            await SaveJournalAsync(gameRoot, journal);
            return new(source, true);
        }

        if (Path.Exists(recorded))
        {
            // Preserve a modified backup. Recovery writes to a new path instead.
            entry.BackupRelativePath = ChooseBackupPath(gameRoot, entry.RelativePath, reservedPaths);
            reservedPaths.Add(entry.BackupRelativePath);
            await SaveJournalAsync(gameRoot, journal);
            recorded = PatchFileSafety.UnderRoot(gameRoot, entry.BackupRelativePath);
        }
        await AtomicCopyAsync(source, recorded, entry.OriginalSha256!, overwrite: false, cancellationToken, preserveFileTimes: true);
        return new(recorded, true);
    }

    private async Task<bool> CompleteRestoreAsync(string gameRoot, Journal journal, Entry entry, CancellationToken cancellationToken)
    {
        string target = PatchFileSafety.UnderRoot(gameRoot, entry.RelativePath);
        await EnsureCurrentHashAsync(target, entry.OriginalSha256, cancellationToken);
        if (entry.BackupRelativePath is { } relative)
        {
            string backup = PatchFileSafety.UnderRoot(gameRoot, relative);
            if (File.Exists(backup))
            {
                // A completed old-style copy or interrupted restore may still have its backup.
                // Never remove an unrelated/modified backup just because the target is original.
                await using var verifiedBackup = new FileStream(backup, FileMode.Open, FileAccess.Read, FileShare.Read | FileShare.Delete,
                    81920, FileOptions.Asynchronous | FileOptions.SequentialScan);
                if (!SameHash(Convert.ToHexStringLower(await SHA256.HashDataAsync(verifiedBackup, cancellationToken)), entry.OriginalSha256)) return false;
                await EnsureCurrentHashAsync(target, entry.OriginalSha256, cancellationToken);
                var attributes = File.GetAttributes(backup);
                try
                {
                    if ((attributes & FileAttributes.ReadOnly) != 0) File.SetAttributes(backup, attributes & ~FileAttributes.ReadOnly);
                    File.Delete(backup);
                }
                catch
                {
                    if (File.Exists(backup)) File.SetAttributes(backup, attributes);
                    throw;
                }
            }
        }
        RestoreOriginalMetadata(target, entry);
        journal.Entries.Remove(entry);
        await SaveJournalAsync(gameRoot, journal);
        return true;
    }

    private static async Task MoveOriginalBackupAsync(string backup, string target, Entry entry, CancellationToken cancellationToken)
    {
        PatchFileSafety.CheckPath(backup);
        // Allow rename while preventing writes to the verified original until the move finishes.
        await using var original = new FileStream(backup, FileMode.Open, FileAccess.Read, FileShare.Read | FileShare.Delete,
            81920, FileOptions.Asynchronous | FileOptions.SequentialScan);
        string hash = Convert.ToHexStringLower(await SHA256.HashDataAsync(original, cancellationToken));
        if (!SameHash(hash, entry.OriginalSha256)) throw new IOException($"Original backup checksum mismatch: {backup}");
        await EnsureCurrentHashAsync(target, entry.InstalledSha256, cancellationToken);
        cancellationToken.ThrowIfCancellationRequested();
        PatchFileSafety.CheckPath(backup);
        PatchFileSafety.CheckPath(target);
        FileAttributes? restoreAttributesOnFailure = null;
        try
        {
            if (File.Exists(target))
            {
                var attributes = File.GetAttributes(target);
                if ((attributes & FileAttributes.ReadOnly) != 0)
                {
                    File.SetAttributes(target, attributes & ~FileAttributes.ReadOnly);
                    restoreAttributesOnFailure = attributes;
                }
            }
            File.Move(backup, target, overwrite: true);
            restoreAttributesOnFailure = null;
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            throw new IOException($"无法恢复文件：{target}。请解除文件占用或确认目录写入权限后重试。{error.Message}", error);
        }
        finally
        {
            if (restoreAttributesOnFailure is { } attributes && File.Exists(target)) File.SetAttributes(target, attributes);
        }
        // CompleteRestoreAsync verifies the final path before committing the journal.
    }

    private async Task<List<string>> RecoverAsync(string gameRoot, Journal journal, CancellationToken cancellationToken)
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
                    if (!await CompleteRestoreAsync(gameRoot, journal, entry, cancellationToken))
                        conflicts.Add(entry.RelativePath);
                    continue;
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
                if (entry.InstalledSha256 is null)
                {
                    if (!await CompleteRestoreAsync(gameRoot, journal, entry, cancellationToken))
                        conflicts.Add(entry.RelativePath);
                    continue;
                }
                entry.PendingSha256 = null;
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

    private IDisposable AcquireLock(string gameRoot, bool migrateLegacy)
    {
        if (RootsOverlap(gameRoot, projectDirectory))
            throw new InvalidDataException("项目数据目录与游戏目录必须分开。");
        PatchFileSafety.CheckPath(projectDirectory);
        // Use the game path, not the data location, so separate application instances share the same lock.
        string identity = OperatingSystem.IsWindows() ? gameRoot.ToUpperInvariant() : gameRoot;
        string key = Convert.ToHexStringLower(SHA256.HashData(Encoding.UTF8.GetBytes(identity)));
        // Share the lock with earlier releases that use the same installation journal.
        string lockPath = Path.Combine(Path.GetTempPath(), "AvgTranslator-install-" + key + ".lock");
        PatchFileSafety.CheckPath(lockPath);
        var operationLock = new FileStream(lockPath, FileMode.OpenOrCreate, FileAccess.ReadWrite, FileShare.None,
            1, FileOptions.DeleteOnClose);
        try
        {
            string legacyDirectory = PatchFileSafety.UnderRoot(gameRoot, PatchFileSafety.StateDirectoryName, allowStateDirectory: true);
            string legacyPath = PatchFileSafety.UnderRoot(gameRoot, PatchFileSafety.StateDirectoryName + "/install.lock", allowStateDirectory: true);
            var legacyLock = File.Exists(legacyPath) ? new FileStream(legacyPath, FileMode.Open,
                migrateLegacy ? FileAccess.ReadWrite : FileAccess.Read, FileShare.None, 1,
                migrateLegacy ? FileOptions.DeleteOnClose : FileOptions.None) : null;
            return new InstallationLock(operationLock, legacyLock, migrateLegacy ? legacyDirectory : null);
        }
        catch { operationLock.Dispose(); throw; }
    }

    private sealed class InstallationLock(FileStream operationLock, FileStream? legacyLock, string? legacyDirectory) : IDisposable
    {
        public void Dispose()
        {
            try
            {
                legacyLock?.Dispose();
                // Remove only the now-empty legacy folder; unrelated files are never deleted.
                if (legacyDirectory is not null && Directory.Exists(legacyDirectory))
                {
                    try
                    {
                        if (!Directory.EnumerateFileSystemEntries(legacyDirectory).Any()) Directory.Delete(legacyDirectory);
                    }
                    catch (IOException) { }
                    catch (UnauthorizedAccessException) { }
                }
            }
            finally { operationLock.Dispose(); }
        }
    }

    private async Task<Journal> LoadJournalAsync(string gameRoot, CancellationToken cancellationToken, bool migrateLegacy = false)
    {
        string legacyPath = PatchFileSafety.UnderRoot(gameRoot,
            PatchFileSafety.StateDirectoryName + "/patch-journal.json", allowStateDirectory: true);
        Journal? journal = File.Exists(JournalPath) ? await ReadJournalAsync(JournalPath, gameRoot, cancellationToken) : null;
        if (File.Exists(legacyPath))
        {
            var legacy = await ReadJournalAsync(legacyPath, gameRoot, cancellationToken);
            if (journal is not null && JsonSerializer.Serialize(journal, CoreJson.TypeInfo<Journal>(JsonOptions)) != JsonSerializer.Serialize(legacy, CoreJson.TypeInfo<Journal>(JsonOptions)))
                throw new InvalidDataException("项目与游戏目录的安装记录不一致，已保留两份记录，请确认使用的是对应项目数据。");
            journal ??= legacy;
            if (migrateLegacy)
            {
                // Durable project copy first: interruption leaves either the old record or two equivalent copies.
                await SaveJournalAsync(gameRoot, journal);
                File.Delete(legacyPath);
            }
        }
        return journal ?? new Journal { GameDirectory = gameRoot };
    }

    private static async Task<Journal> ReadJournalAsync(string path, string gameRoot, CancellationToken cancellationToken)
    {
        Journal journal = await ReadJsonAsync<Journal>(path, cancellationToken);
        if (journal.SchemaVersion != 1 || journal.Entries is null)
            throw new InvalidDataException("Unsupported or incomplete installation journal.");
        if (journal.GameDirectory is not null && !gameRoot.Equals(journal.GameDirectory,
                OperatingSystem.IsWindows() ? StringComparison.OrdinalIgnoreCase : StringComparison.Ordinal))
            throw new InvalidDataException("安装记录属于另一个游戏目录，请使用对应的项目数据。");
        journal.GameDirectory = gameRoot;
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
        CancellationToken cancellationToken, string? expectedCurrentHash = null, bool preserveFileTimes = false)
    {
        PatchFileSafety.CheckPath(source);
        PatchFileSafety.CheckPath(target);
        string directory = Path.GetDirectoryName(target)!;
        Directory.CreateDirectory(directory);
        PatchFileSafety.CheckPath(target);
        string temporary = Path.Combine(directory, ".gamel10n-" + Guid.NewGuid().ToString("N") + ".tmp");
        FileAttributes? restoreAttributesOnFailure = null;
        try
        {
            DateTime? creationTime = null, lastWriteTime = null;
            await using (var input = new FileStream(source, FileMode.Open, FileAccess.Read, FileShare.Read, 81920, FileOptions.Asynchronous | FileOptions.SequentialScan))
            await using (var output = new FileStream(temporary, FileMode.CreateNew, FileAccess.Write, FileShare.None, 81920, FileOptions.Asynchronous))
            {
                if (preserveFileTimes)
                {
                    creationTime = File.GetCreationTimeUtc(source);
                    lastWriteTime = File.GetLastWriteTimeUtc(source);
                }
                await input.CopyToAsync(output, cancellationToken);
                await output.FlushAsync(cancellationToken);
                output.Flush(flushToDisk: true);
            }
            if (!SameHash(await HashFileAsync(temporary, cancellationToken), expectedHash))
                throw new InvalidDataException($"File checksum changed while copying: {source}");
            if (creationTime is { } created) File.SetCreationTimeUtc(temporary, created);
            if (lastWriteTime is { } modified) File.SetLastWriteTimeUtc(temporary, modified);
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

    private static void RestoreOriginalMetadata(string target, Entry entry)
    {
        if (entry.OriginalSha256 is null || !File.Exists(target)) return;
        var attributes = File.GetAttributes(target) & ~FileAttributes.ReadOnly;
        File.SetAttributes(target, attributes);
        try
        {
            if (entry.OriginalCreationTimeUtc is { } created) File.SetCreationTimeUtc(target, created);
            if (entry.OriginalLastWriteTimeUtc is { } modified) File.SetLastWriteTimeUtc(target, modified);
        }
        finally { File.SetAttributes(target, entry.OriginalReadOnly ? attributes | FileAttributes.ReadOnly : attributes); }
    }

    private async Task SaveJournalAsync(string gameRoot, Journal journal)
    {
        journal.GameDirectory = gameRoot;
        string target = JournalPath;
        Directory.CreateDirectory(projectDirectory);
        string temporary = Path.Combine(Path.GetDirectoryName(target)!, "journal-" + Guid.NewGuid().ToString("N") + ".tmp");
        try
        {
            await using (var stream = new FileStream(temporary, FileMode.CreateNew, FileAccess.Write, FileShare.None, 4096, FileOptions.Asynchronous))
            {
                await JsonSerializer.SerializeAsync(stream, journal, CoreJson.TypeInfo<Journal>(JsonOptions));
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

    private static async Task<T> ReadJsonAsync<T>(string path, CancellationToken cancellationToken)
    {
        PatchFileSafety.CheckPath(path);
        await using var stream = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read, 4096, FileOptions.Asynchronous);
        return await JsonSerializer.DeserializeAsync<T>(stream, CoreJson.TypeInfo<T>(JsonOptions), cancellationToken)
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

    internal sealed class Journal
    {
        public Journal() { }
        public int SchemaVersion { get; set; } = 1;
        public string? GameDirectory { get; set; }
        public List<Entry> Entries { get; set; } = [];
    }

    internal sealed class Entry
    {
        public Entry() { }
        public string RelativePath { get; set; } = "";
        public string? OriginalSha256 { get; set; }
        public bool OriginalReadOnly { get; set; }
        public DateTime? OriginalCreationTimeUtc { get; set; }
        public DateTime? OriginalLastWriteTimeUtc { get; set; }
        public string? BackupRelativePath { get; set; }
        public string? InstalledSha256 { get; set; }
        public string? PendingSha256 { get; set; }
        public bool Restoring { get; set; }
    }
}
