using GameL10n.Core;
using GameL10n.Core.Storage;
using GameL10n.Core.Translation;
using System.Text.Json;

namespace GameL10n.Tests.Pipeline;

public sealed class TranslationMemoryTests : IDisposable
{
    private readonly string root = Path.Combine(Path.GetTempPath(), "gamel10n-memory-" + Guid.NewGuid().ToString("N"));
    private readonly TranslationPolicy policy = new("简体中文", "natural", new Dictionary<string, string>());

    [Fact]
    public async Task LegacyImportPreservesKeysAndIsNotScannedAgain()
    {
        var legacy = Path.Combine(root, "legacy");
        var destination = Path.Combine(root, "memory");
        var memory = new TranslationMemory(destination, legacy);
        var unit = new TranslationUnit("1", "story", "1", "hello", "scene");
        var key = memory.Key(unit, policy);
        await LocalFiles.WriteJsonAsync(Path.Combine(legacy, key + ".json"), new MemoryEntry(key, unit.SourceText,
            unit.Context, "你好", "test", DateTimeOffset.UtcNow));
        Assert.Equal("你好", await memory.FindAsync(unit, policy));
        Assert.True(File.Exists(Path.Combine(destination, key[..2] + ".json")));
        await File.WriteAllTextAsync(Path.Combine(legacy, key + ".json"), "damaged legacy data must never be read again");
        Assert.Equal("你好", await new TranslationMemory(destination, legacy).FindAsync(unit, policy));
    }

    [Fact]
    public async Task RefreshesOnlyOldFrozenEmphasisAndReusesCorrectTranslationsAcrossRestart()
    {
        var destination = Path.Combine(root, "memory");
        var memory = new TranslationMemory(destination);
        var frozen = new TranslationUnit("1", "story", "1", "Weapons [[also]] count as One-Handed.", "scene");
        var correct = new TranslationUnit("2", "story", "2", "Your Hitbox is [[smaller]].", "scene");
        var plain = new TranslationUnit("3", "story", "3", "Hello [name]", "scene");
        string OldKey(TranslationUnit unit) => LocalFiles.HashText(JsonSerializer.Serialize(new
        {
            unit.SourceText, unit.Context, unit.Speaker, unit.Kind,
            policy.TargetLanguage, policy.Style, policy.Version, Terms = Array.Empty<string[]>()
        }));
        await memory.StoreBatchAsync([
            new(OldKey(frozen), frozen.SourceText, frozen.Context, "武器[[also]]视为单手武器。", "old", DateTimeOffset.UtcNow),
            new(OldKey(correct), correct.SourceText, correct.Context, "你的受击范围[[更小]]。", "old", DateTimeOffset.UtcNow),
            new(OldKey(plain), plain.SourceText, plain.Context, "你好[name]", "old", DateTimeOffset.UtcNow)
        ]);
        Assert.Equal(OldKey(plain), memory.Key(plain, policy));
        Assert.Null(await memory.FindAsync(frozen, policy));
        Assert.Equal("你的受击范围[[更小]]。", await memory.FindAsync(correct, policy));
        Assert.Equal("你好[name]", await memory.FindAsync(plain, policy));
        await memory.StoreAsync(frozen, policy, "武器[[也]]视为单手武器。", "new");
        var reopened = new TranslationMemory(destination);
        Assert.Equal("武器[[也]]视为单手武器。", await reopened.FindAsync(frozen, policy));
        Assert.Equal("你的受击范围[[更小]]。", await reopened.FindAsync(correct, policy));

        // A new translation may intentionally preserve a term; it must not enter a retry loop.
        await reopened.StoreAsync(frozen, policy, "武器[[also]]视为单手武器。", "new");
        Assert.Equal("武器[[also]]视为单手武器。", await new TranslationMemory(destination).FindAsync(frozen, policy));
    }

    [Fact]
    public async Task DamagedOrMissingShardRecoversBackupAndNextWriteKeepsBackupValid()
    {
        var destination = Path.Combine(root, "memory");
        var memory = new TranslationMemory(destination);
        var unit = new TranslationUnit("1", "story", "1", "hello", "scene");
        var path = Path.Combine(destination, memory.Key(unit, policy)[..2] + ".json");
        await memory.StoreAsync(unit, policy, "你好", "test");
        await memory.StoreAsync(unit, policy, "您好", "test");
        await File.WriteAllTextAsync(path, "{");
        var recovered = new TranslationMemory(destination);
        Assert.Equal("你好", await recovered.FindAsync(unit, policy));
        await recovered.StoreAsync(unit, policy, "新译文", "test");
        File.Delete(path);
        Assert.Equal("你好", await new TranslationMemory(destination).FindAsync(unit, policy));
    }

    [Fact]
    public async Task ResourceCacheRejectsDamageAndAdapterVersionChanges()
    {
        var extraction = new ExtractionResult([
            new("1", "a", "1", "hello", "scene"), new("2", "b", "2", "goodbye", "scene"),
            new("3", "a", "3", "again", "scene")], ["warning"]);
        var store = new ExtractionStore(root);
        await store.StoreAsync("fingerprint", "adapter", "1", extraction);
        var loaded = await store.LoadAsync("fingerprint", "adapter", "1");
        Assert.Equal(extraction.Units, loaded!.Units);
        Assert.Null(await store.LoadAsync("fingerprint", "adapter", "2"));
        var resource = Directory.GetFiles(root, "*.json", SearchOption.AllDirectories).First(file => Path.GetFileName(file) != "manifest.json");
        await File.AppendAllTextAsync(resource, " ");
        Assert.Null(await store.LoadAsync("fingerprint", "adapter", "1"));
    }

    public void Dispose() { if (Directory.Exists(root)) Directory.Delete(root, true); }
}
