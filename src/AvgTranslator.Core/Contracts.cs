namespace AvgTranslator.Core;

public sealed record GameProbe(
    string EngineId, string AdapterId, string AdapterVersion, string Description,
    IReadOnlyList<string> RequiredFiles, IReadOnlyList<string> Warnings,
    string? OriginalExecutable = null);

public sealed record TranslationUnit(
    string Id, string ResourcePath, string Locator, string SourceText,
    string Context, string? Speaker = null, string Kind = "dialogue")
{
    [System.Text.Json.Serialization.JsonIgnore(Condition = System.Text.Json.Serialization.JsonIgnoreCondition.WhenWritingNull)]
    public IReadOnlyList<string>? ProtectedTokens { get; init; }
}

public sealed record ExtractionResult(
    IReadOnlyList<TranslationUnit> Units, IReadOnlyList<string> Warnings);

public sealed record AdapterBuildResult(
    IReadOnlyList<string> Files, IReadOnlyList<string> Warnings,
    string? LaunchExecutable = null);

public sealed record ValidationIssue(string Code, string Message, string? UnitId = null);

public sealed record ValidationResult(bool Passed, IReadOnlyList<ValidationIssue> Issues,
    string RuntimeStatus = "not-tested");

public interface IGameAdapter
{
    string Id { get; }
    string Version { get; }
    Adapters.AdapterCatalogEntry? CatalogEntry => null;
    Task<GameProbe?> ProbeAsync(string gameDirectory, CancellationToken cancellationToken = default);
    Task<ExtractionResult> ExtractAsync(string sourceDirectory, CancellationToken cancellationToken = default);
    Task<AdapterBuildResult> BuildPatchAsync(string sourceDirectory,
        IReadOnlyDictionary<string, string> translations, string patchDirectory,
        CancellationToken cancellationToken = default);
    Task<ValidationResult> ValidatePatchAsync(string sourceDirectory, string patchDirectory,
        CancellationToken cancellationToken = default);
    IReadOnlyList<ValidationIssue> ValidateTranslation(TranslationUnit unit, string text) => [];
}

public sealed record TranslationRequest(IReadOnlyList<TranslationUnit> Units, string TargetLanguage,
    IReadOnlyDictionary<string, string> Terminology, string Style)
{
    public IReadOnlyDictionary<string, IReadOnlyList<string>>? ValidationFeedback { get; init; }
}

public sealed record TokenUsage(long InputTokens, long OutputTokens, bool Estimated = false)
{
    public long TotalTokens => InputTokens + OutputTokens;
}

public sealed record TranslationResponse(IReadOnlyDictionary<string, string> Translations,
    TokenUsage Usage, string? ProviderRequestId = null);

public interface ITranslationProvider
{
    string Identity { get; }
    string? DisplayName => null;
    int MaximumBatchSize => 256;
    Task<TranslationResponse> TranslateAsync(TranslationRequest request,
        CancellationToken cancellationToken = default);
}
