using System.Diagnostics;
using System.Text.Json;
using System.Text.Json.Nodes;
using GameL10n.Core;
using GameL10n.Core.Isolation;
using GameL10n.Core.Pipeline;
using GameL10n.Core.Storage;
using GameL10n.Core.Translation;

return await DataTool.RunAsync(args);

internal static class DataTool
{
    private static readonly StringComparison Paths = OperatingSystem.IsWindows()
        ? StringComparison.OrdinalIgnoreCase : StringComparison.Ordinal;

    public static async Task<int> RunAsync(string[] args)
    {
        try
        {
            if (args.Length == 0 || args[0] is "--help" or "-h")
            {
                Console.WriteLine("migrate (--game-directory <path> | --all) [--source-root <path>] [--destination-root <path>] [--activate] [--bootstrap-root <path>] [--report <path>]");
                Console.WriteLine("verify --game-directory <path> [--data-root <path>] [--report <path>]");
                return 0;
            }
            var options = Parse(args[1..]);
            if (args[0] == "migrate" && options.ContainsKey("all"))
                await EmitAsync(await MigrateAllAsync(options), options);
            else if (args[0] == "migrate")
                await EmitAsync(await MigrateAsync(Path.GetFullPath(Required(options, "game-directory")), options), options);
            else if (args[0] == "verify")
            {
                var gameDirectory = Path.GetFullPath(Required(options, "game-directory"));
                var root = Path.GetFullPath(options.GetValueOrDefault("data-root") ?? DataLocation.Resolve());
                RejectRunningApp();
                using var appLock = AcquireRootLock(root);
                await EmitAsync(await VerifyAsync(root, gameDirectory), options);
            }
            else throw new ArgumentException("Unknown command. Use --help.");
            return 0;
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or InvalidDataException
            or ArgumentException or JsonException or InvalidOperationException)
        {
            Console.Error.WriteLine("Data operation failed: " + error.Message);
            return 1;
        }
    }

    private static async Task<object> MigrateAllAsync(Dictionary<string, string?> options)
    {
        if (options.ContainsKey("game-directory")) throw new ArgumentException("Choose --all or --game-directory.");
        var source = Path.GetFullPath(options.GetValueOrDefault("source-root") ?? DataLocation.Resolve());
        var destination = Path.GetFullPath(options.GetValueOrDefault("destination-root") ?? source);
        var games = Path.Combine(source, "games");
        if (!Directory.Exists(games)) throw new DirectoryNotFoundException("The source has no games directory.");
        var legacy = await ReadObjectAsync(Path.Combine(source, "settings.json"));
        var legacyPath = legacy?["gameDirectory"]?.GetValue<string>();
        var paths = new List<string>();
        foreach (var directory in Directory.EnumerateDirectories(games))
        {
            var id = Path.GetFileName(directory);
            if (id.Length != 24 || !id.All(Uri.IsHexDigit)) continue;
            WorkspaceFiles.RejectLinkAncestors(directory);
            var descriptor = await ReadObjectAsync(Path.Combine(directory, "game.json"));
            var job = descriptor is null ? await ReadObjectAsync(Path.Combine(directory, "job.json")) : null;
            var path = descriptor?["gameDirectory"]?.GetValue<string>() ?? job?["gameDirectory"]?.GetValue<string>();
            if (path is null && legacyPath is not null && TranslationPipeline.GameId(legacyPath) == id) path = legacyPath;
            if (path is null || TranslationPipeline.GameId(path) != id)
                throw new InvalidDataException("A game directory has no matching recorded installation path: " + id);
            paths.Add(Path.GetFullPath(path));
        }
        if (paths.Count == 0) throw new InvalidDataException("No game projects were found.");
        var perGame = new Dictionary<string, string?>(options);
        perGame.Remove("activate");
        var reports = new List<object>();
        foreach (var path in paths) reports.Add(await MigrateAsync(path, perGame));
        if (options.ContainsKey("activate"))
            await ActivateAsync(Path.GetFullPath(options.GetValueOrDefault("bootstrap-root") ?? DataLocation.DefaultDirectory), destination);
        return new { status = "migrated-all", gameCount = reports.Count, dataRoot = destination, games = reports };
    }

