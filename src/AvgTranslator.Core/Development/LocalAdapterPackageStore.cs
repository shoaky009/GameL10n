using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Text.RegularExpressions;
using AvgTranslator.Core.Isolation;
using AvgTranslator.Core.Adapters;
using AvgTranslator.Core.Storage;

namespace AvgTranslator.Core.Development;

/// <summary>Packages grouped by engine/template, compatibility records, and per-game version pins.</summary>
public sealed partial class LocalAdapterPackageStore(string rootDirectory, string? workDirectory = null)
{
    private static readonly JsonSerializerOptions Json = new(JsonSerializerDefaults.Web) { WriteIndented = true };
    private static readonly System.Collections.Concurrent.ConcurrentDictionary<string, SemaphoreSlim> PublicationLocks = new(StringComparer.OrdinalIgnoreCase);
    private readonly SemaphoreSlim activationLock = PublicationLocks.GetOrAdd(Path.GetFullPath(rootDirectory), _ => new(1, 1));
    public string RootDirectory { get; } = Path.GetFullPath(rootDirectory);
    public string WorkDirectory => workDirectory is null ? Path.Combine(RootDirectory, "work") : Path.GetFullPath(workDirectory);

    public async Task<GeneratedAdapterPackage> PublishAndActivateAsync(string gameId, GeneratedAdapterPackage package,
        IReadOnlyList<string> verificationDiagnostics, CancellationToken cancellationToken = default)
    {
        await activationLock.WaitAsync(cancellationToken);
        try
        {
            if (package.Family is null) throw new GeneratedAdapterException("An engine or template identity is required before publication.");
            package.Family.Validate();
            if (package.Id != package.Family.AdapterId) throw new GeneratedAdapterException("The package identity must match its engine or template.");
            VerifyPackage(package);
            var destination = PackagePath(package.Id, package.Version);
            var published = package with { DirectoryPath = destination };
            if (!Directory.Exists(destination))
            {
                var pending = destination + ".pending-" + Guid.NewGuid().ToString("N");
                Directory.CreateDirectory(pending);
                try
                {
                    var files = package.ManifestVersion >= 2 ? package.PackageFiles!.Keys
                        : new[] { "Program.cs", "adapter.dll", "adapter.runtimeconfig.json" }.AsEnumerable();
                    foreach (var file in files)
                        await GeneratedAdapterResources.CopyAsync(WorkspaceFiles.Resolve(package.DirectoryPath, file),
                            WorkspaceFiles.Resolve(pending, file), cancellationToken);
                    await File.WriteAllTextAsync(Path.Combine(pending, "package.json"), JsonSerializer.Serialize(published, Json), cancellationToken);
                    await File.WriteAllTextAsync(Path.Combine(pending, "verification.json"), JsonSerializer.Serialize(new
                    {
                        verifiedAt = DateTimeOffset.UtcNow, diagnostics = verificationDiagnostics, runtimeTested = false
                    }, Json), cancellationToken);
                    VerifyPackage(package with { DirectoryPath = pending });
                    Directory.Move(pending, destination);
                }
                finally { GeneratedGameAdapter.DeleteStagedTree(pending); }
            }
            VerifyPackage(published);
            var gameDirectory = GamePath(gameId);
            Directory.CreateDirectory(gameDirectory);
            var active = Path.Combine(gameDirectory, "active.json");
            if (File.Exists(active))
            {
                var current = JsonSerializer.Deserialize<Activation>(WorkspaceFiles.ReadBounded(gameDirectory, "active.json", 4096), Json);
                if (current?.Id == package.Id && current.Version == package.Version)
                {
                    await RecordReleaseAsync(current, published, cancellationToken);
                    return published;
                }
            }
            var next = Path.Combine(gameDirectory, "active-" + Guid.NewGuid().ToString("N") + ".tmp");
            var activation = new Activation(package.Id, package.Version, DateTimeOffset.UtcNow, gameId);
            var contents = JsonSerializer.SerializeToUtf8Bytes(activation, Json);
            await using (var stream = new FileStream(next, FileMode.CreateNew, FileAccess.Write, FileShare.None))
            {
                await stream.WriteAsync(contents, cancellationToken);
                stream.Flush(true);
            }
            cancellationToken.ThrowIfCancellationRequested();
            if (File.Exists(active)) File.Copy(active, Path.Combine(gameDirectory, "previous.json"), true);
            File.Move(next, active, true);
            await RecordReleaseAsync(activation, published, cancellationToken);
            return published;
        }
        finally { activationLock.Release(); }
    }

