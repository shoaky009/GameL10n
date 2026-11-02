using System.Collections.Concurrent;
using System.Text.Json;
using System.Text.Json.Serialization;
using AvgTranslator.Core.Adapters;
using AvgTranslator.Core.Isolation;

namespace AvgTranslator.Core.Development;

public sealed partial class LocalAdapterPackageStore
{
    private static readonly ConcurrentDictionary<string, SemaphoreSlim> IndexLocks = new(StringComparer.OrdinalIgnoreCase);
    private static readonly JsonSerializerOptions IndexJson = new(JsonSerializerDefaults.Web);
    private const string IndexFile = "library-index.json";

    /// <summary>One display index; browsing never opens projects, packages or their payloads.</summary>
    public Task<EngineLibrarySnapshot> ReadEngineLibraryAsync(IEnumerable<AdapterCatalogEntry> builtIns,
        string dataDirectory, IReadOnlyList<EngineLibraryReference>? runtimeReferences = null,
        CancellationToken cancellationToken = default)
    {
        var entries = builtIns.ToArray();
        var running = runtimeReferences?.ToArray() ?? [];
        return WithLibraryIndexAsync(async () =>
        {
            var index = await ReadLibraryIndexAsync(cancellationToken);
            var families = new Dictionary<string, EngineLibraryFamily>(StringComparer.Ordinal);
            var builtInGroups = entries.GroupBy(entry => entry.Family.AdapterId)
                .ToDictionary(group => group.Key, group => group.ToArray(), StringComparer.Ordinal);
            foreach (var id in index.Families.Keys.Union(builtInGroups.Keys, StringComparer.Ordinal))
            {
                var stored = index.Families.GetValueOrDefault(id);
                var builtIn = builtInGroups.GetValueOrDefault(id) ?? [];
                var family = builtIn.FirstOrDefault()?.Family ?? stored!.Family;
                var enabled = stored?.Enabled ?? true;
                var games = (stored?.Games.Select(game => game.ToCompatibility()) ?? [])
                    .Concat(builtIn.SelectMany(entry => entry.Games)).DistinctBy(GameKey).ToArray();
                var gamesByVersion = games.GroupBy(game => game.AdapterVersion)
                    .ToDictionary(group => group.Key, group => group.ToArray(), StringComparer.Ordinal);
                var references = running.Where(reference => reference.AdapterId == id).Distinct().ToArray();
                var versions = new List<EngineLibraryVersion>();
                foreach (var version in (IEnumerable<IndexVersion>?)stored?.Versions.Values ?? [])
                    versions.Add(new(id, version.Version, version.Source, version.Exists,
                        version.Source == EngineLibrarySource.External ? Path.Combine(RootDirectory, "packages", id, version.Version) : null,
                        version.PublishedAt, enabled, enabled && version.IsCandidate,
                        gamesByVersion.GetValueOrDefault(version.Version) ?? [],
                        references.Where(reference => reference.Version == version.Version).ToArray()));
                foreach (var entry in builtIn.DistinctBy(entry => entry.Version))
                {
                    versions.RemoveAll(version => version.Version == entry.Version && version.Source == EngineLibrarySource.History);
                    versions.Add(new(id, entry.Version, EngineLibrarySource.BuiltIn, true, null, null, enabled, enabled,
                        gamesByVersion.GetValueOrDefault(entry.Version) ?? [],
                        references.Where(reference => reference.Version == entry.Version).ToArray()));
                }
                families[id] = new(family, enabled, versions.OrderBy(version => version.Source)
                    .ThenByDescending(version => version.PublishedAt).ThenBy(version => version.Version, StringComparer.Ordinal).ToArray(), games, references);
            }
            return new EngineLibrarySnapshot(families.Values.OrderBy(family => family.Family.Name, StringComparer.OrdinalIgnoreCase).ToArray(), index.Diagnostics)
            { Knowledge = index.Knowledge.Values.ToArray() };
        }, cancellationToken);
    }

    /// <summary>Inspect only this version's required files. Integrity and project references are checked on use/deletion.</summary>
    public Task<EngineLibraryVersion> InspectLibraryVersionAsync(EngineLibraryVersion version, CancellationToken cancellationToken = default) =>
        Task.Run(() =>
        {
            cancellationToken.ThrowIfCancellationRequested();
            if (version.Source != EngineLibrarySource.External) return version;
            var path = PackagePath(version.AdapterId, version.Version);
            var exists = Directory.Exists(path) && new[] { "package.json", "adapter.dll", "adapter.runtimeconfig.json" }
                .All(name => File.Exists(WorkspaceFiles.Resolve(path, name)));
            return version with { Exists = exists, DirectoryPath = path };
        }, cancellationToken);

