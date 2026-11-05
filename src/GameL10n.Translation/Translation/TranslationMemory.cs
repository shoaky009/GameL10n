using System.Text.Json;
using System.Text.Json.Serialization;
using GameL10n.Core.Storage;
using System.Text.Json.Nodes;
using GameL10n.Core.Serialization;

namespace GameL10n.Core.Translation;

public sealed record TranslationPolicy(string TargetLanguage, string Style,
    IReadOnlyDictionary<string, string> Terminology, string Version = "1");

public sealed record MemoryEntry(string Key, string SourceText, string Context, string Translation,
    string Provider, DateTimeOffset CreatedAt);

[JsonConverter(typeof(MemoryShardConverter))]
public sealed record MemoryShard(int SchemaVersion, IReadOnlyDictionary<string, MemoryEntry> Entries);

// The dictionary already stores the key. Keep it in the in-memory model for batch writes,
// but persist it only once. Version 1 shards and legacy individual entries remain readable.
internal sealed class MemoryShardConverter : JsonConverter<MemoryShard>
{
    internal sealed record StoredShard(int SchemaVersion, Dictionary<string, MemoryEntry> Entries);
    internal sealed record WriteShard(int SchemaVersion, IReadOnlyDictionary<string, StoredEntry> Entries);
    internal sealed record StoredEntry(string SourceText, string Context, string Translation, string Provider, DateTimeOffset CreatedAt);

    public override MemoryShard Read(ref Utf8JsonReader reader, Type typeToConvert, JsonSerializerOptions options)
    {
        var stored = JsonSerializer.Deserialize<StoredShard>(ref reader, TranslationJson.TypeInfo<StoredShard>(options))
            ?? throw new JsonException("翻译缓存分片为空。");
        if (stored.SchemaVersion is not (1 or 2) || stored.Entries is null)
            throw new JsonException("不支持的翻译缓存格式。");
        foreach (var (key, entry) in stored.Entries)
            if (entry is null || (stored.SchemaVersion == 1 ? entry.Key != key : entry.Key is not null && entry.Key != key))
                throw new JsonException("翻译缓存键不一致。");
        return new(stored.SchemaVersion, stored.Entries.ToDictionary(pair => pair.Key,
            pair => pair.Value with { Key = pair.Key }, StringComparer.Ordinal));
    }

    public override void Write(Utf8JsonWriter writer, MemoryShard value, JsonSerializerOptions options) =>
        JsonSerializer.Serialize(writer, new WriteShard(2, value.Entries.ToDictionary(pair => pair.Key,
            pair => new StoredEntry(pair.Value.SourceText, pair.Value.Context, pair.Value.Translation,
                pair.Value.Provider, pair.Value.CreatedAt), StringComparer.Ordinal)), TranslationJson.TypeInfo<WriteShard>(options));
}

public sealed class TranslationMemory(string directory, string? legacyDirectory = null)
{
    private readonly SemaphoreSlim gate = new(1, 1);
    private readonly Dictionary<string, MemoryEntry> entries = new(StringComparer.Ordinal);
    private volatile bool initialized;
    internal IReadOnlyDictionary<string, MemoryEntry> Entries => entries;

    public async Task InitializeAsync(CancellationToken ct = default)
    {
        if (initialized) return;
        await gate.WaitAsync(ct);
        try
        {
            if (initialized) return;
            Directory.CreateDirectory(directory);
            await using var fileLock = AcquireLock();
            entries.Clear();
            foreach (var name in Directory.EnumerateFiles(directory, "*.json*").Select(Path.GetFileName)
                .Where(name => name is not null && (name.Length == 7 || name.Length == 11))
                .Select(name => name![..2]).Where(IsPrefix).Distinct(StringComparer.Ordinal))
            {
                var shard = await ReadShardAsync(name, ct);
                foreach (var pair in shard) entries.Add(pair.Key, pair.Value);
            }
            if (legacyDirectory is not null) await ImportLegacyLockedAsync(legacyDirectory, ct);
            initialized = true;
        }
        finally { gate.Release(); }
    }

    public async Task<int> ImportLegacyAsync(string sourceDirectory, CancellationToken ct = default)
    {
        await InitializeAsync(ct);
        await gate.WaitAsync(ct);
        try
        {
            await using var fileLock = AcquireLock();
            return await ImportLegacyLockedAsync(sourceDirectory, ct);
        }
        finally { gate.Release(); }
    }

