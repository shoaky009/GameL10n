namespace AvgTranslator.Core;

public sealed record TranslationUnit(
    string Id, string ResourcePath, string Locator, string SourceText,
    string Context, string? Speaker = null, string Kind = "dialogue")
{
    [System.Text.Json.Serialization.JsonIgnore(Condition = System.Text.Json.Serialization.JsonIgnoreCondition.WhenWritingNull)]
    public IReadOnlyList<string>? ProtectedTokens { get; init; }
}

public sealed record ValidationIssue(string Code, string Message, string? UnitId = null);

public sealed record ValidationResult(bool Passed, IReadOnlyList<ValidationIssue> Issues,
    string RuntimeStatus = "not-tested");

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
