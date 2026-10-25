using System.Text.Json;
using AvgTranslator.Core.Providers;
using AvgTranslator.Core.Storage;

namespace AvgTranslator.Tests.Providers;

public sealed class ApiProfileTests
{
    [Fact]
    public void PlainTextFormatPersistsAndOlderProfilesKeepJsonBatch()
    {
        var profile = new ApiProfile("local", "本地翻译", "http://localhost:8080/v1", "local",
            TranslationFormat: TranslationInputFormat.PlainText);
        var loaded = JsonSerializer.Deserialize<ApiProfile>(JsonSerializer.Serialize(profile, LocalFiles.JsonOptions), LocalFiles.JsonOptions)!;
        Assert.Equal(profile, loaded);
        var old = JsonSerializer.Deserialize<ApiProfile>("""{"id":"old","name":"old","endpoint":"http://localhost:8080/v1","model":"local"}""", LocalFiles.JsonOptions)!;
        Assert.Equal(TranslationInputFormat.JsonBatch, old.TranslationFormat);
    }

    [Fact]
    public void TranslationChainPreservesOrderAcrossEditsReloadAndRemoval()
    {
        var first = new ApiProfile("one", "First", "https://first.invalid/v1", "model-a");
        var second = new ApiProfile("two", "Second", "https://second.invalid/v1", "model-b", ProviderApiProtocol.Responses);
        var third = new ApiProfile("three", "Third", "https://third.invalid/v1", "model-c");
        var configuration = new ApiProfileConfiguration([]).Save(first).Save(second).Save(third)
            .SetTranslationChain([second.Id, first.Id]);
        var renamed = second with { Name = "Renamed" };
        configuration = configuration.Save(renamed);
        var loaded = JsonSerializer.Deserialize<ApiProfileConfiguration>(
            JsonSerializer.Serialize(configuration, LocalFiles.JsonOptions), LocalFiles.JsonOptions)!;
        Assert.Equal(new[] { renamed, first }, loaded.ResolveTranslationChain());
        Assert.Equal(first, loaded.Resolve(ApiPurpose.GameAnalysis));
        Assert.Equal(renamed, loaded.Resolve(ApiPurpose.Translation));
        Assert.Equal(new[] { first }, loaded.Remove(second.Id).ResolveTranslationChain());
        Assert.Equal(new[] { second.Id, first.Id }, loaded.GetTranslationProfileIds());
        Assert.Throws<InvalidOperationException>(() => loaded.Remove(second.Id).Remove(first.Id).ResolveTranslationChain());
        Assert.Throws<ArgumentException>(() => loaded.SetTranslationChain([first.Id, first.Id]));
        Assert.Throws<ArgumentException>(() => loaded.SetTranslationChain(["missing"]));
    }

    [Fact]
    public void LegacySelectionBecomesOneEntryAndExplicitEmptyChainStaysEmpty()
    {
        const string legacy = """
            {"profiles":[{"id":"one","name":"First","endpoint":"https://first.invalid/v1","model":"m"}],
             "analysisProfileId":"one","translationProfileId":"one"}
            """;
        var loaded = JsonSerializer.Deserialize<ApiProfileConfiguration>(legacy, LocalFiles.JsonOptions)!;
        Assert.Equal("one", Assert.Single(loaded.ResolveTranslationChain()).Id);
        var empty = loaded.SetTranslationChain([]);
        var reloaded = JsonSerializer.Deserialize<ApiProfileConfiguration>(
            JsonSerializer.Serialize(empty, LocalFiles.JsonOptions), LocalFiles.JsonOptions)!;
        Assert.Throws<InvalidOperationException>(() => reloaded.ResolveTranslationChain());
        Assert.Equal("one", reloaded.Resolve(ApiPurpose.GameAnalysis).Id);
        Assert.Throws<InvalidOperationException>(() => (loaded with { TranslationProfileIds = ["missing"] }).ResolveTranslationChain());
    }

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
