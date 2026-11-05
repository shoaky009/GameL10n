using System.Text.Json;
using GameL10n.Core.Development;
using GameL10n.Core.Providers;
using GameL10n.Core.Storage;

namespace GameL10n.Tests.Pipeline;

public sealed class GameProjectTests : IDisposable
{
    private readonly string root = Path.Combine(Path.GetTempPath(), "gamel10n-projects-" + Guid.NewGuid().ToString("N"));

    [Fact]
    public async Task InheritedDefaultsFollowChangesAfterSavingAndReloadingWithoutCopyingValues()
    {
        var first = new ApiProfile("first", "First", "https://first.invalid/v1", "m");
        var second = new ApiProfile("second", "Second", "https://second.invalid/v1", "m");
        var defaults = new ApiProfileConfiguration([]).Save(first).Save(second);
        var store = new GameProjectStore(root);
        var game = store.Create(Path.Combine(root, "inherited"));
        await store.SaveAsync(game);
        var settings = (await store.GetAsync(game.Id))!.Settings;
        var runningApi = settings.ResolveApiConfiguration(defaults);
        var runningLimits = settings.ResolveAdaptationLimits(new(100, 2_000_000));
        defaults = defaults.Select(ApiPurpose.GameAnalysis, second.Id).SetTranslationChain([second.Id, first.Id]);
        await store.SaveAsync(game with { Settings = settings with { Style = "edited" } });
        settings = (await new GameProjectStore(root).GetAsync(game.Id))!.Settings;

        Assert.Equal(second, settings.ResolveApiConfiguration(defaults).Resolve(ApiPurpose.GameAnalysis));
        Assert.Equal([second, first], settings.ResolveApiConfiguration(defaults).ResolveTranslationChain());
        Assert.Equal(new AdaptationLimits(400, 20_000_000), settings.ResolveAdaptationLimits(new(400, 20_000_000)));
        Assert.Equal(first, runningApi.Resolve(ApiPurpose.GameAnalysis));
        Assert.Equal(new AdaptationLimits(100, 2_000_000), runningLimits);
        using var json = JsonDocument.Parse(await File.ReadAllTextAsync(Path.Combine(store.GetProjectDirectory(game.Id), "game.json")));
        foreach (var field in new[] { "analysisProfileId", "translationProfileIds", "adaptationTokenBudget", "adaptationMaximumTurns" })
            Assert.False(json.RootElement.GetProperty("settings").TryGetProperty(field, out _));
    }

    [Fact]
    public void LegacyOverridesStayIndependentAndCanReturnToGlobalDefaultsByPurpose()
    {
        const string legacy = """
            {"analysisProfileId":"old","translationProfileIds":["old"],
             "adaptationTokenBudget":4000000,"adaptationMaximumTurns":120}
            """;
        var settings = JsonSerializer.Deserialize<GameTranslationSettings>(legacy, LocalFiles.JsonOptions)!;
        var first = new ApiProfile("old", "Old", "https://old.invalid/v1", "m");
        var second = new ApiProfile("new", "New", "https://new.invalid/v1", "m");
        var defaults = new ApiProfileConfiguration([first, second], second.Id, second.Id);
        Assert.Equal(first, settings.ResolveApiConfiguration(defaults).Resolve(ApiPurpose.GameAnalysis));
        Assert.Equal(new[] { first }, settings.ResolveApiConfiguration(defaults).ResolveTranslationChain());
        Assert.Equal(new AdaptationLimits(120, 4_000_000), settings.ResolveAdaptationLimits(new(400, 20_000_000)));

        settings = settings with { AnalysisProfileId = null };
        Assert.Equal(second, settings.ResolveApiConfiguration(defaults).Resolve(ApiPurpose.GameAnalysis));
        Assert.Equal(first, settings.ResolveApiConfiguration(defaults).Resolve(ApiPurpose.Translation));
        settings = settings with { TranslationProfileIds = null, AdaptationTokenBudget = null, AdaptationMaximumTurns = null };
        settings = JsonSerializer.Deserialize<GameTranslationSettings>(JsonSerializer.Serialize(settings, LocalFiles.JsonOptions), LocalFiles.JsonOptions)!;
        Assert.Equal(second, settings.ResolveApiConfiguration(defaults).Resolve(ApiPurpose.Translation));
        Assert.Equal(new AdaptationLimits(400, 20_000_000), settings.ResolveAdaptationLimits(new(400, 20_000_000)));
    }

    [Fact]
    public void ExplicitEmptyOrDeletedProjectApiDoesNotSilentlyFallBackToGlobalAccount()
    {
        var defaults = new ApiProfileConfiguration([]).Save(new("global", "Global", "https://global.invalid/v1", "m"));
        var settings = new GameTranslationSettings { AnalysisProfileId = "deleted", TranslationProfileIds = [] };
        var effective = settings.ResolveApiConfiguration(defaults);
        Assert.Throws<InvalidOperationException>(() => effective.Resolve(ApiPurpose.GameAnalysis));
        Assert.Throws<InvalidOperationException>(() => effective.ResolveTranslationChain());
        Assert.Throws<InvalidOperationException>(() => (settings with { AnalysisProfileId = "" })
            .ResolveApiConfiguration(defaults).Resolve(ApiPurpose.GameAnalysis));
        var inherited = new GameTranslationSettings().ResolveApiConfiguration(new([]));
        Assert.Throws<InvalidOperationException>(() => inherited.Resolve(ApiPurpose.GameAnalysis));
        Assert.Throws<InvalidOperationException>(() => inherited.ResolveTranslationChain());
    }