    private async Task<int> ImportLegacyLockedAsync(string sourceDirectory, CancellationToken ct)
    {
        var markerPath = Path.Combine(directory, ".legacy-import.json");
        var marker = await LocalFiles.ReadRecoverableJsonAsync<LegacyImport>(markerPath, TranslationJson.TypeInfo<LegacyImport>(TranslationJson.Storage), ct,
            value => value.SchemaVersion == 1 && value.EntryCount >= 0);
        if (marker is not null) return marker.EntryCount;
        var imported = new Dictionary<string, MemoryEntry>(StringComparer.Ordinal);
        if (Directory.Exists(sourceDirectory))
            await Parallel.ForEachAsync(Directory.EnumerateFiles(sourceDirectory, "*.json"),
                new ParallelOptions { MaxDegreeOfParallelism = 8, CancellationToken = ct }, async (path, token) =>
            {
                var key = Path.GetFileNameWithoutExtension(path);
                if (!IsKey(key)) return;
                var entry = await LocalFiles.ReadJsonAsync<MemoryEntry>(path, TranslationJson.TypeInfo<MemoryEntry>(TranslationJson.Storage), token);
                if (entry is null || entry.Key != key || !IsValidEntry(entry))
                    throw new InvalidDataException("旧版翻译缓存内容校验失败。");
                lock (imported) imported[key] = entry;
            });
        // Existing game-local entries take precedence when a migration is resumed.
        foreach (var pair in entries) imported[pair.Key] = pair.Value;
        await StoreLockedAsync(imported.Values, ct);
        await LocalFiles.WriteRecoverableJsonAsync(markerPath, new LegacyImport(1, imported.Count), TranslationJson.TypeInfo<LegacyImport>(TranslationJson.Storage), ct);
        return imported.Count;
    }

    internal sealed record LegacyImport(int SchemaVersion, int EntryCount);

    public string Key(TranslationUnit unit, TranslationPolicy policy) =>
        Key(unit, policy, SingleWordEmphasisGroups(unit.SourceText).Count > 0);

    private static IReadOnlyList<string> SingleWordEmphasisGroups(string source) =>
        !source.Contains("[[", StringComparison.Ordinal) ? [] : TextValidation.TranslatableBracketGroups(source)
            .Where(group => group.StartsWith("[[", StringComparison.Ordinal) && group.EndsWith("]]", StringComparison.Ordinal)
                && group.Length > 4 && group[2..^2].All(char.IsAsciiLetter)).ToArray();

    private static string Key(TranslationUnit unit, TranslationPolicy policy, bool bracketEmphasis)
    {
        // Only terms present in this unit or its explicit context affect this entry.
        var terms = policy.Terminology.Where(t => unit.SourceText.Contains(t.Key, StringComparison.Ordinal) ||
            unit.Context.Contains(t.Key, StringComparison.Ordinal) || (unit.Speaker?.Contains(t.Key, StringComparison.Ordinal) ?? false))
            .OrderBy(t => t.Key, StringComparer.Ordinal).Select(t => new[] { t.Key, t.Value });
        return LocalFiles.HashText(JsonSerializer.Serialize(new JsonObject
        {
            ["SourceText"] = unit.SourceText,
            ["Context"] = unit.Context,
            ["Speaker"] = unit.Speaker,
            ["Kind"] = unit.Kind,
            ["TargetLanguage"] = policy.TargetLanguage,
            ["Style"] = policy.Style,
            ["Version"] = policy.Version + (bracketEmphasis ? "\nbracket-emphasis-1" : ""),
            ["Terms"] = TranslationJson.Node((terms).ToArray(), TranslationJson.Default)
        }, TranslationJson.TypeInfo<JsonObject>(TranslationJson.Default)) + TextValidation.ProtectionSignature(unit));
    }