    private static async Task<object> MigrateAsync(string gameDirectory, Dictionary<string, string?> options)
    {
        RejectRunningApp();
        var bootstrap = Path.GetFullPath(options.GetValueOrDefault("bootstrap-root") ?? DataLocation.DefaultDirectory);
        var source = Path.GetFullPath(options.GetValueOrDefault("source-root") ?? DataLocation.Resolve());
        var destination = Path.GetFullPath(options.GetValueOrDefault("destination-root") ?? source);
        WorkspaceFiles.RejectLinkAncestors(source);
        WorkspaceFiles.RejectLinkAncestors(destination);
        if (!Directory.Exists(source)) throw new DirectoryNotFoundException("The source data directory does not exist.");
        if (Inside(gameDirectory, destination) || Same(gameDirectory, destination))
            throw new InvalidDataException("The data directory must be outside the game directory.");
        if (!Same(source, destination) && (Inside(source, destination) || Inside(destination, source)))
            throw new InvalidDataException("Source and destination data roots must not contain one another.");
        Directory.CreateDirectory(destination);
        using var destinationLock = AcquireRootLock(destination);
        using var sourceLock = Same(source, destination) ? null : ReadLockIfPresent(Path.Combine(source, ".app.lock"));
        var id = TranslationPipeline.GameId(gameDirectory);
        var project = Path.Combine(destination, "games", id);
        var sourceProject = Path.Combine(source, "games", id);
        var stage = Path.Combine(destination, ".migration", id);
        var backup = project + ".bak";

        if (await IsCurrentAsync(project))
        {
            var existing = await VerifyAsync(destination, gameDirectory);
            if (options.ContainsKey("activate")) await ActivateAsync(bootstrap, destination);
            return new { status = "already-migrated", verification = existing };
        }

        // A process interruption between the two directory renames leaves the verified stage and backup.
        if (!Directory.Exists(sourceProject) && Same(source, destination) && Directory.Exists(backup))
            sourceProject = backup;
        if (!Directory.Exists(sourceProject)) throw new DirectoryNotFoundException("No legacy data exists for the selected game.");
        using var projectLock = ReadLockIfPresent(Path.Combine(sourceProject, ".task.lock"));
        var timer = Stopwatch.StartNew();
        // Rebuild only the disposable stage. A previous import marker or shard can otherwise
        // conceal newer legacy translations when retrying a partially completed migration.
        if (Directory.Exists(stage))
        {
            _ = LocalFiles.UnderRoot(destination, ".migration/" + id);
            // Copies retain source attributes. Clear ReadOnly only on disposable staging
            // files so a failed migration can be retried without touching original snapshots.
            var stagedFiles = EnumerateSafeFiles(stage).ToArray();
            foreach (var file in stagedFiles)
            {
                var attributes = File.GetAttributes(file);
                if ((attributes & FileAttributes.ReadOnly) != 0)
                    File.SetAttributes(file, attributes & ~FileAttributes.ReadOnly);
            }
            Directory.Delete(stage, recursive: true);
        }
        Directory.CreateDirectory(stage);
        if (await IsCurrentAsync(sourceProject))
        {
            var currentCopy = await CopyTreeAsync(sourceProject, stage, replaceStaging: true,
                relative => relative is not ".task.lock" and not "memory/.memory.lock");
            await NormalizePathsAsync(sourceProject, stage);
            var currentVerification = await VerifyAsync(destination, gameDirectory, stage);
            await CopyGlobalsAsync(source, destination);
            // Windows cannot rename a directory with an open child file, even with
            // FileShare.Delete. The data-root locks remain held through the commit.
            projectLock?.Dispose();
            CommitProject(stage, project, backup);
            if (options.ContainsKey("activate")) await ActivateAsync(bootstrap, destination);
            return new
            {
                status = "copied-current", gameId = id, dataRoot = destination,
                copiedFiles = currentCopy.Files, copiedBytes = currentCopy.Bytes,
                elapsedMilliseconds = timer.ElapsedMilliseconds, verification = currentVerification
            };
        }
        var copied = await CopyTreeAsync(sourceProject, stage, replaceStaging: true,
            relative => relative is not ".task.lock" and not "game.json" and not "migration.json"
                && relative != "memory/.memory.lock"
                && !relative.StartsWith("extraction/", StringComparison.Ordinal));

        var snapshot = await LocalFiles.ReadJsonAsync<SourceSnapshot>(Path.Combine(stage, "source.json"))
            ?? throw new InvalidDataException("The legacy source snapshot is missing.");
        var relativeSources = ProjectRelative(sourceProject, stage, snapshot.Directory, "sources");
        foreach (var file in snapshot.Files)
        {
            var path = LocalFiles.UnderRoot(Path.Combine(stage, relativeSources), file.RelativePath);
            if (!File.Exists(path) || await LocalFiles.HashFileAsync(path) != file.Sha256)
                throw new InvalidDataException("A source snapshot failed its SHA-256 check.");
        }
        var fingerprint = Path.GetFileName(relativeSources);
        var extraction = await LocalFiles.ReadJsonAsync<ExtractionResult>(Path.Combine(stage, "extraction.json"))
            ?? throw new InvalidDataException("The legacy extraction is missing.");
        var extractionStore = new ExtractionStore(stage);
        await extractionStore.StoreAsync(fingerprint, snapshot.AdapterId, snapshot.AdapterVersion, extraction);
        var loaded = await extractionStore.LoadAsync(fingerprint, snapshot.AdapterId, snapshot.AdapterVersion)
            ?? throw new InvalidDataException("The converted extraction could not be read.");
        if (!JsonSerializer.SerializeToUtf8Bytes(extraction, LocalFiles.JsonOptions)
            .AsSpan().SequenceEqual(JsonSerializer.SerializeToUtf8Bytes(loaded, LocalFiles.JsonOptions)))
            throw new InvalidDataException("The converted extraction does not match its legacy input.");

        var memoryDirectory = Path.Combine(stage, "memory");
        var legacyMemory = Path.Combine(source, "memory", id);
        var memory = new TranslationMemory(memoryDirectory);
        if (Directory.Exists(legacyMemory)) await memory.ImportLegacyAsync(legacyMemory);
        else await memory.InitializeAsync();
        var memoryCount = await VerifyLegacyMemoryAsync(legacyMemory, memoryDirectory, Path.Combine(sourceProject, "memory"));

        // Raw request, response and refusal files remain byte-for-byte copies. Only workspace paths change.
        await NormalizePathsAsync(sourceProject, stage);

        var settingsPath = Path.Combine(source, "settings.json");
        var legacySettings = await ReadObjectAsync(settingsPath);
        var gameSettings = ReadGameSettings(legacySettings);
        var descriptor = await LocalFiles.ReadJsonAsync<GameProject>(Path.Combine(sourceProject, "game.json")) ?? new GameProject
        {
            Id = id, GameDirectory = gameDirectory,
            Name = new DirectoryInfo(gameDirectory).Name,
            Settings = gameSettings, AdapterId = snapshot.AdapterId, AdapterVersion = snapshot.AdapterVersion,
            UpdatedAt = DateTimeOffset.UtcNow
        };

        await CopyGlobalsAsync(source, destination);
        await CopyAdaptationHistoryAsync(source, stage, id);
        await LocalFiles.WriteJsonAsync(Path.Combine(stage, "migration.json"), new
        {
            schemaVersion = 2, sourceRoot = source, gameId = id, verifiedAt = DateTimeOffset.UtcNow,
            copiedFiles = copied.Files, copiedBytes = copied.Bytes,
            memoryEntries = memoryCount, extractionUnits = extraction.Units.Count,
            sourceSnapshotsVerified = snapshot.Files.Count, legacyPreserved = true
        });
        // The descriptor is the commit marker. Never replace a game that already has a v2 descriptor.
        await LocalFiles.WriteJsonAsync(Path.Combine(stage, "game.json"), descriptor);
        if (await IsCurrentAsync(project)) throw new IOException("The destination game was modified during migration.");
        projectLock?.Dispose();
        CommitProject(stage, project, backup);
        if (options.ContainsKey("activate")) await ActivateAsync(bootstrap, destination);
        timer.Stop();
        var verified = await VerifyAsync(destination, gameDirectory);
        return new
        {
            status = "migrated", gameId = id, dataRoot = destination,
            legacyBackup = Directory.Exists(backup) ? backup : null,
            elapsedMilliseconds = timer.ElapsedMilliseconds, verification = verified
        };
    }

