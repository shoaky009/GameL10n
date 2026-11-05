using System.Text.Json;
using System.Security.Cryptography;
using GameL10n.Core.Adapters;
using GameL10n.Core.Isolation;
using GameL10n.Core.Storage;
using GameL10n.Core.Serialization;

namespace GameL10n.Core.Development;

public sealed partial class LocalAdapterPackageStore
{
    private const string CompatibilityCatalogFile = "games.json";

    private CompatibilityVersionRemoval ReadCompatibilityVersionRemoval(string adapterId, string version)
    {
        var directory = WorkspaceFiles.Resolve(RootDirectory, Path.Combine("catalog", adapterId));
        CompatibilityCatalog? updated = null;
        var catalogPath = WorkspaceFiles.Resolve(directory, CompatibilityCatalogFile);
        if (File.Exists(catalogPath))
        {
            var catalog = ReadCompatibilityCatalog(directory);
            var remaining = catalog.Games.Where(game => game.AdapterVersion != version).ToArray();
            if (remaining.Length == 0 || remaining.Length != catalog.Games.Count) updated = catalog with { Games = remaining };
        }
        var legacy = new List<string>();
        if (Directory.Exists(directory))
        {
            // Inspect every legacy observation, including those hidden by games.json or a newer observation of the same game.
            foreach (var file in Directory.EnumerateFiles(directory, "*.json"))
            {
                var name = Path.GetFileName(file);
                if (name == CompatibilityCatalogFile) continue;
                var path = WorkspaceFiles.Resolve(directory, name);
                var record = JsonSerializer.Deserialize<CompatibilityRecord>(ReadDirectMetadata(directory, name, 128 * 1024), EngineJson.TypeInfo<CompatibilityRecord>(Json))
                    ?? throw new InvalidDataException("兼容记录为空：" + path);
                ValidateCompatibilityFamily(record.Family, directory);
                ValidateCompatibilityGame(record.Game);
                if (record.Game.AdapterVersion == version) legacy.Add(path);
            }
        }
        return new(directory, updated, legacy);
    }

    private static async Task RemoveCompatibilityVersionAsync(CompatibilityVersionRemoval removal, CancellationToken ct)
    {
        var path = WorkspaceFiles.Resolve(removal.Directory, CompatibilityCatalogFile);
        if (removal.UpdatedCatalog is { } catalog)
        {
            if (catalog.Games.Count == 0) File.Delete(path);
            else await LocalFiles.WriteJsonAsync(path, catalog, EngineJson.TypeInfo<CompatibilityCatalog>(EngineJson.Storage), ct);
        }
        foreach (var legacy in removal.LegacyFiles)
        {
            ct.ThrowIfCancellationRequested();
            File.Delete(legacy);
        }
        DeleteEmptyDirectory(removal.Directory);
    }

    public async Task RecordCompatibilityAsync(AdapterFamily family, GameCompatibility game, CancellationToken cancellationToken = default)
    {
        family.Validate();
        ValidateCompatibilityGame(game);
        var directory = WorkspaceFiles.Resolve(RootDirectory, Path.Combine("catalog", family.AdapterId));
        await activationLock.WaitAsync(cancellationToken);
        try
        {
            var path = WorkspaceFiles.Resolve(directory, CompatibilityCatalogFile);
            var legacyFiles = new List<LegacyCompatibilityFile>();
            IReadOnlyList<GameCompatibility> prior;
            if (File.Exists(path))
            {
                try { prior = ReadCompatibilityCatalog(directory).Games; }
                catch (Exception error) when (IsLibraryReadError(error))
                { throw new InvalidDataException("无法更新兼容游戏清单 games.json，原文件已保留：" + error.Message, error); }
            }
            else
            {
                prior = ReadLegacyCompatibilityRecords(directory, null, legacyFiles).Select(record => record.Game).ToArray();
            }
            game = game with { UpdatedAt = DateTimeOffset.UtcNow };
            var games = LatestGames(prior.Where(item => item.GameId != game.GameId).Append(game));
            await LocalFiles.WriteJsonAsync(path, new CompatibilityCatalog(family, games), EngineJson.TypeInfo<CompatibilityCatalog>(EngineJson.Storage), cancellationToken);
            await UpdateLibraryIndexAsync(index => ReplaceIndexGames(index, family, games), CancellationToken.None);

            // Only remove the exact legacy bytes included in this committed catalog.
            foreach (var file in legacyFiles)
            {
                var legacyPath = WorkspaceFiles.Resolve(directory, file.Name);
                if (File.Exists(legacyPath) && File.GetLastWriteTimeUtc(legacyPath) == file.LastWriteTimeUtc
                    && SHA256.HashData(ReadDirectMetadata(directory, file.Name, 128 * 1024)).AsSpan().SequenceEqual(file.Sha256))
                    File.Delete(legacyPath);
            }
        }
        finally { activationLock.Release(); }
    }

