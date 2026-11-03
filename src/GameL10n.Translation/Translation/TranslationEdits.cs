using System.Text.Json;
using System.Text.Json.Serialization;
using GameL10n.Core.Storage;

namespace GameL10n.Core.Translation;

// Project-local corrections and selected retranslations override shared model drafts.
// Entries are bound to the source/context and language to avoid applying to changed text.
public sealed record TranslationEdit(string SourceSignature, string Language, string Text,
    bool Reviewed, DateTimeOffset UpdatedAt,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? RetranslationId = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingDefault)] bool IsGenerated = false);

public sealed record TranslationEdits(int SchemaVersion, IReadOnlyDictionary<string, TranslationEdit> Entries,
    IReadOnlyDictionary<string, TranslationEdit>? Candidates = null)
{
    public static string Signature(TranslationUnit unit) => LocalFiles.HashText(JsonSerializer.Serialize(
        new { unit.SourceText, unit.Context, unit.Speaker, unit.Kind }) + TextValidation.ProtectionSignature(unit));

    public TranslationEdit? Find(TranslationUnit unit, string language) =>
        Entries.TryGetValue(unit.Id, out var entry) && entry.Language == language
            && entry.SourceSignature == Signature(unit) ? entry : null;

    public static async Task<TranslationEdits> LoadAsync(string projectDirectory, CancellationToken ct = default)
    {
        var path = Path.Combine(projectDirectory, "translation-edits.json");
        if (!File.Exists(path)) return new(1, new Dictionary<string, TranslationEdit>(StringComparer.Ordinal));
        // Atomic replacement can proceed while a background task reads the previous snapshot.
        await using var stream = new FileStream(path, FileMode.Open, FileAccess.Read,
            FileShare.ReadWrite | FileShare.Delete, 65536, FileOptions.Asynchronous);
        var value = await JsonSerializer.DeserializeAsync<TranslationEdits>(stream, LocalFiles.JsonOptions, ct);
        if (value?.SchemaVersion != 1 || value.Entries is null || value.Entries.Concat(value.Candidates ?? new Dictionary<string, TranslationEdit>()).Any(pair =>
            string.IsNullOrWhiteSpace(pair.Key) || pair.Value is null || pair.Value.Text is null
            || pair.Value.SourceSignature is not { Length: 64 } || string.IsNullOrWhiteSpace(pair.Value.Language)))
            throw new InvalidDataException("校对记录无法读取，请保留 translation-edits.json 后检查文件。");
        return value;
    }

    public Task SaveAsync(string projectDirectory, CancellationToken ct = default) =>
        LocalFiles.WriteJsonAsync(Path.Combine(projectDirectory, "translation-edits.json"), this, ct);
}