    internal Task RecordKnowledgeIndexAsync(EngineKnowledgeSummary summary, CancellationToken ct) =>
        UpdateLibraryIndexAsync(index => index.Knowledge[summary.Id] = summary, ct);

    private Task UpdateLibraryIndexAsync(Action<LibraryIndex> update, CancellationToken ct) =>
        WithLibraryIndexAsync(async () =>
        {
            try
            {
                var index = await ReadLibraryIndexAsync(ct);
                update(index);
                await WriteLibraryIndexAsync(index, ct);
            }
            catch (Exception error) when (IsLibraryReadError(error) || error is OperationCanceledException)
            {
                // An authoritative write may already have committed. Never leave a valid-looking stale index.
                try { File.Delete(WorkspaceFiles.Resolve(RootDirectory, IndexFile)); }
                catch (Exception invalidationError) when (IsLibraryReadError(invalidationError))
                { throw new IOException("引擎数据已保存，但索引同步失败且无法使旧索引失效：" + invalidationError.Message, error); }
                throw;
            }
            return true;
        }, ct);

    // This lock is independent of publication. A list read cannot queue behind a package copy or reference scan.
    // Writers may hold activationLock before taking this lock; index rebuilds never take activationLock.
    private Task<T> WithLibraryIndexAsync<T>(Func<Task<T>> action, CancellationToken ct) => Task.Run(async () =>
    {
        var gate = IndexLocks.GetOrAdd(RootDirectory, _ => new(1, 1));
        await gate.WaitAsync(ct);
        try { return await action(); }
        finally { gate.Release(); }
    }, ct);

    private async Task<LibraryIndex> ReadLibraryIndexAsync(CancellationToken ct)
    {
        var path = WorkspaceFiles.Resolve(RootDirectory, IndexFile);
        if (File.Exists(path))
        {
            try
            {
                var index = JsonSerializer.Deserialize<LibraryIndex>(ReadDirectMetadata(RootDirectory, IndexFile, 64 * 1024 * 1024), IndexJson)
                    ?? throw new InvalidDataException("引擎索引为空。");
                ValidateIndex(index);
                return index;
            }
            catch (Exception error) when (error is JsonException or InvalidDataException)
            { /* Derived metadata is recoverable from the registry, without opening any project data. */ }
        }
        var rebuilt = RebuildLibraryIndex(ct);
        await WriteLibraryIndexAsync(rebuilt, ct);
        return rebuilt;
    }