    [Fact]
    public async Task ProjectSettingsStayIndependentAndDamagedPrimaryRecovers()
    {
        var store = new GameProjectStore(root);
        var first = store.Create(Path.Combine(root, "install-a"), new() { Style = "first", TranslationProfileIds = ["api-a"] });
        var second = store.Create(Path.Combine(root, "install-b"), new() { Style = "second", TranslationProfileIds = ["api-b", "fallback"] });
        await store.SaveAsync(first);
        await store.SaveAsync(second);
        await store.SaveAsync(first with { Settings = first.Settings with { Style = "edited" } });
        Assert.False(File.Exists(Path.Combine(store.GetProjectDirectory(first.Id), "game.json.bak")));
        // Recovery remains compatible with a backup written by an older app.
        await LocalFiles.WriteJsonAsync(Path.Combine(store.GetProjectDirectory(first.Id), "game.json.bak"), first, FixtureJson.TypeInfo<global::GameL10n.Core.Storage.GameProject>(FixtureJson.Storage));
        await File.WriteAllTextAsync(Path.Combine(store.GetProjectDirectory(first.Id), "game.json"), "{");
        Assert.Equal("first", (await store.GetAsync(first.Id))!.Settings.Style);
        Assert.Equal("second", (await store.GetAsync(second.Id))!.Settings.Style);
        Assert.Equal(["api-b", "fallback"], (await store.GetAsync(second.Id))!.Settings.TranslationProfileIds);
        Assert.Equal(2, (await store.ListAsync()).Count);
    }

    [Fact]
    public async Task UnrecoverableProjectDoesNotBlockHealthyGames()
    {
        var store = new GameProjectStore(root);
        var first = store.Create(Path.Combine(root, "install-a"));
        var second = store.Create(Path.Combine(root, "install-b"));
        await store.SaveAsync(first);
        await store.SaveAsync(second);
        await File.WriteAllTextAsync(Path.Combine(store.GetProjectDirectory(first.Id), "game.json"), "{");
        var damaged = new List<string>();
        var games = await store.ListAsync(onError: (id, _) => damaged.Add(id));
        Assert.Equal(second.Id, Assert.Single(games).Id);
        Assert.Equal(first.Id, Assert.Single(damaged));
    }

    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public async Task SummariesRebuildOnlyFromGameMetadataWithoutCopyingTranslationSettings(bool corruptIndex)
    {
        var store = new GameProjectStore(root);
        var project = store.Create(Path.Combine(root, "listed"), new()
        {
            Style = "project-only-style", Terminology = "project-only-terminology",
            AnalysisProfileId = "analysis", TranslationProfileIds = ["translation", "fallback"]
        }) with { TaskSummary = new("翻译已暂停", true, 12, 30) };
        var removed = store.Create(Path.Combine(root, "removed")) with { IsRemoved = true };
        var orphan = store.Create(Path.Combine(root, "legacy-task-only"));
        await store.SaveAsync(project);
        await store.SaveAsync(removed);
        await LocalFiles.WriteJsonAsync(Path.Combine(store.GetProjectDirectory(orphan.Id), "job.json"),
            new { orphan.GameDirectory }, FixtureJson.TypeInfo<object>(FixtureJson.Storage));
        await File.WriteAllTextAsync(Path.Combine(store.GetProjectDirectory(project.Id), "job.json"), "{");
        await File.WriteAllTextAsync(Path.Combine(root, "settings.json"), "{");
        var indexPath = Path.Combine(root, "games", "index.json");
        if (corruptIndex) await File.WriteAllTextAsync(indexPath, "{");
        else File.Delete(indexPath);

        var errors = new List<string>();
        var summary = Assert.Single(await new GameProjectStore(root).ListSummariesAsync(onError: (id, _) => errors.Add(id)));

        Assert.Empty(errors);
        Assert.Equal(project.Id, summary.Id);
        Assert.Equal(project.TaskSummary, summary.TaskSummary);
        Assert.Equal("analysis", summary.AnalysisProfileId);
        Assert.Equal(["translation", "fallback"], summary.TranslationProfileIds);
        Assert.Null(await store.GetAsync(orphan.Id));
        Assert.Equal("project-only-style", (await store.GetAsync(project.Id))!.Settings.Style);
        var index = await File.ReadAllTextAsync(indexPath);
        Assert.DoesNotContain("project-only-style", index);
        Assert.DoesNotContain("project-only-terminology", index);
        Assert.DoesNotContain("\"settings\"", index);
        Assert.False(File.Exists(indexPath + ".bak"));
    }