    public async Task<string?> FindAsync(TranslationUnit unit, TranslationPolicy policy, CancellationToken ct = default)
    {
        await InitializeAsync(ct);
        var emphasis = SingleWordEmphasisGroups(unit.SourceText);
        var key = Key(unit, policy, emphasis.Count > 0);
        var legacyEmphasis = false;
        if (!entries.TryGetValue(key, out var entry))
        {
            if (emphasis.Count == 0) return null;
            key = Key(unit, policy, false);
            // Preserve compatible old translations. Revisit frozen English emphasis once,
            // using the corrected format instructions, without invalidating the whole cache.
            if (!entries.TryGetValue(key, out entry)) return null;
            legacyEmphasis = true;
        }
        if (entry.Key != key || entry.SourceText != unit.SourceText || entry.Context != unit.Context)
            throw new InvalidDataException("翻译缓存内容校验失败，请保留诊断后修复缓存。");
        return legacyEmphasis && emphasis.Any(group => entry.Translation.Contains(group, StringComparison.Ordinal)) ? null : entry.Translation;
    }

    public Task StoreAsync(TranslationUnit unit, TranslationPolicy policy, string translation,
        string provider, CancellationToken ct = default)
    {
        var key = Key(unit, policy);
        return StoreBatchAsync([new MemoryEntry(key, unit.SourceText, unit.Context, translation, provider, DateTimeOffset.UtcNow)], ct);
    }

    public async Task StoreBatchAsync(IEnumerable<MemoryEntry> batch, CancellationToken ct = default)
    {
        var pending = batch.ToArray();
        if (pending.Length == 0) return;
        if (pending.Any(entry => !IsValidEntry(entry))) throw new InvalidDataException("翻译缓存内容校验失败。");
        await InitializeAsync(ct);
        await gate.WaitAsync(ct);
        try
        {
            await using var fileLock = AcquireLock();
            await StoreLockedAsync(pending, ct);
        }
        finally { gate.Release(); }
    }

    private async Task StoreLockedAsync(IEnumerable<MemoryEntry> batch, CancellationToken ct)
    {
        foreach (var group in batch.GroupBy(entry => entry.Key[..2], StringComparer.Ordinal))
        {
            var shard = await ReadShardAsync(group.Key, ct);
            var changed = false;
            foreach (var entry in group)
            {
                if (shard.GetValueOrDefault(entry.Key) == entry) continue;
                shard[entry.Key] = entry;
                changed = true;
            }
            if (changed)
                await LocalFiles.WriteRecoverableJsonAsync(Path.Combine(directory, group.Key + ".json"), new MemoryShard(2, shard), TranslationJson.TypeInfo<MemoryShard>(TranslationJson.Storage), ct);
            foreach (var pair in shard) entries[pair.Key] = pair.Value;
        }
    }

    private async Task<Dictionary<string, MemoryEntry>> ReadShardAsync(string prefix, CancellationToken ct)
    {
        var path = Path.Combine(directory, prefix + ".json");
        var shard = await LocalFiles.ReadRecoverableJsonAsync<MemoryShard>(path, TranslationJson.TypeInfo<MemoryShard>(TranslationJson.Storage), ct,
            value => value.SchemaVersion is 1 or 2 && value.Entries is not null && value.Entries.All(pair =>
                IsValidEntry(pair.Value) && pair.Key.StartsWith(prefix, StringComparison.Ordinal) && pair.Key == pair.Value.Key));
        if (shard?.SchemaVersion == 1)
            await LocalFiles.WriteJsonAsync(path, shard with { SchemaVersion = 2 }, TranslationJson.TypeInfo<MemoryShard>(TranslationJson.Storage), ct);
        return shard is null ? new(StringComparer.Ordinal) : new(shard.Entries, StringComparer.Ordinal);
    }

    private FileStream AcquireLock()
    {
        try { return new FileStream(Path.Combine(directory, ".memory.lock"), FileMode.OpenOrCreate, FileAccess.ReadWrite, FileShare.None); }
        catch (IOException error) { throw new IOException("翻译缓存正被其他任务使用，请稍后重试。", error); }
    }

    private static bool IsPrefix(string value) => value.Length == 2 && value.All(character => character is >= '0' and <= '9' or >= 'a' and <= 'f');
    private static bool IsKey(string? value) => value is { Length: 64 } && value.All(character => character is >= '0' and <= '9' or >= 'a' and <= 'f');
    private static bool IsValidEntry(MemoryEntry entry) => entry is not null && IsKey(entry.Key) &&
        entry.SourceText is not null && entry.Context is not null && entry.Translation is not null && entry.Provider is not null;
}