    private static async Task<object> VerifyAsync(string root, string gameDirectory, string? projectOverride = null)
    {
        var timer = Stopwatch.StartNew();
        var project = projectOverride ?? Path.Combine(root, "games", TranslationPipeline.GameId(gameDirectory));
        var descriptor = await LocalFiles.ReadJsonAsync<GameProject>(Path.Combine(project, "game.json"))
            ?? throw new InvalidDataException("The game descriptor is missing.");
        if (descriptor.SchemaVersion != 2 || descriptor.Id != TranslationPipeline.GameId(gameDirectory))
            throw new InvalidDataException("The game descriptor has an unsupported version or identity.");
        var snapshot = await LocalFiles.ReadJsonAsync<SourceSnapshot>(Path.Combine(project, "source.json"))
            ?? throw new InvalidDataException("The source snapshot is missing.");
        var sources = Path.IsPathRooted(snapshot.Directory) ? snapshot.Directory : LocalFiles.UnderRoot(project, snapshot.Directory);
        foreach (var file in snapshot.Files)
            if (await LocalFiles.HashFileAsync(LocalFiles.UnderRoot(sources, file.RelativePath)) != file.Sha256)
                throw new InvalidDataException("A source snapshot failed its SHA-256 check.");
        var extractionTimer = Stopwatch.StartNew();
        var extraction = await new ExtractionStore(project).LoadAsync(Path.GetFileName(sources), snapshot.AdapterId, snapshot.AdapterVersion)
            ?? throw new InvalidDataException("The converted extraction is missing.");
        extractionTimer.Stop();
        var memoryTimer = Stopwatch.StartNew();
        var memory = new TranslationMemory(Path.Combine(project, "memory"));
        await memory.InitializeAsync();
        memoryTimer.Stop();
        var terms = new Dictionary<string, string>(StringComparer.Ordinal);
        foreach (var line in descriptor.Settings.Terminology.Split('\n', StringSplitOptions.TrimEntries | StringSplitOptions.RemoveEmptyEntries))
        {
            var pair = line.Split('=', 2, StringSplitOptions.TrimEntries);
            if (pair.Length == 2 && pair[0].Length > 0) terms[pair[0]] = pair[1];
        }
        var policy = new TranslationPolicy(descriptor.Settings.TargetLanguage.Trim(), descriptor.Settings.Style.Trim(), terms);
        var lookupTimer = Stopwatch.StartNew();
        var cacheHits = 0;
        foreach (var unit in extraction.Units)
            if (await memory.FindAsync(unit, policy) is not null) cacheHits++;
        lookupTimer.Stop();
        timer.Stop();
        return new
        {
            gameId = descriptor.Id, schemaVersion = descriptor.SchemaVersion, extractionUnits = extraction.Units.Count,
            memoryShardFiles = Directory.EnumerateFiles(Path.Combine(project, "memory"), "??.json").Count(),
            cacheHits, sourceFilesVerified = snapshot.Files.Count,
            responseFiles = CountFiles(Path.Combine(project, "responses")),
            extractionLoadMilliseconds = extractionTimer.ElapsedMilliseconds,
            memoryLoadMilliseconds = memoryTimer.ElapsedMilliseconds,
            cacheLookupMilliseconds = lookupTimer.ElapsedMilliseconds, elapsedMilliseconds = timer.ElapsedMilliseconds
        };
    }

