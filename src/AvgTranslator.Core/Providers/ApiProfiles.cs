using System.Text.Json.Serialization;

namespace AvgTranslator.Core.Providers;

public enum ApiPurpose { GameAnalysis, Translation }

public sealed record ApiProfile(string Id, string Name, string Endpoint, string Model,
    ProviderApiProtocol Protocol = ProviderApiProtocol.ChatCompletions, string? EncryptedApiKey = null,
    bool RequiresSessionKey = false)
{
    [JsonIgnore]
    public string Description => $"{(Protocol == ProviderApiProtocol.Responses ? "Responses" : "Chat Completions")} · {Model}";
    public override string ToString() => Name;
}

public sealed record ApiProfileConfiguration(IReadOnlyList<ApiProfile> Profiles,
    string? AnalysisProfileId = null, string? TranslationProfileId = null)
{
    public ApiProfile Resolve(ApiPurpose purpose)
    {
        var id = purpose switch
        {
            ApiPurpose.GameAnalysis => AnalysisProfileId,
            ApiPurpose.Translation => TranslationProfileId,
            _ => throw new ArgumentOutOfRangeException(nameof(purpose))
        };
        return Profiles.FirstOrDefault(profile => profile.Id == id)
            ?? throw new InvalidOperationException($"请在“API 与术语”中为{(purpose == ApiPurpose.GameAnalysis ? "游戏分析" : "文本翻译")}选择 API 配置。");
    }

    public ApiProfileConfiguration Save(ApiProfile profile)
    {
        if (string.IsNullOrWhiteSpace(profile.Id) || string.IsNullOrWhiteSpace(profile.Name) || profile.Name.Length > 80 || profile.Name.Any(char.IsControl))
            throw new ArgumentException("配置名称不能为空，且不能超过 80 个字符。");
        if (Profiles.Any(existing => existing.Id != profile.Id && existing.Name.Equals(profile.Name, StringComparison.OrdinalIgnoreCase)))
            throw new ArgumentException("已有同名 API 配置，请换一个名称。");
        var profiles = Profiles.Any(existing => existing.Id == profile.Id)
            ? Profiles.Select(existing => existing.Id == profile.Id ? profile : existing).ToArray()
            : [.. Profiles, profile];
        // A first configuration serves both purposes. Additional profiles do not switch either purpose.
        return Profiles.Count == 0 ? new(profiles, profile.Id, profile.Id) : this with { Profiles = profiles };
    }

    public ApiProfileConfiguration Select(ApiPurpose purpose, string? profileId)
    {
        if (profileId is not null && !Profiles.Any(profile => profile.Id == profileId))
            throw new ArgumentException("选择的 API 配置已不存在，请重新选择。");
        return purpose switch
        {
            ApiPurpose.GameAnalysis => this with { AnalysisProfileId = profileId },
            ApiPurpose.Translation => this with { TranslationProfileId = profileId },
            _ => throw new ArgumentOutOfRangeException(nameof(purpose))
        };
    }

    public ApiProfileConfiguration Remove(string id) => this with
    {
        Profiles = Profiles.Where(profile => profile.Id != id).ToArray(),
        AnalysisProfileId = AnalysisProfileId == id ? null : AnalysisProfileId,
        TranslationProfileId = TranslationProfileId == id ? null : TranslationProfileId
    };
}