    private LibraryIndex RebuildLibraryIndex(CancellationToken ct)
    {
        var index = new LibraryIndex();
        LibraryState state;
        try { state = ReadLibraryState(); }
        catch (Exception error) when (IsLibraryReadError(error))
        { throw new InvalidDataException("无法重建引擎索引：启停状态无法读取。", error); }

        // Registry metadata only: no game pins, project snapshots, payloads, source trees or content hashes.
        foreach (var directory in SafeDirectories(WorkspaceFiles.Resolve(RootDirectory, "catalog"), index.Diagnostics))
        {
            foreach (var file in Directory.EnumerateFiles(directory, "*.json"))
            {
                ct.ThrowIfCancellationRequested();
                try
                {
                    var record = JsonSerializer.Deserialize<IndexCompatibility>(ReadDirectMetadata(directory, Path.GetFileName(file), 128 * 1024), IndexJson)
                        ?? throw new InvalidDataException("兼容记录为空。");
                    if (record.Family is null || record.Game is null) throw new InvalidDataException("兼容记录缺少引擎或游戏信息。");
                    record.Family.Validate();
                    if (record.Family.AdapterId != Path.GetFileName(directory)) throw new InvalidDataException("兼容记录与目录不一致。");
                    AddIndexGame(index, record.Family, record.Game);
                }
                catch (Exception error) when (IsLibraryReadError(error))
                { index.Diagnostics.Add("无法读取兼容记录 " + Path.GetFileName(file) + "：" + error.Message); }
            }
        }
        var namedFamilies = index.Families.Keys.ToHashSet(StringComparer.Ordinal);
        ApplyIndexState(index, state);
        foreach (var familyDirectory in SafeDirectories(WorkspaceFiles.Resolve(RootDirectory, "packages"), index.Diagnostics))
        {
            var id = Path.GetFileName(familyDirectory);
            if (!TryFamily(id, out var fallback)) continue;
            foreach (var directory in SafeDirectories(familyDirectory, index.Diagnostics, ancestorsChecked: true))
            {
                ct.ThrowIfCancellationRequested();
                var version = Path.GetFileName(directory);
                if (version.Contains(".pending-", StringComparison.Ordinal) || !PackagePart().IsMatch(version)) continue;
                var family = GetIndexFamily(index, fallback);
                // Releases already have registry metadata. Do not reopen every version's manifest on a rebuild.
                if (namedFamilies.Contains(id) && family.Versions.GetValueOrDefault(version) is { Source: EngineLibrarySource.External }) continue;
                try
                {
                    var package = JsonSerializer.Deserialize<IndexPackage>(ReadDirectMetadata(directory, "package.json", 16 * 1024 * 1024), IndexJson)
                        ?? throw new InvalidDataException("包清单为空。");
                    if (package.Family is null || package.Id != id || package.Version != version || package.Family.AdapterId != id)
                        throw new InvalidDataException("包清单与目录不一致。");
                    package.Family.Validate();
                    family.Family = package.Family;
                    namedFamilies.Add(id);
                }
                catch (Exception error) when (IsLibraryReadError(error))
                { index.Diagnostics.Add("无法读取外置引擎 " + id + "/" + version + "：" + error.Message); }
                family.Versions[version] = new(version, EngineLibrarySource.External, true, ReadPublishedAt(directory, index.Diagnostics));
            }
        }
        ApplyIndexState(index, state);
        foreach (var summary in EngineKnowledgeStore.ForAdapters(this).ReadSummaries(index.Diagnostics.Add))
            index.Knowledge[summary.Id] = summary;
        return index;
    }

    private async Task WriteLibraryIndexAsync(LibraryIndex index, CancellationToken ct)
    {
        var path = WorkspaceFiles.Resolve(RootDirectory, IndexFile);
        Directory.CreateDirectory(RootDirectory);
        var temporary = path + "." + Guid.NewGuid().ToString("N") + ".tmp";
        try
        {
            await using (var stream = new FileStream(temporary, FileMode.CreateNew, FileAccess.Write, FileShare.None, 65536, FileOptions.Asynchronous))
            {
                await JsonSerializer.SerializeAsync(stream, index, IndexJson, ct);
                await stream.FlushAsync(ct);
            }
            ct.ThrowIfCancellationRequested();
            File.Move(temporary, path, true);
        }
        finally { if (File.Exists(temporary)) File.Delete(temporary); }
    }

    private static IndexFamily GetIndexFamily(LibraryIndex index, AdapterFamily family)
    {
        if (!index.Families.TryGetValue(family.AdapterId, out var stored))
            index.Families[family.AdapterId] = stored = new() { Family = family };
        return stored;
    }

    private static void AddIndexGame(LibraryIndex index, AdapterFamily family, IndexGame game)
    {
        if (string.IsNullOrWhiteSpace(game.GameId) || string.IsNullOrWhiteSpace(game.Title)
            || string.IsNullOrWhiteSpace(game.AdapterVersion) || !PackagePart().IsMatch(game.AdapterVersion)
            || game.Variant is null || game.StructureStatus is null || game.RuntimeStatus is null)
            throw new InvalidDataException("兼容记录缺少游戏或版本信息。");
        var stored = GetIndexFamily(index, family);
        stored.Family = family;
        stored.Games.RemoveAll(item => item.GameId == game.GameId && item.AdapterVersion == game.AdapterVersion && item.Variant == game.Variant);
        stored.Games.Add(game);
        stored.Versions.TryAdd(game.AdapterVersion, new(game.AdapterVersion, EngineLibrarySource.History, false));
    }