    [Fact]
    public async Task SummariesReadOneIndexAndConcurrentStoresKeepSavedChanges()
    {
        var firstStore = new GameProjectStore(root);
        var secondStore = new GameProjectStore(root);
        var first = firstStore.Create(Path.Combine(root, "first"));
        var second = secondStore.Create(Path.Combine(root, "second"));
        await Task.WhenAll(firstStore.SaveAsync(first), secondStore.SaveAsync(second));
        await File.WriteAllTextAsync(Path.Combine(firstStore.GetProjectDirectory(first.Id), "game.json"), "{");

        Assert.Equal(2, (await new GameProjectStore(root).ListSummariesAsync()).Count);
        first = first with
        {
            Name = "renamed", Settings = first.Settings with { AnalysisProfileId = "new-api" },
            TaskSummary = new("翻译完成", false, 30, 30)
        };
        await secondStore.SaveAsync(first);

        var summaries = await firstStore.ListSummariesAsync();
        Assert.Equal(2, summaries.Count);
        var updated = Assert.Single(summaries, item => item.Id == first.Id);
        Assert.Equal("renamed", updated.Name);
        Assert.Equal("new-api", updated.AnalysisProfileId);
        Assert.Equal(first.TaskSummary, updated.TaskSummary);
    }

    [Fact]
    public async Task RemovingWithoutDeletingPreservesDataAndCanBeAddedAgain()
    {
        var store = new GameProjectStore(root);
        var project = store.Create(Path.Combine(root, "original"), new() { Style = "preserved" });
        await store.SaveAsync(project);
        var memory = Path.Combine(store.GetProjectDirectory(project.Id), "saved-translations.json");
        await File.WriteAllTextAsync(memory, "已完成译文");
        await store.RemoveAsync(project.Id, deleteData: false);
        Assert.Empty(await new GameProjectStore(root).ListAsync());
        var retained = (await store.GetAsync(project.Id))!;
        Assert.True(retained.IsRemoved);
        Assert.Equal("preserved", retained.Settings.Style);
        Assert.Equal("已完成译文", await File.ReadAllTextAsync(memory));
        await store.SaveAsync(retained with { IsRemoved = false });
        Assert.Equal(project.Id, Assert.Single(await store.ListAsync()).Id);
    }

    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public async Task MissingProjectDirectoryCanBeRemovedFromIndexWithoutRecreatingFiles(bool deleteData)
    {
        var store = new GameProjectStore(root);
        var project = store.Create(Path.Combine(root, "installed"));
        Directory.CreateDirectory(project.GameDirectory);
        var original = Path.Combine(project.GameDirectory, "game.dat");
        await File.WriteAllTextAsync(original, "original");
        await store.SaveAsync(project);
        Directory.Delete(store.GetProjectDirectory(project.Id), recursive: true);
        Assert.Single(await store.ListSummariesAsync());

        await store.RemoveAsync(project.Id, deleteData);

        Assert.Empty(await store.ListSummariesAsync());
        Assert.False(Directory.Exists(store.GetProjectDirectory(project.Id)));
        Assert.Equal("original", await File.ReadAllTextAsync(original));
    }

    [Fact]
    public async Task DeletingProjectDoesNotDeleteInstalledGameOrOtherProjects()
    {
        var store = new GameProjectStore(root);
        var game = Path.Combine(root, "original");
        Directory.CreateDirectory(game);
        var original = Path.Combine(game, "game.dat");
        await File.WriteAllTextAsync(original, "original bytes");
        var project = store.Create(game);
        var other = store.Create(Path.Combine(root, "other"));
        await store.SaveAsync(project);
        await store.SaveAsync(other);
        await store.RemoveAsync(project.Id, deleteData: true);
        Assert.False(Directory.Exists(store.GetProjectDirectory(project.Id)));
        Assert.Equal("original bytes", await File.ReadAllTextAsync(original));
        Assert.Equal(other.Id, Assert.Single(await store.ListAsync()).Id);
        await Assert.ThrowsAsync<InvalidDataException>(() => store.RemoveAsync("../original", true));
    }

    [Fact]
    public async Task InterruptedDeletionKeepsProjectVisibleForRetry()
    {
        if (!OperatingSystem.IsWindows()) return;
        var store = new GameProjectStore(root);
        var project = store.Create(Path.Combine(root, "original"));
        await store.SaveAsync(project);
        var lockedFile = Path.Combine(store.GetProjectDirectory(project.Id), "locked.dat");
        await using (var locked = new FileStream(lockedFile, FileMode.Create, FileAccess.Write, FileShare.None))
        {
            await Assert.ThrowsAsync<IOException>(() => store.RemoveAsync(project.Id, true));
            Assert.Equal(project.Id, Assert.Single(await store.ListAsync()).Id);
        }
        await store.RemoveAsync(project.Id, true);
        Assert.Empty(await store.ListAsync());
    }

    public void Dispose() { if (Directory.Exists(root)) Directory.Delete(root, true); }
}