    private static async Task<int> VerifyLegacyMemoryAsync(string legacy, string converted, string currentMemory)
    {
        if (!Directory.Exists(legacy)) return 0;
        var shards = new Dictionary<string, MemoryShard>(StringComparer.Ordinal);
        var currentShards = new Dictionary<string, MemoryShard?>(StringComparer.Ordinal);
        var count = 0;
        foreach (var file in Directory.EnumerateFiles(legacy, "*.json"))
        {
            var key = Path.GetFileNameWithoutExtension(file);
            if (key.Length != 64) continue;
            var entry = await LocalFiles.ReadJsonAsync<MemoryEntry>(file)
                ?? throw new InvalidDataException("A legacy translation entry is empty.");
            if (entry.Key != key) throw new InvalidDataException("A legacy translation key does not match its filename.");
            var prefix = key[..2];
            if (!shards.TryGetValue(prefix, out var shard))
            {
                shard = await LocalFiles.ReadJsonAsync<MemoryShard>(Path.Combine(converted, prefix + ".json"))
                    ?? throw new InvalidDataException("A converted memory shard is missing.");
                shards.Add(prefix, shard);
            }
            if (!currentShards.TryGetValue(prefix, out var currentShard))
            {
                currentShard = await LocalFiles.ReadJsonAsync<MemoryShard>(Path.Combine(currentMemory, prefix + ".json"));
                currentShards.Add(prefix, currentShard);
            }
            var expected = currentShard?.Entries.GetValueOrDefault(key) ?? entry;
            if (!shard.Entries.TryGetValue(key, out var actual) || actual != expected)
                throw new InvalidDataException("A converted translation differs from its legacy input.");
            count++;
        }
        return count;
    }

