using System.Text.Json;
using AvgTranslator.Core.Storage;

namespace AvgTranslator.Core.Translation;

public sealed record TranslationPolicy(string TargetLanguage, string Style,
    IReadOnlyDictionary<string, string> Terminology, string Version = "1");

public sealed record MemoryEntry(string Key, string SourceText, string Context, string Translation,
    string Provider, DateTimeOffset CreatedAt);

public sealed class TranslationMemory(string directory)
{
    public string Key(TranslationUnit unit, TranslationPolicy policy)
    {
        // Only terms present in this unit or its explicit context affect this entry.
        var terms = policy.Terminology.Where(t => unit.SourceText.Contains(t.Key, StringComparison.Ordinal) ||
            unit.Context.Contains(t.Key, StringComparison.Ordinal) || (unit.Speaker?.Contains(t.Key, StringComparison.Ordinal) ?? false))
            .OrderBy(t => t.Key, StringComparer.Ordinal).Select(t => new[] { t.Key, t.Value });
        return LocalFiles.HashText(JsonSerializer.Serialize(new
        {
            unit.SourceText, unit.Context, unit.Speaker, unit.Kind,
            policy.TargetLanguage, policy.Style, policy.Version, Terms = terms
        }));
    }

    public async Task<string?> FindAsync(TranslationUnit unit, TranslationPolicy policy, CancellationToken ct = default)
    {
        var key = Key(unit, policy);
        var entry = await LocalFiles.ReadJsonAsync<MemoryEntry>(Path.Combine(directory, key + ".json"), ct);
        if (entry is null) return null;
        if (entry.Key != key || entry.SourceText != unit.SourceText || entry.Context != unit.Context)
            throw new InvalidDataException("翻译缓存内容校验失败，请保留诊断后修复缓存。");
        return entry.Translation;
    }

    public Task StoreAsync(TranslationUnit unit, TranslationPolicy policy, string translation,
        string provider, CancellationToken ct = default)
    {
        var key = Key(unit, policy);
        return LocalFiles.WriteJsonAsync(Path.Combine(directory, key + ".json"),
            new MemoryEntry(key, unit.SourceText, unit.Context, translation, provider, DateTimeOffset.UtcNow), ct);
    }
}
