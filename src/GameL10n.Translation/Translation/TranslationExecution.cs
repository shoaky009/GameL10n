using GameL10n.Core.Providers;

namespace GameL10n.Core.Translation;

public sealed record TranslationOptions(int BatchSize = 24, bool RetryUncertainRequests = true,
    IReadOnlyList<string>? UnitIds = null)
{
    public IProgress<IReadOnlyDictionary<string, string>>? TranslationUpdates { get; init; }
    public IProgress<IReadOnlyDictionary<string, string?>>? FailureUpdates { get; init; }
    public bool RequireAllTranslations { get; init; }
    public bool CandidateOnly { get; init; }
    public string? RetranslationId { get; init; }
}

public sealed record TranslationProgress(string Stage, int Completed, int Total, int CacheHits,
    TokenUsage Usage, string Message, int Failed = 0);

public sealed record TranslationRunResult(IReadOnlyDictionary<string, string> Translations,
    int CacheHits, TokenUsage Usage, TranslationRunSummary Summary);

public sealed record TranslationRunSummary(int Total, int Completed, int Failed);

public sealed record PendingTranslation(string Id, string MemoryKey);
public sealed record TranslationIntent(string RequestKey, string Provider, DateTimeOffset SentAt,
    IReadOnlyList<PendingTranslation> Units, bool CandidateOnly = false, string? RetranslationId = null);
public sealed record TranslationRefusal(TranslationIntent Request, DateTimeOffset RecordedAt, ProviderResponseDiagnostic? Diagnostic);

public sealed class UncertainRequestException(string message) : Exception(message);
public sealed class TranslationRetriesExhaustedException(TranslationProviderException error)
    : Exception("API 请求重试 5 次后仍失败，任务已暂停。已完成译文已保存，可继续相同范围的翻译。最后一次失败：" + error.FailureSummary + "。", error);

public sealed class TranslationRefusedException(bool alreadyRecorded = false)
    : Exception((alreadyRecorded ? "这批内容在翻译 API 链中的所有接口均已有拒绝记录，本次未再次调用 API。" : "翻译 API 链中的所有接口均拒绝处理这批内容（服务商内容限制），已停止尝试。") +
        "任务已暂停，已完成译文已保存。可调整翻译 API 链或处理被拒绝的内容后继续；拒绝说明保存在日志和任务记录中。");