    private static async Task<(int Files, long Bytes)> CopyTreeAsync(string source, string destination,
        bool replaceStaging, Func<string, bool>? include = null)
    {
        var count = 0;
        long bytes = 0;
        foreach (var file in EnumerateSafeFiles(source))
        {
            var relative = Path.GetRelativePath(source, file).Replace('\\', '/');
            if (include is not null && !include(relative)) continue;
            var target = LocalFiles.UnderRoot(destination, relative);
            await CopyFileVerifiedAsync(file, target, replaceStaging);
            count++;
            bytes += new FileInfo(file).Length;
        }
        return (count, bytes);
    }

    private static IEnumerable<string> EnumerateSafeFiles(string root)
    {
        WorkspaceFiles.RejectLinkAncestors(root);
        foreach (var file in Directory.EnumerateFiles(root))
        {
            WorkspaceFiles.RejectReparsePoints(file);
            yield return file;
        }
        foreach (var directory in Directory.EnumerateDirectories(root))
        {
            WorkspaceFiles.RejectReparsePoints(directory);
            foreach (var file in EnumerateSafeFiles(directory)) yield return file;
        }
    }

    private static async Task CopyFileVerifiedAsync(string source, string destination, bool replaceStaging)
    {
        var hash = await LocalFiles.HashFileAsync(source);
        if (File.Exists(destination))
        {
            if (await LocalFiles.HashFileAsync(destination) == hash) return;
            if (!replaceStaging) throw new IOException("The destination contains a different existing file; it was preserved: " + destination);
        }
        Directory.CreateDirectory(Path.GetDirectoryName(destination)!);
        var temporary = destination + ".copy-" + Guid.NewGuid().ToString("N") + ".tmp";
        try
        {
            File.Copy(source, temporary, false);
            if (await LocalFiles.HashFileAsync(temporary) != hash || await LocalFiles.HashFileAsync(source) != hash)
                throw new IOException("A source file changed while it was copied. Close the application and retry.");
            File.Move(temporary, destination, replaceStaging);
        }
        finally { if (File.Exists(temporary)) File.Delete(temporary); }
    }

    private static async Task CopyAdaptationHistoryAsync(string source, string stage, string id)
    {
        var work = Path.Combine(source, "extensions", "work");
        if (!Directory.Exists(work)) return;
        foreach (var directory in Directory.EnumerateDirectories(work))
        {
            var name = Path.GetFileName(directory);
            if (!name.StartsWith(id + "-", StringComparison.OrdinalIgnoreCase)) continue;
            await CopyTreeAsync(directory, Path.Combine(stage, "adaptation", name), replaceStaging: true);
        }
    }

    private static async Task CopyGlobalsAsync(string source, string destination)
    {
        if (Same(source, destination)) return;
        var settings = Path.Combine(source, "settings.json");
        // Global API profiles retain their exact ciphertext; existing destination settings take priority.
        if (File.Exists(settings) && !File.Exists(Path.Combine(destination, "settings.json")))
            await CopyFileVerifiedAsync(settings, Path.Combine(destination, "settings.json"), false);
        var extensions = Path.Combine(source, "extensions");
        if (Directory.Exists(extensions))
            await CopyTreeAsync(extensions, Path.Combine(destination, "extensions"), replaceStaging: false);
    }