    public IReadOnlyList<AdapterCatalogEntry> ReadCatalog(IEnumerable<AdapterCatalogEntry> builtIns, string query = "",
        ICollection<string>? diagnostics = null)
    {
        var entries = builtIns.ToList();
        foreach (var directory in SafeDirectories(WorkspaceFiles.Resolve(RootDirectory, "catalog"), diagnostics))
            foreach (var record in ReadCompatibilityRecords(directory, diagnostics))
                entries.Add(new(record.Family, record.Game.AdapterVersion, [record.Game]));

        return entries.GroupBy(entry => entry.Family.AdapterId)
            .SelectMany(group =>
            {
                var gamesByVersion = LatestGames(group.SelectMany(entry => entry.Games))
                    .GroupBy(game => game.AdapterVersion)
                    .ToDictionary(games => games.Key, games => games.ToArray(), StringComparer.Ordinal);
                return group.Select(entry => entry.Version).Distinct(StringComparer.Ordinal)
                    .Select(version => new AdapterCatalogEntry(group.First().Family, version,
                        gamesByVersion.GetValueOrDefault(version) ?? []));
            })
            .Where(entry => entry.Matches(query)).OrderBy(entry => entry.Family.Name, StringComparer.OrdinalIgnoreCase).ToArray();
    }

    private IReadOnlyList<CompatibilityRecord> ReadCompatibilityRecords(string directory, ICollection<string>? diagnostics)
    {
        if (!File.Exists(Path.Combine(directory, CompatibilityCatalogFile)))
            return ReadLegacyCompatibilityRecords(directory, diagnostics);
        try
        {
            var catalog = ReadCompatibilityCatalog(directory);
            return LatestGames(catalog.Games).Select(game => new CompatibilityRecord(catalog.Family, game)).ToArray();
        }
        catch (Exception error) when (IsLibraryReadError(error))
        {
            diagnostics?.Add("无法读取兼容游戏清单 " + directory + "/games.json：" + error.Message);
            return [];
        }
    }

    private static CompatibilityCatalog ReadCompatibilityCatalog(string directory)
    {
        var catalog = JsonSerializer.Deserialize<CompatibilityCatalog>(
            ReadDirectMetadata(directory, CompatibilityCatalogFile, 16 * 1024 * 1024), EngineJson.TypeInfo<CompatibilityCatalog>(Json))
            ?? throw new InvalidDataException("兼容游戏清单为空。");
        ValidateCompatibilityFamily(catalog.Family, directory);
        if (catalog.Games is null) throw new InvalidDataException("兼容游戏清单缺少游戏列表。");
        foreach (var game in catalog.Games) ValidateCompatibilityGame(game);
        return catalog;
    }