    public GeneratedAdapterPackage? LoadActive(string gameId)
    {
        var directory = GamePath(gameId);
        var active = WorkspaceFiles.Resolve(directory, "active.json");
        if (!File.Exists(active)) return null;
        var activation = JsonSerializer.Deserialize<Activation>(WorkspaceFiles.ReadBounded(directory, "active.json", 4096), Json)
            ?? throw new GeneratedAdapterException("The local activation manifest is invalid.");
        return LoadPackage(activation);
    }

    public GeneratedAdapterPackage LoadVersion(string adapterId, string version) =>
        LoadPackage(new(adapterId, version, DateTimeOffset.MinValue));

    private GeneratedAdapterPackage LoadPackage(Activation activation)
    {
        var packagePath = PackagePath(activation.Id, activation.Version);
        var package = ReadPackage(packagePath)
            ?? throw new GeneratedAdapterException("The local package manifest is invalid.");
        if (package.Id != activation.Id || package.Version != activation.Version)
            throw new GeneratedAdapterException("The activation and package manifests disagree.");
        package = package with { DirectoryPath = packagePath };
        VerifyPackage(package);
        return package;
    }

    /// <summary>Published releases remain candidates after their original project is removed; rollback withdraws a release.</summary>
    public IReadOnlyList<GeneratedAdapterPackage> ListReusablePackages(ICollection<string>? diagnostics = null)
    {
        List<Activation> activations;
        try { activations = ReadCandidateActivations(ReadLibraryState(), diagnostics); }
        catch (Exception error) when (IsLibraryReadError(error))
        { diagnostics?.Add("Skipped local adapter candidates: " + error.Message); return []; }
        var found = new List<GeneratedAdapterPackage>();
        // Deduplicate references before integrity checks, so each package is hashed at most once per call.
        foreach (var activation in activations.DistinctBy(item => (item.Id, item.Version)))
        {
            try
            {
                var package = LoadPackage(activation);
                if (package.Family is not null) found.Add(package);
            }
            catch (Exception error) when (error is IOException or InvalidDataException or UnauthorizedAccessException or JsonException or GeneratedAdapterException)
            {
                diagnostics?.Add("Skipped an unavailable local adapter: " + error.Message);
            }
        }
        return found;
    }

    public async Task RecordCompatibilityAsync(AdapterFamily family, GameCompatibility game, CancellationToken cancellationToken = default)
    {
        family.Validate();
        var fingerprint = LocalFiles.HashText(JsonSerializer.Serialize(game.SourceHashes.OrderBy(pair => pair.Key), Json));
        var key = LocalFiles.HashText(JsonSerializer.Serialize(new[] { game.GameId, game.AdapterVersion, game.Variant })) + "-" + fingerprint[..16];
        var path = WorkspaceFiles.Resolve(RootDirectory, Path.Combine("catalog", family.AdapterId, key + ".json"));
        await activationLock.WaitAsync(cancellationToken);
        try
        {
            await LocalFiles.WriteJsonAsync(path, new CompatibilityRecord(family, game), cancellationToken);
            await UpdateLibraryIndexAsync(index => AddIndexGame(index, family, IndexGame.From(game)), CancellationToken.None);
        }
        finally { activationLock.Release(); }
    }

    public IReadOnlyList<AdapterCatalogEntry> ReadCatalog(IEnumerable<AdapterCatalogEntry> builtIns, string query = "",
        ICollection<string>? diagnostics = null)
    {
        var entries = builtIns.ToList();
        var root = WorkspaceFiles.Resolve(RootDirectory, "catalog");
        if (Directory.Exists(root))
        {
            foreach (var directory in SafeDirectories(root, diagnostics))
            {
                string[] files;
                try { files = Directory.GetFiles(directory, "*.json"); }
                catch (Exception error) when (IsLibraryReadError(error))
                { diagnostics?.Add("Skipped an unreadable compatibility directory: " + error.Message); continue; }
                foreach (var file in files)
                {
                    try
                    {
                        var record = JsonSerializer.Deserialize<CompatibilityRecord>(
                            ReadDirectMetadata(directory, Path.GetFileName(file), 128 * 1024), Json)
                            ?? throw new InvalidDataException("Invalid compatibility record.");
                        if (record.Family is null || record.Game is null || record.Game.SourceHashes is null
                            || string.IsNullOrWhiteSpace(record.Game.GameId) || string.IsNullOrWhiteSpace(record.Game.Title)
                            || string.IsNullOrWhiteSpace(record.Game.AdapterVersion) || record.Game.Variant is null
                            || record.Game.StructureStatus is null)
                            throw new InvalidDataException("兼容记录缺少引擎或游戏信息。");
                        record.Family.Validate();
                        if (record.Family.AdapterId != Path.GetFileName(directory))
                            throw new InvalidDataException("兼容记录与目录的引擎标识不一致。");
                        entries.Add(new(record.Family, record.Game.AdapterVersion, [record.Game]));
                    }
                    catch (Exception error) when (error is IOException or InvalidDataException or UnauthorizedAccessException or JsonException)
                    {
                        diagnostics?.Add("Skipped an unreadable compatibility record: " + error.Message);
                    }
                }
            }
        }
        return entries.GroupBy(entry => (entry.Family.AdapterId, entry.Version))
            .Select(group => new AdapterCatalogEntry(group.First().Family, group.Key.Version,
                group.SelectMany(entry => entry.Games).Distinct().ToArray()))
            .Where(entry => entry.Matches(query)).OrderBy(entry => entry.Family.Name, StringComparer.OrdinalIgnoreCase).ToArray();
    }