    private static void ApplyIndexState(LibraryIndex index, LibraryState state, GeneratedAdapterPackage? published = null)
    {
        if (published?.Family is { } publishedFamily)
        {
            var family = GetIndexFamily(index, publishedFamily);
            family.Family = publishedFamily;
            var prior = family.Versions.GetValueOrDefault(published.Version);
            family.Versions[published.Version] = new(published.Version, EngineLibrarySource.External, true,
                prior?.PublishedAt ?? DateTimeOffset.UtcNow, Discovery: Descriptor(published), DiscoveryLoaded: true);
        }
        foreach (var (id, setting) in state.Families)
        {
            if (!TryFamily(id, out var identity)) continue;
            var family = GetIndexFamily(index, identity);
            family.Enabled = setting.Enabled;
            foreach (var release in setting.Releases ?? [])
                if (!family.Versions.TryGetValue(release.Version, out var existing) || existing.Source == EngineLibrarySource.History)
                    family.Versions[release.Version] = new(release.Version, EngineLibrarySource.External, true, release.ActivatedAt);
        }
        foreach (var (id, family) in index.Families)
        {
            var setting = state.Families.GetValueOrDefault(id);
            family.Enabled = setting?.Enabled ?? true;
            var candidates = (setting?.Releases ?? []).Select(release => release.Version).ToHashSet(StringComparer.Ordinal);
            candidates.ExceptWith(setting?.ExcludedVersions ?? []);
            foreach (var (version, entry) in family.Versions.ToArray())
                family.Versions[version] = entry with { IsCandidate = candidates.Contains(version) };
        }
    }

    private static void ValidateIndex(LibraryIndex index)
    {
        if (index.SchemaVersion != 1 || index.Families is null || index.Knowledge is null || index.Diagnostics is null)
            throw new InvalidDataException("引擎索引格式无效。");
        foreach (var (id, family) in index.Families)
        {
            if (family?.Family is null || family.Family.AdapterId != id || family.Versions is null || family.Games is null)
                throw new InvalidDataException("引擎索引分类无效。");
            family.Family.Validate();
            foreach (var (version, entry) in family.Versions)
                if (entry is null || entry.Version != version || !PackagePart().IsMatch(version) || !Enum.IsDefined(entry.Source))
                    throw new InvalidDataException("引擎索引版本无效。");
            if (family.Games.Any(game => game is null || string.IsNullOrWhiteSpace(game.GameId) || string.IsNullOrWhiteSpace(game.Title)
                || string.IsNullOrWhiteSpace(game.AdapterVersion) || game.Variant is null))
                throw new InvalidDataException("引擎索引兼容记录无效。");
        }
        foreach (var (id, summary) in index.Knowledge)
        {
            if (summary?.Family is null || summary.Id != id || summary.Aliases is null || summary.Origins is null || summary.Summary is null
                || summary.Conditions is null) throw new InvalidDataException("引擎经验索引无效。");
            summary.Family.Validate();
        }
    }

    private static (string, string, string) GameKey(GameCompatibility game) => (game.GameId, game.AdapterVersion, game.Variant);
    private sealed record IndexPackage(string Id, string Version, AdapterFamily? Family);
    private sealed record IndexCompatibility(AdapterFamily Family, IndexGame Game);
    private sealed record IndexGame(string GameId, string Title, string Variant, string AdapterVersion,
        string StructureStatus, string RuntimeStatus = "not-tested", string? PatchDirectory = null)
    {
        public GameCompatibility ToCompatibility() => new(GameId, Title, Variant, AdapterVersion,
            new Dictionary<string, string>(), StructureStatus, RuntimeStatus, PatchDirectory);
        public static IndexGame From(GameCompatibility game) => new(game.GameId, game.Title, game.Variant, game.AdapterVersion,
            game.StructureStatus, game.RuntimeStatus, game.PatchDirectory);
    }
    private sealed record IndexVersion(string Version, EngineLibrarySource Source, bool Exists, DateTimeOffset? PublishedAt = null,
        bool IsCandidate = false, AdapterDiscoveryDescriptor? Discovery = null, bool DiscoveryLoaded = false);
    private sealed class IndexFamily
    {
        public required AdapterFamily Family { get; set; }
        [JsonRequired] public bool Enabled { get; set; } = true;
        [JsonRequired] public Dictionary<string, IndexVersion> Versions { get; init; } = new(StringComparer.Ordinal);
        [JsonRequired] public List<IndexGame> Games { get; init; } = [];
    }
    private sealed class LibraryIndex
    {
        [JsonRequired] public int SchemaVersion { get; init; } = 1;
        public bool DiscoveryInitialized { get; set; }
        [JsonRequired] public Dictionary<string, IndexFamily> Families { get; init; } = new(StringComparer.Ordinal);
        [JsonRequired] public Dictionary<string, EngineKnowledgeSummary> Knowledge { get; init; } = new(StringComparer.Ordinal);
        [JsonRequired] public List<string> Diagnostics { get; init; } = [];
    }
}
