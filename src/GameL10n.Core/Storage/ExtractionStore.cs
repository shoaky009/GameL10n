using System.Security.Cryptography;
using System.Text.Json;

namespace GameL10n.Core.Storage;

public sealed record ExtractionResource(string ResourcePath, string File, string Sha256, int UnitCount);
public sealed record ExtractionManifest(int SchemaVersion, string SourceFingerprint, string AdapterId,
    string AdapterVersion, IReadOnlyList<ExtractionResource> Resources, IReadOnlyList<string> UnitOrder,
    IReadOnlyList<string> Warnings);

/// <summary>Immutable extraction cache bound to verified source bytes and an adapter version.</summary>
public sealed class ExtractionStore(string projectDirectory)
{
    public static string CacheKey(string sourceFingerprint, string adapterId, string adapterVersion) =>
        LocalFiles.HashText(JsonSerializer.Serialize(new { sourceFingerprint, adapterId, adapterVersion }));

    public async Task<ExtractionResult?> LoadAsync(string sourceFingerprint, string adapterId, string adapterVersion,
        CancellationToken ct = default)
    {
        var directory = CacheDirectory(sourceFingerprint, adapterId, adapterVersion);
        try
        {
            var manifestPath = Path.Combine(directory, "manifest.json");
            if (!File.Exists(manifestPath)) return null;
            var manifestBytes = await File.ReadAllBytesAsync(manifestPath, ct);
            if (Convert.ToHexString(SHA256.HashData(manifestBytes)).ToLowerInvariant() !=
                (await File.ReadAllTextAsync(Path.Combine(directory, "manifest.sha256"), ct)).Trim()) return null;
            var manifest = JsonSerializer.Deserialize<ExtractionManifest>(manifestBytes, LocalFiles.JsonOptions);
            if (manifest is null) return null;
            if (manifest.SchemaVersion != 1 || manifest.SourceFingerprint != sourceFingerprint ||
                manifest.AdapterId != adapterId || manifest.AdapterVersion != adapterVersion ||
                manifest.Resources is null || manifest.UnitOrder is null || manifest.Warnings is null)
                return null;
            var units = new Dictionary<string, TranslationUnit>(StringComparer.Ordinal);
            var shards = new IReadOnlyList<TranslationUnit>[manifest.Resources.Count];
            await Parallel.ForEachAsync(Enumerable.Range(0, manifest.Resources.Count),
                new ParallelOptions { MaxDegreeOfParallelism = 8, CancellationToken = ct }, async (index, token) =>
            {
                var resource = manifest.Resources[index];
                var bytes = await File.ReadAllBytesAsync(LocalFiles.UnderRoot(directory, resource.File), token);
                if (Convert.ToHexString(SHA256.HashData(bytes)).ToLowerInvariant() != resource.Sha256)
                    throw new InvalidDataException("提取资源缓存哈希不一致。");
                var shard = JsonSerializer.Deserialize<ResourceExtraction>(bytes, LocalFiles.JsonOptions);
                if (shard?.SchemaVersion != 1 || shard.ResourcePath != resource.ResourcePath ||
                    shard.Units is null || shard.Units.Count != resource.UnitCount || shard.Units.Any(unit =>
                        unit.ResourcePath != resource.ResourcePath || string.IsNullOrWhiteSpace(unit.Id) || string.IsNullOrWhiteSpace(unit.SourceText)))
                    throw new InvalidDataException("提取资源缓存内容不一致。");
                shards[index] = shard.Units;
            });
            foreach (var shard in shards)
                foreach (var unit in shard)
                    if (!units.TryAdd(unit.Id, unit)) return null;
            if (units.Count != manifest.UnitOrder.Count || manifest.UnitOrder.Distinct(StringComparer.Ordinal).Count() != units.Count ||
                manifest.UnitOrder.Any(id => !units.ContainsKey(id))) return null;
            return new ExtractionResult(manifest.UnitOrder.Select(id => units[id]).ToArray(), manifest.Warnings);
        }
        catch (Exception error) when (error is JsonException or InvalidDataException or FileNotFoundException or DirectoryNotFoundException)
        {
            // Extraction is reproducible from the verified source snapshot; a damaged cache is a miss.
            return null;
        }
    }

    public async Task StoreAsync(string sourceFingerprint, string adapterId, string adapterVersion,
        ExtractionResult extraction, CancellationToken ct = default)
    {
        if (extraction.Units.Count == 0 || extraction.Units.Any(unit => string.IsNullOrWhiteSpace(unit.Id) ||
                string.IsNullOrWhiteSpace(unit.SourceText) || string.IsNullOrWhiteSpace(unit.ResourcePath)) ||
            extraction.Units.Select(unit => unit.Id).Distinct(StringComparer.Ordinal).Count() != extraction.Units.Count)
            throw new InvalidDataException("提取结果包含空文本或重复的文本 ID。");
        var directory = CacheDirectory(sourceFingerprint, adapterId, adapterVersion);
        if (await LoadAsync(sourceFingerprint, adapterId, adapterVersion, ct) is not null) return;
        var staging = directory + ".pending-" + Guid.NewGuid().ToString("N");
        Directory.CreateDirectory(staging);
        var resources = new List<ExtractionResource>();
        foreach (var group in extraction.Units.GroupBy(unit => unit.ResourcePath, StringComparer.Ordinal))
        {
            var relative = "resources/" + LocalFiles.HashText(group.Key) + ".json";
            var path = LocalFiles.UnderRoot(staging, relative);
            var units = group.ToArray();
            await LocalFiles.WriteJsonAsync(path, new ResourceExtraction(1, group.Key, units), ct);
            resources.Add(new(group.Key, relative, await LocalFiles.HashFileAsync(path, ct), units.Length));
        }
        await LocalFiles.WriteJsonAsync(Path.Combine(staging, "manifest.json"), new ExtractionManifest(1, sourceFingerprint,
            adapterId, adapterVersion, resources, extraction.Units.Select(unit => unit.Id).ToArray(), extraction.Warnings), ct);
        await File.WriteAllTextAsync(Path.Combine(staging, "manifest.sha256"),
            await LocalFiles.HashFileAsync(Path.Combine(staging, "manifest.json"), ct), ct);
        if (Directory.Exists(directory)) Directory.Move(directory, directory + ".damaged-" + Guid.NewGuid().ToString("N"));
        Directory.Move(staging, directory);
    }

    private string CacheDirectory(string fingerprint, string adapterId, string version) =>
        Path.Combine(projectDirectory, "extraction", CacheKey(fingerprint, adapterId, version));

    private sealed record ResourceExtraction(int SchemaVersion, string ResourcePath, IReadOnlyList<TranslationUnit> Units);
}