    public async Task<bool> RollbackAsync(string gameId, CancellationToken cancellationToken = default)
    {
        await activationLock.WaitAsync(cancellationToken);
        try
        {
            var directory = GamePath(gameId);
            var previous = WorkspaceFiles.Resolve(directory, "previous.json");
            if (!File.Exists(previous)) return false;
            var contents = WorkspaceFiles.ReadBounded(directory, "previous.json", 4096);
            var activation = JsonSerializer.Deserialize<Activation>(contents, Json) ?? throw new GeneratedAdapterException("Invalid previous activation.");
            var activeFile = WorkspaceFiles.Resolve(directory, "active.json");
            var current = File.Exists(activeFile)
                ? JsonSerializer.Deserialize<Activation>(WorkspaceFiles.ReadBounded(directory, "active.json", 4096), Json) : null;
            _ = LoadPackage(activation);
            var state = ReadLibraryState();
            if (current is not null && (current.Id != activation.Id || current.Version != activation.Version))
            {
                var currentFamily = state.Families.GetValueOrDefault(current.Id) ?? new FamilyState();
                state.Families[current.Id] = currentFamily with
                {
                    ExcludedVersions = (currentFamily.ExcludedVersions ?? []).Append(current.Version).Distinct(StringComparer.Ordinal).ToArray()
                };
            }
            var previousFamily = state.Families.GetValueOrDefault(activation.Id) ?? new FamilyState();
            state.Families[activation.Id] = previousFamily with
            {
                Releases = (previousFamily.Releases ?? []).Where(item => item.Version != activation.Version).Append(activation).ToArray(),
                ExcludedVersions = (previousFamily.ExcludedVersions ?? []).Where(version => version != activation.Version).ToArray()
            };
            await WriteLibraryStateAsync(state, cancellationToken);
            var next = Path.Combine(directory, "rollback-" + Guid.NewGuid().ToString("N") + ".tmp");
            await File.WriteAllBytesAsync(next, contents, cancellationToken);
            File.Move(next, WorkspaceFiles.Resolve(directory, "active.json"), true);
            return true;
        }
        finally { activationLock.Release(); }
    }

    private string PackagePath(string id, string version)
    {
        if (!PackagePart().IsMatch(id) || !PackagePart().IsMatch(version)) throw new GeneratedAdapterException("Invalid package identity.");
        return WorkspaceFiles.Resolve(RootDirectory, Path.Combine("packages", id, version));
    }

    private string GamePath(string gameId)
    {
        if (string.IsNullOrWhiteSpace(gameId)) throw new ArgumentException("Game identity is required.", nameof(gameId));
        var key = Convert.ToHexStringLower(SHA256.HashData(Encoding.UTF8.GetBytes(gameId)));
        return WorkspaceFiles.Resolve(RootDirectory, Path.Combine("games", key));
    }

    private static void VerifyPackage(GeneratedAdapterPackage package) => GeneratedAdapterResources.VerifyPackage(package);

    private static GeneratedAdapterPackage? ReadPackage(string directory)
    {
        return JsonSerializer.Deserialize<GeneratedAdapterPackage>(WorkspaceFiles.ReadBounded(directory, "package.json", 16 * 1024 * 1024), Json);
    }

    [GeneratedRegex("^[a-zA-Z0-9][a-zA-Z0-9._-]{0,95}$")]
    private static partial Regex PackagePart();
    private sealed record Activation(string Id, string Version, DateTimeOffset ActivatedAt, string? GameId = null);
    private sealed record CompatibilityRecord(AdapterFamily Family, GameCompatibility Game);
}
