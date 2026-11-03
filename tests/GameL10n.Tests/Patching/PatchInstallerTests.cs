using System.Runtime.InteropServices;
using System.Diagnostics;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;
using GameL10n.Core.Patching;
using Xunit;

namespace GameL10n.Tests.Patching;

public sealed class PatchInstallerTests : IDisposable
{
    private readonly string _root;
    private readonly string _game;
    private readonly string _patch;
    private readonly string _project;
    private readonly PatchInstaller _installer;
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web);

    public PatchInstallerTests()
    {
        // Every fixture is beneath the checkout; no test touches the installed game.
        _root = Path.Combine(FindWorkspace(), "target", "patch-installer-tests", Guid.NewGuid().ToString("N"));
        _game = Path.Combine(_root, "game");
        _patch = Path.Combine(_root, "patch");
        _project = Path.Combine(_root, "project");
        _installer = new(_project);
        Directory.CreateDirectory(_game);
        Directory.CreateDirectory(_patch);
    }

    [Fact]
    public async Task NewExecutableLeavesOriginalIntactAndIsRemovedOnRestore()
    {
        await WriteGameAsync("game.exe", "original executable");
        await WritePatchAsync(("game.zh.exe", "translated executable"), ("data/text.txt", "translated text"));

        PatchInstallResult installed = await _installer.InstallAsync(_game, _patch);
        Assert.Equal(2, installed.InstalledFiles);
        Assert.Empty(installed.BackupFiles);
        Assert.Equal("original executable", await ReadGameAsync("game.exe"));
        Assert.Equal("translated executable", await ReadGameAsync("game.zh.exe"));

        PatchRestoreResult restored = await _installer.RestoreAsync(_game);
        Assert.Equal(2, restored.RemovedFiles);
        Assert.Empty(restored.Conflicts);
        Assert.False(File.Exists(GamePath("game.zh.exe")));
        Assert.False(File.Exists(GamePath("data/text.txt")));
        Assert.Equal("original executable", await ReadGameAsync("game.exe"));
    }

    [Fact]
    public async Task RepeatedInstallsKeepTheFirstOriginalAndExistingBak()
    {
        await WriteGameAsync("game.exe", "original");
        await WriteGameAsync("game.exe.bak", "older manual backup");
        await WritePatchAsync(("game.exe", "patch one"));

        PatchInstallResult first = await _installer.InstallAsync(_game, _patch);
        string backup = Assert.Single(first.BackupFiles);
        Assert.EndsWith(".bak", backup);
        Assert.NotEqual("game.exe.bak", backup);
        Assert.Equal("original", await ReadGameAsync(backup));
        Assert.Equal("older manual backup", await ReadGameAsync("game.exe.bak"));

        PatchInstallResult repeated = await _installer.InstallAsync(_game, _patch);
        Assert.Equal(0, repeated.InstalledFiles);
        Assert.Equal(1, repeated.UnchangedFiles);
        Assert.Empty(repeated.BackupFiles);

        await WritePatchAsync(("game.exe", "patch two"));
        PatchInstallResult second = await _installer.InstallAsync(_game, _patch);
        Assert.Equal(1, second.InstalledFiles);
        Assert.Empty(second.BackupFiles);
        Assert.Equal("original", await ReadGameAsync(backup));

        PatchRestoreResult restored = await _installer.RestoreAsync(_game);
        Assert.Equal(1, restored.RestoredFiles);
        Assert.Empty(restored.Conflicts);
        Assert.Equal("original", await ReadGameAsync("game.exe"));
        Assert.Equal("original", await ReadGameAsync(backup));
        Assert.Equal("older manual backup", await ReadGameAsync("game.exe.bak"));
    }

    [Fact]
    public async Task ReadOnlyOriginalCanBeInstalledAndRestoredWithItsAttribute()
    {
        await WriteGameAsync("scr.aos", "original");
        var target = GamePath("scr.aos");
        File.SetAttributes(target, File.GetAttributes(target) | FileAttributes.ReadOnly);
        try
        {
            await WritePatchAsync(("scr.aos", "translated"), ("game.zh.exe", "launcher"));
            var installed = await _installer.InstallAsync(_game, _patch);
            Assert.Equal(2, installed.InstalledFiles);
            Assert.Equal("translated", await ReadGameAsync("scr.aos"));
            Assert.Equal("original", await ReadGameAsync("scr.aos.bak"));
            Assert.Empty((await _installer.RestoreAsync(_game)).Conflicts);
            Assert.Equal("original", await ReadGameAsync("scr.aos"));
            Assert.True((File.GetAttributes(target) & FileAttributes.ReadOnly) != 0);
        }
        finally { File.SetAttributes(target, File.GetAttributes(target) & ~FileAttributes.ReadOnly); }
    }

    [Fact]
    public async Task FailedReadOnlyReplacementKeepsOriginalAndResumesAfterUnlock()
    {
        if (!OperatingSystem.IsWindows()) return;
        await WriteGameAsync("scr.aos", "original");
        var target = GamePath("scr.aos");
        File.SetAttributes(target, File.GetAttributes(target) | FileAttributes.ReadOnly);
        try
        {
            await WritePatchAsync(("scr.aos", "translated"));
            using (var held = new FileStream(target, FileMode.Open, FileAccess.Read, FileShare.ReadWrite))
            {
                var error = await Assert.ThrowsAsync<IOException>(() => _installer.InstallAsync(_game, _patch));
                Assert.Contains(target, error.Message);
                Assert.Equal("original", await ReadGameAsync("scr.aos"));
                Assert.True((File.GetAttributes(target) & FileAttributes.ReadOnly) != 0);
            }
            Assert.Equal(1, (await _installer.InstallAsync(_game, _patch)).InstalledFiles);
            Assert.Equal("translated", await ReadGameAsync("scr.aos"));
            Assert.Empty((await _installer.RestoreAsync(_game)).Conflicts);
            Assert.Equal("original", await ReadGameAsync("scr.aos"));
            Assert.True((File.GetAttributes(target) & FileAttributes.ReadOnly) != 0);
        }
        finally { File.SetAttributes(target, File.GetAttributes(target) & ~FileAttributes.ReadOnly); }
    }

    [Fact]
    public async Task IdenticalPreexistingFileIsNotOwnedOrDeletedByRestore()
    {
        await WriteGameAsync("extra.dll", "same bytes");
        await WritePatchAsync(("extra.dll", "same bytes"));

        PatchInstallResult installed = await _installer.InstallAsync(_game, _patch);
        Assert.Equal(1, installed.UnchangedFiles);
        PatchRestoreResult restored = await _installer.RestoreAsync(_game);
        Assert.Equal(0, restored.RemovedFiles);
        Assert.Equal("same bytes", await ReadGameAsync("extra.dll"));
    }

    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public async Task RunningGameBlocksChangesAndOperationCanResumeAfterExit(bool restore)
    {
        if (!OperatingSystem.IsWindows()) return;
        await WriteGameAsync("script.dat", "original");
        await WritePatchAsync(("script.dat", "translated"));
        if (restore) await _installer.InstallAsync(_game, _patch);
        string? journalBefore = File.Exists(JournalPath) ? await File.ReadAllTextAsync(JournalPath) : null;

        // A real process beneath the game root also covers launchers in subdirectories.
        string executable = GamePath("tools/running-game.exe");
        Directory.CreateDirectory(Path.GetDirectoryName(executable)!);
        File.Copy(Path.Combine(Environment.SystemDirectory, "cmd.exe"), executable);
        var start = new ProcessStartInfo(executable)
        {
            UseShellExecute = false, CreateNoWindow = true, WorkingDirectory = _game,
            RedirectStandardInput = true, RedirectStandardOutput = true, RedirectStandardError = true
        };
        start.ArgumentList.Add("/d");
        start.ArgumentList.Add("/q");
        using (var process = Process.Start(start)!)
        {
            try
            {
                Assert.False(process.HasExited);
                var error = await Assert.ThrowsAsync<IOException>(async () =>
                {
                    if (restore) await _installer.RestoreAsync(_game);
                    else await _installer.InstallAsync(_game, _patch);
                });
                Assert.Contains("running-game.exe", error.Message);
                Assert.Contains("请关闭后重试", error.Message);
                Assert.Equal(restore ? "translated" : "original", await ReadGameAsync("script.dat"));
                Assert.Equal(journalBefore, File.Exists(JournalPath) ? await File.ReadAllTextAsync(JournalPath) : null);
                if (!restore) Assert.False(Directory.Exists(GamePath(".avg-translator")));
            }
            finally
            {
                if (!process.HasExited) process.Kill();
                using var timeout = new CancellationTokenSource(TimeSpan.FromSeconds(10));
                await process.WaitForExitAsync(timeout.Token);
            }
        }

        if (restore) Assert.Empty((await _installer.RestoreAsync(_game)).Conflicts);
        else Assert.Equal(1, (await _installer.InstallAsync(_game, _patch)).InstalledFiles);
        Assert.Equal(restore ? "original" : "translated", await ReadGameAsync("script.dat"));
    }

    [Fact]
    public async Task OriginalSourceSelectionRemainsOriginalAcrossPatchRevisions()
    {
        await WriteGameAsync("scr.aos", "original archive");
        await WritePatchAsync(("scr.aos", "patch one"));
        await _installer.InstallAsync(_game, _patch);
        Assert.Equal("original archive", await File.ReadAllTextAsync(
            await _installer.ResolveOriginalFileAsync(_game, "scr.aos")));
        await WritePatchAsync(("scr.aos", "patch two"));
        await _installer.InstallAsync(_game, _patch);
        Assert.Equal("original archive", await File.ReadAllTextAsync(
            await _installer.ResolveOriginalFileAsync(_game, "scr.aos")));
        Assert.Equal("patch two", await ReadGameAsync("scr.aos"));
    }

    [Fact]
    public async Task ExternalGameUpdateBecomesCurrentAnalysisSource()
    {
        await WriteGameAsync("scr.aos", "original archive");
        Assert.Equal(GamePath("scr.aos"), await _installer.ResolveOriginalFileAsync(_game, "scr.aos"));
        Assert.False(Directory.Exists(GamePath(".avg-translator")));
        await WritePatchAsync(("scr.aos", "patch"));
        await _installer.InstallAsync(_game, _patch);
        await WriteGameAsync("scr.aos", "official update");

        Assert.Equal(GamePath("scr.aos"), await _installer.ResolveOriginalFileAsync(_game, "scr.aos"));
        Assert.Equal("official update", await ReadGameAsync("scr.aos"));
        await Assert.ThrowsAsync<IOException>(() => _installer.InstallAsync(_game, _patch));
    }

    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public async Task MissingRecordedBackupUsesAnotherVerifiedBackup(bool restore)
    {
        await WriteGameAsync("data/story.txt", "original");
        await WriteGameAsync("data/story.txt.bak", "original");
        await WritePatchAsync(("data/story.txt", "patch one"));
        string recorded = Assert.Single((await _installer.InstallAsync(_game, _patch)).BackupFiles);
        File.Delete(GamePath(recorded));
        string journalBefore = await File.ReadAllTextAsync(JournalPath);

        Assert.Equal(GamePath("data/story.txt.bak"), await _installer.ResolveOriginalFileAsync(_game, "data/story.txt"));
        Assert.Equal(journalBefore, await File.ReadAllTextAsync(JournalPath));
        Assert.False(File.Exists(GamePath(recorded)));
        if (restore)
        {
            Assert.Empty((await _installer.RestoreAsync(_game)).Conflicts);
            Assert.Equal("original", await ReadGameAsync("data/story.txt"));
        }
        else
        {
            await WritePatchAsync(("data/story.txt", "patch two"));
            Assert.Equal(1, (await _installer.InstallAsync(_game, _patch)).InstalledFiles);
            Assert.Equal("data/story.txt.bak", ReadJournal()["entries"]![0]!["backupRelativePath"]!.GetValue<string>());
            Assert.Equal("patch two", await ReadGameAsync("data/story.txt"));
        }
        Assert.Equal("original", await ReadGameAsync("data/story.txt.bak"));
    }

    [Theory]
    [InlineData(false, false)]
    [InlineData(true, false)]
    [InlineData(false, true)]
    [InlineData(true, true)]
    public async Task SnapshotRecoversDeletedOrModifiedBackup(bool corrupt, bool restore)
    {
        await WriteGameAsync("scr.aos", "original");
        await WritePatchAsync(("scr.aos", "patch"));
        string recorded = Assert.Single((await _installer.InstallAsync(_game, _patch)).BackupFiles);
        string snapshots = Path.Combine(_root, "sources");
        string snapshot = Path.Combine(snapshots, new string('a', 64), "scr.aos");
        Directory.CreateDirectory(Path.GetDirectoryName(snapshot)!);
        File.Copy(GamePath(recorded), snapshot);
        if (corrupt) await WriteGameAsync(recorded, "modified backup");
        else File.Delete(GamePath(recorded));
        var installer = new PatchInstaller(_project, snapshots);

        Assert.Equal(snapshot, await installer.ResolveOriginalFileAsync(_game, "scr.aos"));
        Assert.Equal("patch", await ReadGameAsync("scr.aos"));
        if (restore)
        {
            Assert.Equal(1, (await installer.RestoreAsync(_game)).RestoredFiles);
            Assert.Equal("original", await ReadGameAsync("scr.aos"));
        }
        else
        {
            // Even reinstalling an unchanged patch repairs its missing original backup.
            var installed = await installer.InstallAsync(_game, _patch);
            Assert.Equal(1, installed.UnchangedFiles);
            Assert.Equal("original", await ReadGameAsync(Assert.Single(installed.BackupFiles)));
            Assert.Empty((await new PatchInstaller(_project).RestoreAsync(_game)).Conflicts);
            Assert.Equal("original", await ReadGameAsync("scr.aos"));
        }
        if (corrupt) Assert.Equal("modified backup", await ReadGameAsync(recorded));
        Assert.Equal("original", await File.ReadAllTextAsync(snapshot));
    }

    [Fact]
    public async Task RecoveryRejectsWrongSnapshotAndIncompleteSnapshotWithoutTouchingGame()
    {
        await WriteGameAsync("scr.aos", "original");
        await WritePatchAsync(("scr.aos", "patch one"));
        File.Delete(GamePath(Assert.Single((await _installer.InstallAsync(_game, _patch)).BackupFiles)));
        string snapshots = Path.Combine(_root, "sources");
        foreach (var (name, text) in new[] { (new string('a', 64), "wrong version"), (new string('b', 64) + ".pending-x", "original") })
        {
            string directory = Path.Combine(snapshots, name);
            Directory.CreateDirectory(directory);
            await File.WriteAllTextAsync(Path.Combine(directory, "scr.aos"), text);
        }
        var installer = new PatchInstaller(_project, snapshots);
        await Assert.ThrowsAsync<IOException>(() => installer.ResolveOriginalFileAsync(_game, "scr.aos"));
        await WritePatchAsync(("scr.aos", "patch two"));
        await Assert.ThrowsAsync<IOException>(() => installer.InstallAsync(_game, _patch));
        Assert.Equal("scr.aos", Assert.Single((await installer.RestoreAsync(_game)).Conflicts));
        Assert.Equal("patch one", await ReadGameAsync("scr.aos"));
        Assert.False(File.Exists(GamePath("scr.aos.bak")));
    }

    [Fact]
    public async Task SourceSelectionVerifiesBackupsAndRejectsPatchOnlyFiles()
    {
        await WriteGameAsync("scr.aos", "original archive");
        await WritePatchAsync(("scr.aos", "patch"), ("added.dll", "new file"));
        await _installer.InstallAsync(_game, _patch);
        await Assert.ThrowsAsync<InvalidDataException>(() => _installer.ResolveOriginalFileAsync(_game, "added.dll"));
        await WriteGameAsync("scr.aos.bak", "corrupted original");
        await Assert.ThrowsAsync<IOException>(() => _installer.ResolveOriginalFileAsync(_game, "scr.aos"));
    }

    [Fact]
    public async Task RestorePreservesAnExternalGameUpdateAndKeepsItsRecoveryRecord()
    {
        await WriteGameAsync("game.exe", "original");
        await WritePatchAsync(("game.exe", "patch"), ("new.dll", "new plugin"));
        await _installer.InstallAsync(_game, _patch);
        await WriteGameAsync("game.exe", "updated by game distributor");

        PatchRestoreResult result = await _installer.RestoreAsync(_game);
        Assert.Equal("game.exe", Assert.Single(result.Conflicts));
        Assert.Equal(1, result.RemovedFiles);
        Assert.Equal("updated by game distributor", await ReadGameAsync("game.exe"));
        Assert.Equal("original", await ReadGameAsync("game.exe.bak"));
        Assert.Single(ReadJournal()["entries"]!.AsArray());

        await WriteGameAsync("game.exe", "patch");
        Assert.Empty((await _installer.RestoreAsync(_game)).Conflicts);
        Assert.Equal("original", await ReadGameAsync("game.exe"));
    }

    [Fact]
    public async Task MissingOrCorruptOriginalBackupPreventsFurtherOverwrite()
    {
        await WriteGameAsync("game.exe", "original");
        await WritePatchAsync(("game.exe", "patch one"));
        await _installer.InstallAsync(_game, _patch);
        await WriteGameAsync("game.exe.bak", "corrupt backup");
        await WritePatchAsync(("game.exe", "patch two"));

        await Assert.ThrowsAsync<IOException>(() => _installer.InstallAsync(_game, _patch));
        Assert.Equal("patch one", await ReadGameAsync("game.exe"));
        Assert.Equal("game.exe", Assert.Single((await _installer.RestoreAsync(_game)).Conflicts));
        Assert.Equal("patch one", await ReadGameAsync("game.exe"));
    }

    [Fact]
    public async Task BadPayloadChecksumFailsBeforeAnyGameFileChanges()
    {
        await WriteGameAsync("game.exe", "original");
        await WritePatchAsync(("game.exe", "patch"), ("extra.dll", "expected"));
        await File.WriteAllTextAsync(Path.Combine(_patch, "extra.dll"), "tampered");

        await Assert.ThrowsAsync<InvalidDataException>(() => _installer.InstallAsync(_game, _patch));
        Assert.Equal("original", await ReadGameAsync("game.exe"));
        Assert.False(File.Exists(GamePath("game.exe.bak")));
        Assert.False(Directory.Exists(GamePath(".avg-translator")));
    }

    [Theory]
    [InlineData("../outside.txt")]
    [InlineData("sub/../../outside.txt")]
    [InlineData("/absolute.txt")]
    [InlineData("C:\\outside.txt")]
    [InlineData("sub\\..\\outside.txt")]
    [InlineData("game.exe:stream")]
    [InlineData("folder./file.txt")]
    [InlineData("CON.txt")]
    [InlineData(".avg-translator/patch-journal.json")]
    public async Task UnsafeManifestPathsAreRejected(string relativePath)
    {
        await WriteManifestAsync(new PatchManifest(1, [new PatchFile(relativePath, Hash("payload"))]));
        await Assert.ThrowsAsync<InvalidDataException>(() => _installer.InstallAsync(_game, _patch));
        Assert.Empty(Directory.EnumerateFileSystemEntries(_game));
    }

    [Fact]
    public async Task CaseInsensitiveDuplicateTargetsAreRejected()
    {
        await WritePatchAsync(("game.exe", "patch"));
        await WriteManifestAsync(new PatchManifest(1,
            [new PatchFile("game.exe", Hash("patch")), new PatchFile("GAME.EXE", Hash("patch"))]));
        await Assert.ThrowsAsync<InvalidDataException>(() => _installer.InstallAsync(_game, _patch));
    }

    [Fact]
    public async Task BackupNameCannotCollideWithAnotherManifestTarget()
    {
        await WriteGameAsync("game.exe", "original");
        await WritePatchAsync(("game.exe", "patch"), ("game.exe.bak", "payload also named bak"));

        PatchInstallResult installed = await _installer.InstallAsync(_game, _patch);
        Assert.NotEqual("game.exe.bak", Assert.Single(installed.BackupFiles));
        Assert.Equal("payload also named bak", await ReadGameAsync("game.exe.bak"));
        Assert.Empty((await _installer.RestoreAsync(_game)).Conflicts);
        Assert.Equal("original", await ReadGameAsync("game.exe"));
        Assert.False(File.Exists(GamePath("game.exe.bak")));
    }

    [Fact]
    public async Task RecoveryCommitsAnInterruptedReplacementBeforeRestore()
    {
        await WriteGameAsync("game.exe", "original");
        await WritePatchAsync(("game.exe", "patch one"));
        await _installer.InstallAsync(_game, _patch);
        JsonObject journal = ReadJournal();
        journal["entries"]![0]!["pendingSha256"] = Hash("patch two");
        await WriteJournalAsync(journal);
        await WriteGameAsync("game.exe", "patch two");

        PatchRestoreResult restored = await _installer.RestoreAsync(_game);
        Assert.Empty(restored.Conflicts);
        Assert.Equal(1, restored.RestoredFiles);
        Assert.Equal("original", await ReadGameAsync("game.exe"));
    }

    [Fact]
    public async Task RecoveryRollsBackAnIntentWhoseOriginalWasNeverReplaced()
    {
        await WriteGameAsync("game.exe", "original");
        await WritePendingJournalAsync("game.exe", "original", "patch");

        PatchRestoreResult restored = await _installer.RestoreAsync(_game);
        Assert.Empty(restored.Conflicts);
        Assert.Equal("original", await ReadGameAsync("game.exe"));
        Assert.Empty(ReadJournal()["entries"]!.AsArray());
    }

    [Fact]
    public async Task RecoveryRemovesNewFileWrittenBeforeTheJournalCommit()
    {
        await WritePendingJournalAsync("extra.dll", null, "patch");
        await WriteGameAsync("extra.dll", "patch");

        PatchRestoreResult restored = await _installer.RestoreAsync(_game);
        Assert.Empty(restored.Conflicts);
        Assert.Equal(1, restored.RemovedFiles);
        Assert.False(File.Exists(GamePath("extra.dll")));
    }

    [Fact]
    public async Task RecoveryRecognizesRestoreThatFinishedBeforeJournalCommit()
    {
        await WriteGameAsync("game.exe", "original");
        await WritePatchAsync(("game.exe", "patch"));
        await _installer.InstallAsync(_game, _patch);
        JsonObject journal = ReadJournal();
        journal["entries"]![0]!["restoring"] = true;
        await WriteJournalAsync(journal);
        await WriteGameAsync("game.exe", "original");

        PatchRestoreResult restored = await _installer.RestoreAsync(_game);
        Assert.Empty(restored.Conflicts);
        Assert.Equal("original", await ReadGameAsync("game.exe"));
        Assert.Empty(ReadJournal()["entries"]!.AsArray());
    }

    [Fact]
    public async Task RecoveryPreservesAnUnrecognizedInterruptedTarget()
    {
        await WritePendingJournalAsync("game.exe", "original", "patch");
        await WriteGameAsync("game.exe", "externally updated");

        Assert.Equal("game.exe", Assert.Single((await _installer.RestoreAsync(_game)).Conflicts));
        Assert.Equal("externally updated", await ReadGameAsync("game.exe"));
        Assert.Single(ReadJournal()["entries"]!.AsArray());
    }

    [Fact]
    public async Task RecoveryDoesNotTreatAMissingOriginalAsAnUnstartedNewFile()
    {
        await WritePendingJournalAsync("game.exe", "original", "patch");

        Assert.Equal("game.exe", Assert.Single((await _installer.RestoreAsync(_game)).Conflicts));
        Assert.Single(ReadJournal()["entries"]!.AsArray());
    }

    [Fact]
    public async Task CanceledInstallDoesNotTouchTheOriginal()
    {
        await WriteGameAsync("game.exe", "original");
        await WritePatchAsync(("game.exe", "patch"));
        using var canceled = new CancellationTokenSource();
        canceled.Cancel();

        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => _installer.InstallAsync(_game, _patch, canceled.Token));
        Assert.Equal("original", await ReadGameAsync("game.exe"));
        Assert.False(File.Exists(GamePath("game.exe.bak")));
    }

    [Fact]
    public async Task HardLinkedTargetIsRejectedWithoutChangingEitherName()
    {
        await WriteGameAsync("game.exe", "original");
        string otherName = Path.Combine(_root, "same-original.exe");
        CreateHardLink(otherName, GamePath("game.exe"));
        await WritePatchAsync(("game.exe", "patch"));

        await Assert.ThrowsAsync<IOException>(() => _installer.InstallAsync(_game, _patch));
        Assert.Equal("original", await ReadGameAsync("game.exe"));
        Assert.Equal("original", await File.ReadAllTextAsync(otherName));
    }

    [Fact]
    public async Task HardLinkedPayloadIsRejected()
    {
        await WritePatchAsync(("extra.dll", "patch"));
        CreateHardLink(Path.Combine(_root, "payload-alias.dll"), Path.Combine(_patch, "extra.dll"));
        await Assert.ThrowsAsync<IOException>(() => _installer.InstallAsync(_game, _patch));
        Assert.False(File.Exists(GamePath("extra.dll")));
    }

    [Fact]
    public async Task DirectoryLinkCannotRedirectAnInstallOutsideTheGame()
    {
        string outside = Path.Combine(_root, "outside-game");
        Directory.CreateDirectory(outside);
        await File.WriteAllTextAsync(Path.Combine(outside, "text.txt"), "outside original");
        string link = GamePath("redirected");
        CreateDirectoryLink(link, outside);
        try
        {
            await WritePatchAsync(("redirected/text.txt", "patch"));
            await Assert.ThrowsAsync<IOException>(() => _installer.InstallAsync(_game, _patch));
            Assert.Equal("outside original", await File.ReadAllTextAsync(Path.Combine(outside, "text.txt")));
            Assert.False(File.Exists(Path.Combine(outside, "text.txt.bak")));
        }
        finally { Directory.Delete(link); }
    }

    [Fact]
    public async Task DirectoryLinkCannotRedirectRestoreAfterInstallation()
    {
        await WriteGameAsync("data/text.txt", "original");
        await WritePatchAsync(("data/text.txt", "patch"));
        await _installer.InstallAsync(_game, _patch);
        string outside = Path.Combine(_root, "outside-game");
        Directory.CreateDirectory(outside);
        await File.WriteAllTextAsync(Path.Combine(outside, "text.txt"), "outside original");
        Directory.Move(GamePath("data"), GamePath("saved-data"));
        string link = GamePath("data");
        CreateDirectoryLink(link, outside);
        try
        {
            await Assert.ThrowsAsync<IOException>(() => _installer.RestoreAsync(_game));
            Assert.Equal("outside original", await File.ReadAllTextAsync(Path.Combine(outside, "text.txt")));
            Assert.Equal("original", await ReadGameAsync("saved-data/text.txt.bak"));
        }
        finally { Directory.Delete(link); }
    }

    [Fact]
    public async Task ASecondInstallerCannotRunWhileTheJournalIsLocked()
    {
        await WriteGameAsync("game.exe", "original");
        await WritePatchAsync(("game.exe", "patch"));
        using var held = await _installer.OpenInstallationAsync(_game);

        await Assert.ThrowsAsync<IOException>(() => _installer.InstallAsync(_game, _patch));
        Assert.Equal("original", await ReadGameAsync("game.exe"));
    }

    private async Task WritePendingJournalAsync(string path, string? original, string pending)
    {
        var journal = new JsonObject
        {
            ["schemaVersion"] = 1,
            ["entries"] = new JsonArray(new JsonObject
            {
                ["relativePath"] = path,
                ["originalSha256"] = original is null ? null : Hash(original),
                ["backupRelativePath"] = original is null ? null : path + ".bak",
                ["installedSha256"] = null,
                ["pendingSha256"] = Hash(pending),
                ["restoring"] = false
            })
        };
        await WriteJournalAsync(journal);
    }

    private async Task WritePatchAsync(params (string Path, string Content)[] files)
    {
        foreach ((string relativePath, string content) in files)
        {
            string path = Path.Combine(_patch, relativePath.Replace('/', Path.DirectorySeparatorChar));
            Directory.CreateDirectory(Path.GetDirectoryName(path)!);
            await File.WriteAllTextAsync(path, content);
        }
        await WriteManifestAsync(new PatchManifest(1, files.Select(file => new PatchFile(file.Path, Hash(file.Content))).ToArray()));
    }

    private Task WriteManifestAsync(PatchManifest manifest) =>
        File.WriteAllTextAsync(Path.Combine(_patch, PatchInstaller.ManifestFilename), JsonSerializer.Serialize(manifest, JsonOptions));

    private Task WriteGameAsync(string relativePath, string content)
    {
        string path = GamePath(relativePath);
        Directory.CreateDirectory(Path.GetDirectoryName(path)!);
        return File.WriteAllTextAsync(path, content);
    }

    private Task<string> ReadGameAsync(string relativePath) => File.ReadAllTextAsync(GamePath(relativePath));
    private string GamePath(string relativePath) => Path.Combine(_game, relativePath.Replace('/', Path.DirectorySeparatorChar));
    private string JournalPath => _installer.JournalPath;
    private JsonObject ReadJournal() => JsonNode.Parse(File.ReadAllText(JournalPath))!.AsObject();

    private Task WriteJournalAsync(JsonObject journal)
    {
        Directory.CreateDirectory(Path.GetDirectoryName(JournalPath)!);
        return File.WriteAllTextAsync(JournalPath, journal.ToJsonString());
    }

    private static string Hash(string text) => Convert.ToHexStringLower(SHA256.HashData(Encoding.UTF8.GetBytes(text)));

    private static string FindWorkspace()
    {
        for (var directory = new DirectoryInfo(AppContext.BaseDirectory); directory is not null; directory = directory.Parent)
            if (Directory.Exists(Path.Combine(directory.FullName, "src", "GameL10n.Core")))
                return directory.FullName;
        throw new DirectoryNotFoundException("Cannot find the workspace for patch tests.");
    }

    private static void CreateHardLink(string link, string existing)
    {
        bool success = OperatingSystem.IsWindows() ? CreateHardLinkWindows(link, existing, IntPtr.Zero) :
            OperatingSystem.IsLinux() ? LinkUnix(existing, link) == 0 :
            throw new PlatformNotSupportedException();
        if (!success)
            throw new IOException($"Could not create the hard-link test fixture (native error {Marshal.GetLastPInvokeError()}).");
    }

    private static void CreateDirectoryLink(string link, string target)
    {
        if (!OperatingSystem.IsWindows())
        {
            Directory.CreateSymbolicLink(link, target);
            return;
        }

        // A junction works on Windows without the privilege required for symbolic links.
        var start = new ProcessStartInfo("cmd.exe") { UseShellExecute = false, CreateNoWindow = true,
            RedirectStandardOutput = true, RedirectStandardError = true };
        foreach (string argument in new[] { "/c", "mklink", "/J", link, target })
            start.ArgumentList.Add(argument);
        using Process process = Process.Start(start)!;
        string output = process.StandardOutput.ReadToEnd() + process.StandardError.ReadToEnd();
        process.WaitForExit();
        if (process.ExitCode != 0)
            throw new IOException("Could not create the junction test fixture: " + output);
    }

    [DllImport("kernel32.dll", EntryPoint = "CreateHardLinkW", CharSet = CharSet.Unicode, SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool CreateHardLinkWindows(string link, string existing, IntPtr securityAttributes);

    [DllImport("libc", EntryPoint = "link", SetLastError = true)]
    private static extern int LinkUnix([MarshalAs(UnmanagedType.LPUTF8Str)] string existing, [MarshalAs(UnmanagedType.LPUTF8Str)] string link);

    public void Dispose()
    {
        if (Directory.Exists(_root)) Directory.Delete(_root, recursive: true);
    }
}