    private static async Task NormalizePathsAsync(string sourceProject, string stage)
    {
        foreach (var name in new[] { "source.json", "source.json.bak" })
        {
            var file = Path.Combine(stage, name);
            if (!File.Exists(file)) continue;
            var snapshot = await LocalFiles.ReadJsonAsync<SourceSnapshot>(file)
                ?? throw new InvalidDataException("The source snapshot is missing.");
            await LocalFiles.WriteJsonAsync(file, snapshot with
            {
                Directory = ProjectRelative(sourceProject, stage, snapshot.Directory, "sources")
            });
        }
        foreach (var name in new[] { "latest-result.json", "latest-result.json.bak" })
        {
            var file = Path.Combine(stage, name);
            if (!File.Exists(file)) continue;
            var result = await LocalFiles.ReadJsonAsync<PipelineResult>(file)
                ?? throw new InvalidDataException("The patch result is invalid.");
            await LocalFiles.WriteJsonAsync(file, result with
            {
                PatchDirectory = ProjectRelative(sourceProject, stage, result.PatchDirectory, "patches")
            });
        }
        var patches = Path.Combine(stage, "patches");
        if (!Directory.Exists(patches)) return;
        foreach (var file in EnumerateSafeFiles(patches).Where(path => Path.GetFileName(path) is "translation-report.json" or "translation-report.json.bak"))
        {
            var result = await LocalFiles.ReadJsonAsync<PipelineResult>(file)
                ?? throw new InvalidDataException("A patch report is invalid.");
            await LocalFiles.WriteJsonAsync(file, result with { PatchDirectory = Path.GetRelativePath(stage, Path.GetDirectoryName(file)!) });
        }
    }

    private static void CommitProject(string stage, string project, string backup)
    {
        Directory.CreateDirectory(Path.GetDirectoryName(project)!);
        if (Directory.Exists(project))
        {
            if (Directory.Exists(backup))
                throw new IOException("A legacy .bak directory already exists. Preserve it elsewhere before retrying.");
            Directory.Move(project, backup);
        }
        try { Directory.Move(stage, project); }
        catch
        {
            if (!Directory.Exists(project) && Directory.Exists(backup)) Directory.Move(backup, project);
            throw;
        }
    }

    private static GameTranslationSettings ReadGameSettings(JsonObject? legacy)
    {
        var defaults = new GameTranslationSettings();
        var api = legacy?["apiConfiguration"] as JsonObject;
        var chain = api?["translationProfileIds"]?.Deserialize<string[]>(LocalFiles.JsonOptions)
            ?? (api?["translationProfileId"]?.GetValue<string>() is { } id ? [id] : null);
        return defaults with
        {
            TargetLanguage = legacy?["targetLanguage"]?.GetValue<string>() ?? defaults.TargetLanguage,
            Style = legacy?["style"]?.GetValue<string>() ?? defaults.Style,
            Terminology = legacy?["terminology"]?.GetValue<string>() ?? defaults.Terminology,
            BatchSize = legacy?["batchSize"]?.GetValue<int>() ?? defaults.BatchSize,
            AnalysisProfileId = api?["analysisProfileId"]?.GetValue<string>(),
            TranslationProfileIds = chain
        };
    }

    private static string ProjectRelative(string sourceProject, string stage, string path, string kind)
    {
        var relative = Path.IsPathRooted(path) ? Path.GetRelativePath(sourceProject, path) : path;
        relative = relative.Replace('\\', '/');
        // A workspace copy can still reference the original absolute AppData directory.
        if (relative.StartsWith("../", StringComparison.Ordinal) || Path.IsPathRooted(relative))
        {
            var name = Path.GetFileName(path.TrimEnd('\\', '/'));
            relative = kind + "/" + name;
        }
        if (!relative.StartsWith(kind + "/", StringComparison.Ordinal) || !Directory.Exists(LocalFiles.UnderRoot(stage, relative)))
            throw new InvalidDataException("Legacy metadata references an unavailable project directory.");
        return relative;
    }

