using System.Text.Json.Serialization;
using GameL10n.Core.Providers;

namespace GameL10n.Desktop.Settings;

internal sealed record DesktopSettings
{
    public int SchemaVersion { get; init; } = 2;
    public string? LastGameId { get; init; }
    public int MaximumTranslations { get; init; } = 2;
    public string GameDirectory { get; init; } = "";
    public string TargetLanguage { get; init; } = "简体中文";
    public string Style { get; init; } = "自然流畅，保留人物语气、情绪和原文含义。";
    public string Terminology { get; init; } = "";
    public int BatchSize { get; init; } = 24;
    public long AdaptationTokenBudget { get; init; } = 1_000_000;
    public int AdaptationMaximumTurns { get; init; } = 60;
    public ApiProfileConfiguration? ApiConfiguration { get; init; }

    [JsonPropertyName("endpoint"), JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? LegacyEndpoint { get; init; }
    [JsonPropertyName("model"), JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? LegacyModel { get; init; }
    [JsonPropertyName("protocol"), JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public ProviderApiProtocol? LegacyProtocol { get; init; }

    public ApiProfileConfiguration ReadApiConfiguration()
    {
        if (ApiConfiguration is not null) return ApiConfiguration;
        if (string.IsNullOrWhiteSpace(LegacyEndpoint) && string.IsNullOrWhiteSpace(LegacyModel)) return new([]);
        var profile = new ApiProfile("default", "默认 API", LegacyEndpoint ?? "", LegacyModel ?? "",
            LegacyProtocol ?? ProviderApiProtocol.ChatCompletions);
        return new([profile], profile.Id, profile.Id);
    }
}
