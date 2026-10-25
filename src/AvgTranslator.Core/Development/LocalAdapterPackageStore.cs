using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Text.RegularExpressions;
using AvgTranslator.Core.Isolation;
using AvgTranslator.Core.Adapters;
using AvgTranslator.Core.Storage;

namespace AvgTranslator.Core.Development;

/// <summary>Packages grouped by engine/template, compatibility records, and per-game version pins.</summary>
public sealed partial class LocalAdapterPackageStore(string rootDirectory)
{
    private static readonly JsonSerializerOptions Json = new(JsonSerializerDefaults.Web) { WriteIndented = true };
    private readonly SemaphoreSlim activationLock = new(1, 1);
    public string RootDirectory { get; } = Path.GetFullPath(rootDirectory);
    public string WorkDirectory => Path.Combine(RootDirectory, "work");

    public async Task<GeneratedAdapterPackage> PublishAndActivateAsync(string gameId, GeneratedAdapterPackage package,
        IReadOnlyList<string> verificationDiagnostics, CancellationToken cancellationToken = default)
    {
        await activationLock.WaitAsync(cancellationToken);
        try
        {
            if (package.Family is null) throw new GeneratedAdapterException("An engine or template identity is required before publication.");
            package.Family.Validate();
            if (package.Id != package.Family.AdapterId) throw new GeneratedAdapterException("The package identity must match its engine or template.");
            var destination = PackagePath(package.Id, package.Version);
            var published = package with { DirectoryPath = destination };
            if (!Directory.Exists(destination))
            {
                var pending = destination + ".pending-" + Guid.NewGuid().ToString("N");
                Directory.CreateDirectory(pending);
                foreach (var file in new[] { "Program.cs", "adapter.dll", "adapter.runtimeconfig.json" })
                {
                    var bytes = WorkspaceFiles.ReadBounded(package.DirectoryPath, file, 8 * 1024 * 1024);
                    await File.WriteAllBytesAsync(Path.Combine(pending, file), bytes, cancellationToken);
                }
                await File.WriteAllTextAsync(Path.Combine(pending, "package.json"), JsonSerializer.Serialize(published, Json), cancellationToken);
                await File.WriteAllTextAsync(Path.Combine(pending, "verification.json"), JsonSerializer.Serialize(new
                {
                    verifiedAt = DateTimeOffset.UtcNow, diagnostics = verificationDiagnostics, runtimeTested = false
                }, Json), cancellationToken);
                Directory.Move(pending, destination);
            }
            VerifyPackage(published);
            var gameDirectory = GamePath(gameId);
            Directory.CreateDirectory(gameDirectory);
            var active = Path.Combine(gameDirectory, "active.json");
            if (File.Exists(active))
            {
                var current = JsonSerializer.Deserialize<Activation>(WorkspaceFiles.ReadBounded(gameDirectory, "active.json", 4096), Json);
                if (current?.Id == package.Id && current.Version == package.Version) return published;
            }
            var next = Path.Combine(gameDirectory, "active-" + Guid.NewGuid().ToString("N") + ".tmp");
            var contents = JsonSerializer.SerializeToUtf8Bytes(new Activation(package.Id, package.Version, DateTimeOffset.UtcNow), Json);
            await using (var stream = new FileStream(next, FileMode.CreateNew, FileAccess.Write, FileShare.None))
            {
                await stream.WriteAsync(contents, cancellationToken);
                stream.Flush(true);
            }
            cancellationToken.ThrowIfCancellationRequested();
            if (File.Exists(active)) File.Copy(active, Path.Combine(gameDirectory, "previous.json"), true);
            File.Move(next, active, true);
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

    private GeneratedAdapterPackage LoadPackage(Activation activation)
    {
        var packagePath = PackagePath(activation.Id, activation.Version);
        var package = JsonSerializer.Deserialize<GeneratedAdapterPackage>(WorkspaceFiles.ReadBounded(packagePath, "package.json", 64 * 1024), Json)
            ?? throw new GeneratedAdapterException("The local package manifest is invalid.");
        if (package.Id != activation.Id || package.Version != activation.Version)
            throw new GeneratedAdapterException("The activation and package manifests disagree.");
        package = package with { DirectoryPath = packagePath };
        VerifyPackage(package);
        return package;
    }

    /// <summary>Only pinned, verified releases are candidates. A rollback does not revive an inactive version.</summary>
    public IReadOnlyList<GeneratedAdapterPackage> ListReusablePackages(ICollection<string>? diagnostics = null)
    {
        var directory = WorkspaceFiles.Resolve(RootDirectory, "games");
        if (!Directory.Exists(directory)) return [];
        var found = new List<(DateTimeOffset ActivatedAt, GeneratedAdapterPackage Package)>();
        foreach (var gameDirectory in Directory.EnumerateDirectories(directory))
        {
            try
            {
                if (!File.Exists(WorkspaceFiles.Resolve(gameDirectory, "active.json"))) continue;
                var activation = JsonSerializer.Deserialize<Activation>(WorkspaceFiles.ReadBounded(gameDirectory, "active.json", 4096), Json)
                    ?? throw new GeneratedAdapterException("Invalid activation record.");
                var package = LoadPackage(activation);
                if (package.Family is not null) found.Add((activation.ActivatedAt, package));
            }
            catch (Exception error) when (error is IOException or InvalidDataException or UnauthorizedAccessException or JsonException or GeneratedAdapterException)
            {
                diagnostics?.Add("Skipped an unavailable local adapter: " + error.Message);
            }
        }
        return found.OrderByDescending(item => item.ActivatedAt).Select(item => item.Package)
            .DistinctBy(package => (package.Id, package.Version)).ToArray();
    }

    public Task RecordCompatibilityAsync(AdapterFamily family, GameCompatibility game, CancellationToken cancellationToken = default)
    {
        family.Validate();
        var fingerprint = LocalFiles.HashText(JsonSerializer.Serialize(game.SourceHashes.OrderBy(pair => pair.Key), Json));
        var key = LocalFiles.HashText(game.GameId) + "-" + fingerprint[..16];
        var path = WorkspaceFiles.Resolve(RootDirectory, Path.Combine("catalog", family.AdapterId, key + ".json"));
        return LocalFiles.WriteJsonAsync(path, new CompatibilityRecord(family, game), cancellationToken);
    }

    public IReadOnlyList<AdapterCatalogEntry> ReadCatalog(IEnumerable<AdapterCatalogEntry> builtIns, string query = "",
        ICollection<string>? diagnostics = null)
    {
        var entries = builtIns.ToList();
        var root = WorkspaceFiles.Resolve(RootDirectory, "catalog");
        if (Directory.Exists(root))
        {
            foreach (var directory in Directory.EnumerateDirectories(root))
            {
                WorkspaceFiles.RejectReparsePoints(directory);
                foreach (var file in Directory.EnumerateFiles(directory, "*.json"))
                {
                    try
                    {
                        var record = JsonSerializer.Deserialize<CompatibilityRecord>(
                            WorkspaceFiles.ReadBounded(directory, Path.GetFileName(file), 128 * 1024), Json)
                            ?? throw new InvalidDataException("Invalid compatibility record.");
                        record.Family.Validate();
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
            var packagePath = PackagePath(activation.Id, activation.Version);
            var package = JsonSerializer.Deserialize<GeneratedAdapterPackage>(WorkspaceFiles.ReadBounded(packagePath, "package.json", 64 * 1024), Json)
                ?? throw new GeneratedAdapterException("Invalid previous package.");
            VerifyPackage(package with { DirectoryPath = packagePath });
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

    private static void VerifyPackage(GeneratedAdapterPackage package)
    {
        if (package.Family is { } family)
        {
            family.Validate();
            if (package.Id != family.AdapterId) throw new GeneratedAdapterException("The engine/template and package identities disagree.");
        }
        var assembly = WorkspaceFiles.ReadBounded(package.DirectoryPath, "adapter.dll", 8 * 1024 * 1024);
        var source = WorkspaceFiles.ReadBounded(package.DirectoryPath, "Program.cs", 128 * 1024);
        if (Convert.ToHexStringLower(SHA256.HashData(assembly)) != package.AssemblySha256
            || Convert.ToHexStringLower(SHA256.HashData(source)) != package.SourceSha256)
            throw new GeneratedAdapterException("The local package failed its integrity check.");
        foreach (var file in package.ResourceFiles) _ = WorkspaceFiles.Resolve(package.DirectoryPath, file);
    }

    [GeneratedRegex("^[a-zA-Z0-9][a-zA-Z0-9._-]{0,95}$")]
    private static partial Regex PackagePart();
    private sealed record Activation(string Id, string Version, DateTimeOffset ActivatedAt);
    private sealed record CompatibilityRecord(AdapterFamily Family, GameCompatibility Game);
}