    private static IReadOnlyList<CompatibilityRecord> ReadLegacyCompatibilityRecords(string directory,
        ICollection<string>? diagnostics, List<LegacyCompatibilityFile>? migratedFiles = null)
    {
        var records = new List<CompatibilityRecord>();
        string[] files;
        try { files = Directory.Exists(directory) ? Directory.GetFiles(directory, "*.json") : []; }
        catch (Exception error) when (IsLibraryReadError(error))
        {
            if (migratedFiles is not null)
                throw new InvalidDataException("无法合并旧兼容记录，目录未修改：" + error.Message, error);
            diagnostics?.Add("无法读取兼容记录目录 " + directory + "：" + error.Message);
            return [];
        }
        foreach (var file in files)
        {
            try
            {
                var name = Path.GetFileName(file);
                var modifiedAt = File.GetLastWriteTimeUtc(file);
                var contents = ReadDirectMetadata(directory, name, 128 * 1024);
                var record = JsonSerializer.Deserialize<CompatibilityRecord>(contents, EngineJson.TypeInfo<CompatibilityRecord>(Json))
                    ?? throw new InvalidDataException("兼容记录为空。");
                ValidateCompatibilityFamily(record.Family, directory);
                ValidateCompatibilityGame(record.Game);
                records.Add(record with { Game = record.Game with { UpdatedAt = record.Game.UpdatedAt ?? new DateTimeOffset(modifiedAt) } });
                migratedFiles?.Add(new(name, SHA256.HashData(contents), modifiedAt));
            }
            catch (Exception error) when (IsLibraryReadError(error))
            {
                if (migratedFiles is not null)
                    throw new InvalidDataException("无法合并旧兼容记录 " + Path.GetFileName(file) + "，原文件已保留：" + error.Message, error);
                diagnostics?.Add("无法读取兼容记录 " + Path.GetFileName(file) + "：" + error.Message);
            }
        }
        if (records.Count == 0) return [];
        return LatestGames(records.Select(record => record.Game))
            .Select(game => new CompatibilityRecord(records[0].Family, game)).ToArray();
    }

    private static IReadOnlyList<GameCompatibility> LatestGames(IEnumerable<GameCompatibility> games)
    {
        var latest = new Dictionary<string, GameCompatibility>(StringComparer.Ordinal);
        foreach (var game in games)
            if (!latest.TryGetValue(game.GameId, out var previous)
                || (game.UpdatedAt ?? DateTimeOffset.MinValue) >= (previous.UpdatedAt ?? DateTimeOffset.MinValue))
                latest[game.GameId] = game;
        return latest.Values.ToArray();
    }

    private static void ValidateCompatibilityFamily(AdapterFamily? family, string directory)
    {
        if (family is null) throw new InvalidDataException("兼容记录缺少引擎信息。");
        family.Validate();
        if (family.AdapterId != Path.GetFileName(directory))
            throw new InvalidDataException("兼容记录与目录的引擎标识不一致。");
    }

    private static void ValidateCompatibilityGame(GameCompatibility? game)
    {
        if (game is null || string.IsNullOrWhiteSpace(game.GameId) || string.IsNullOrWhiteSpace(game.Title)
            || string.IsNullOrWhiteSpace(game.AdapterVersion) || !PackagePart().IsMatch(game.AdapterVersion)
            || game.Variant is null || string.IsNullOrWhiteSpace(game.StructureStatus) || string.IsNullOrWhiteSpace(game.RuntimeStatus))
            throw new InvalidDataException("兼容记录缺少游戏、版本或状态信息。");
    }

    internal sealed record CompatibilityRecord(AdapterFamily Family, GameCompatibility Game);
    internal sealed record CompatibilityCatalog(AdapterFamily Family, IReadOnlyList<GameCompatibility> Games);
    private sealed record LegacyCompatibilityFile(string Name, byte[] Sha256, DateTime LastWriteTimeUtc);
    private sealed record CompatibilityVersionRemoval(string Directory, CompatibilityCatalog? UpdatedCatalog,
        IReadOnlyList<string> LegacyFiles);
}
