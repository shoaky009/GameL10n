using System.Text.Json.Serialization;

namespace AvgTranslator.Core.Providers;

public enum ApiPurpose { GameAnalysis, Translation }

public sealed record ApiProfile(string Id, string Name, string Endpoint, string Model,
    ProviderApiProtocol Protocol = ProviderApiProtocol.ChatCompletions, string? EncryptedApiKey = null,
    bool RequiresSessionKey = false,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? AnalysisReasoningEffort = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingDefault)] TranslationInputFormat TranslationFormat = TranslationInputFormat.JsonBatch)
{
    [JsonIgnore]
    public string Description => $"{(Protocol == ProviderApiProtocol.Responses ? "Responses" : "Chat Completions")} · {Model}"
        + (TranslationFormat == TranslationInputFormat.PlainText ? " · 逐条纯文本" : "");
    public override string ToString() => Name;
}

public sealed record ApiProfileConfiguration(IReadOnlyList<ApiProfile> Profiles,
    string? AnalysisProfileId = null, string? TranslationProfileId = null,
    IReadOnlyList<string>? TranslationProfileIds = null)
{
    // Settings saved before chains were introduced retain their single translation API.
    public IReadOnlyList<string> GetTranslationProfileIds() =>
        TranslationProfileIds ?? (TranslationProfileId is { } id ? [id] : []);

    public IReadOnlyList<ApiProfile> ResolveTranslationChain()
    {
        var ids = GetTranslationProfileIds();
        if (ids.Count == 0)
            throw new InvalidOperationException("请配置全局默认翻译 API 链，或在“项目设置”中单独配置。");
        if (ids.Distinct(StringComparer.Ordinal).Count() != ids.Count)
            throw new InvalidOperationException("翻译 API 链不能重复使用同一配置。");
        return ids.Select(id => Profiles.FirstOrDefault(profile => profile.Id == id)
            ?? throw new InvalidOperationException("翻译 API 链中的配置已不存在，请重新配置。")).ToArray();
    }

    public ApiProfile Resolve(ApiPurpose purpose) => purpose switch
    {
        ApiPurpose.GameAnalysis => Profiles.FirstOrDefault(profile => profile.Id == AnalysisProfileId)
            ?? throw new InvalidOperationException("请配置全局默认分析 API，或在“项目设置”中单独选择。"),
        ApiPurpose.Translation => ResolveTranslationChain()[0],
        _ => throw new ArgumentOutOfRangeException(nameof(purpose))
    };

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
        return Profiles.Count == 0 ? new(profiles, profile.Id, profile.Id, [profile.Id]) : this with { Profiles = profiles };
    }

    public ApiProfileConfiguration SetTranslationChain(IEnumerable<string> profileIds)
    {
        var ids = profileIds.ToArray();
        if (ids.Distinct(StringComparer.Ordinal).Count() != ids.Length)
            throw new ArgumentException("翻译 API 链不能重复使用同一配置。", nameof(profileIds));
        if (ids.Any(id => !Profiles.Any(profile => profile.Id == id)))
            throw new ArgumentException("选择的 API 配置已不存在，请重新选择。", nameof(profileIds));
        return this with { TranslationProfileIds = ids, TranslationProfileId = ids.FirstOrDefault() };
    }

    public ApiProfileConfiguration Select(ApiPurpose purpose, string? profileId)
    {
        if (profileId is not null && !Profiles.Any(profile => profile.Id == profileId))
            throw new ArgumentException("选择的 API 配置已不存在，请重新选择。");
        return purpose switch
        {
            ApiPurpose.GameAnalysis => this with { AnalysisProfileId = profileId },
            ApiPurpose.Translation => SetTranslationChain(profileId is null ? [] : [profileId]),
            _ => throw new ArgumentOutOfRangeException(nameof(purpose))
        };
    }

    public ApiProfileConfiguration Remove(string id) => SetTranslationChain(GetTranslationProfileIds().Where(selected => selected != id)) with
    {
        Profiles = Profiles.Where(profile => profile.Id != id).ToArray(),
        AnalysisProfileId = AnalysisProfileId == id ? null : AnalysisProfileId
    };
}