    private static async Task<bool> IsCurrentAsync(string project)
    {
        var file = Path.Combine(project, "game.json");
        if (!File.Exists(file)) return false;
        var descriptor = await ReadObjectAsync(file);
        return descriptor?["schemaVersion"]?.GetValue<int>() >= 2
            && Directory.Exists(Path.Combine(project, "memory")) && Directory.Exists(Path.Combine(project, "extraction"));
    }

    private static async Task<JsonObject?> ReadObjectAsync(string path) => File.Exists(path)
        ? JsonNode.Parse(await File.ReadAllTextAsync(path)) as JsonObject
            ?? throw new InvalidDataException("A settings or descriptor file is not a JSON object.")
        : null;

    private static FileStream AcquireRootLock(string root)
    {
        WorkspaceFiles.RejectLinkAncestors(root);
        try { return new FileStream(Path.Combine(root, ".app.lock"), FileMode.OpenOrCreate, FileAccess.ReadWrite, FileShare.None); }
        catch (IOException) { throw new IOException("The application or another data tool is using this data directory. Close it before migrating."); }
    }

    private static FileStream? ReadLockIfPresent(string path)
    {
        if (!File.Exists(path)) return null;
        try { return new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Delete); }
        catch (IOException) { throw new IOException("A concurrent writer is using the source data. Close the application before migrating."); }
    }

    private static void RejectRunningApp()
    {
        foreach (var name in new[] { "GameL10n.Desktop", "GameL10n", "AvgTranslator.Desktop", "AvgTranslator" })
            foreach (var process in Process.GetProcessesByName(name))
            {
                using (process)
                    if (!process.HasExited) throw new IOException("Close GameL10n before running the data tool.");
            }
    }

    private static async Task ActivateAsync(string bootstrap, string destination)
    {
        WorkspaceFiles.RejectLinkAncestors(bootstrap);
        Directory.CreateDirectory(bootstrap);
        await LocalFiles.WriteJsonAsync(Path.Combine(bootstrap, "location.json"), new { dataDirectory = destination });
    }

    private static async Task EmitAsync(object report, Dictionary<string, string?> options)
    {
        if (options.GetValueOrDefault("report") is { } path)
            await LocalFiles.WriteJsonAsync(Path.GetFullPath(path), report);
        Console.WriteLine(JsonSerializer.Serialize(report, LocalFiles.JsonOptions));
    }

    private static Dictionary<string, string?> Parse(string[] args)
    {
        var result = new Dictionary<string, string?>(StringComparer.Ordinal);
        var valid = new HashSet<string>(["game-directory", "all", "source-root", "destination-root", "activate", "bootstrap-root", "data-root", "report"]);
        for (var index = 0; index < args.Length; index++)
        {
            if (!args[index].StartsWith("--", StringComparison.Ordinal) || !valid.Contains(args[index][2..]))
                throw new ArgumentException("Unknown data tool option. Use --help.");
            var key = args[index][2..];
            string? value = null;
            if (key is not "activate" and not "all")
            {
                if (++index == args.Length || args[index].StartsWith("--", StringComparison.Ordinal))
                    throw new ArgumentException("Missing value for --" + key + ".");
                value = args[index];
            }
            if (!result.TryAdd(key, value)) throw new ArgumentException("Duplicate option --" + key + ".");
        }
        return result;
    }

    private static string Required(Dictionary<string, string?> options, string name) =>
        options.GetValueOrDefault(name) is { Length: > 0 } value ? value
            : throw new ArgumentException("Missing --" + name + ".");
    private static bool Same(string left, string right) => string.Equals(Path.GetFullPath(left).TrimEnd('\\', '/'), Path.GetFullPath(right).TrimEnd('\\', '/'), Paths);
    private static bool Inside(string root, string path) => Path.GetFullPath(path).StartsWith(Path.GetFullPath(root).TrimEnd('\\', '/') + Path.DirectorySeparatorChar, Paths);
    private static int CountFiles(string path) => Directory.Exists(path) ? Directory.EnumerateFiles(path).Count() : 0;
}
