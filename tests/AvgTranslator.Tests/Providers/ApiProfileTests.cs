using System.Text.Json;
using AvgTranslator.Core.Providers;
using AvgTranslator.Core.Storage;

namespace AvgTranslator.Tests.Providers;

public sealed class ApiProfileTests
{
    [Fact]
    public void OneProfileServesBothPurposesAndAddingAnotherDoesNotChangeRouting()
    {
        var shared = new ApiProfile("shared", "通用 API", "https://first.invalid/v1", "model-a");
        var analysis = new ApiProfile("analysis", "分析 API", "https://second.invalid/v1", "model-b", ProviderApiProtocol.Responses);
        var configuration = new ApiProfileConfiguration([]).Save(shared).Save(analysis);
        Assert.Equal(shared, configuration.Resolve(ApiPurpose.GameAnalysis));
        Assert.Equal(shared, configuration.Resolve(ApiPurpose.Translation));
        configuration = configuration.Select(ApiPurpose.GameAnalysis, analysis.Id);
        Assert.Equal(analysis, configuration.Resolve(ApiPurpose.GameAnalysis));
        Assert.Equal(shared, configuration.Resolve(ApiPurpose.Translation));
        using var client = new HttpClient();
        var analysisSettings = configuration.Resolve(ApiPurpose.GameAnalysis);
        var translationSettings = configuration.Resolve(ApiPurpose.Translation);
        var analysisProvider = new OpenAiCompatibleProvider(client, new ProviderSettings(analysisSettings.Endpoint, "", analysisSettings.Model, analysisSettings.Protocol));
        var translationProvider = new OpenAiCompatibleProvider(client, new ProviderSettings(translationSettings.Endpoint, "", translationSettings.Model, translationSettings.Protocol));
        Assert.NotEqual(analysisProvider.Identity, translationProvider.Identity);
    }

    [Fact]
    public void SavedSelectionsSurviveRenameAndReloadAndDeletingDoesNotFallBackToAnotherAccount()
    {
        var first = new ApiProfile("one", "通用 API", "https://first.invalid/v1", "model-a", EncryptedApiKey: "encrypted-key");
        var second = new ApiProfile("two", "分析 API", "https://second.invalid/v1", "model-b");
        var configuration = new ApiProfileConfiguration([]).Save(first).Save(second).Select(ApiPurpose.GameAnalysis, second.Id);
        var renamed = first with { Name = "剧情翻译", Model = "model-c" };
        configuration = configuration.Save(renamed);
        var persisted = JsonSerializer.Serialize(configuration, LocalFiles.JsonOptions);
        var loaded = JsonSerializer.Deserialize<ApiProfileConfiguration>(persisted, LocalFiles.JsonOptions)!;
        Assert.Equal(renamed, loaded.Resolve(ApiPurpose.Translation));
        Assert.Equal(second, loaded.Resolve(ApiPurpose.GameAnalysis));
        var removed = loaded.Remove(first.Id);
        Assert.Throws<InvalidOperationException>(() => removed.Resolve(ApiPurpose.Translation));
        Assert.Equal(second, removed.Resolve(ApiPurpose.GameAnalysis));
        Assert.Equal(renamed, loaded.Resolve(ApiPurpose.Translation)); // Existing snapshots keep their selected settings.
        Assert.Throws<ArgumentException>(() => loaded.Save(second with { Name = renamed.Name }));
    }
}
