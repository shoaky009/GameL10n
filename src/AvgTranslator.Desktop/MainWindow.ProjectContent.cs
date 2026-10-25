using AvgTranslator.Core.Development;
using AvgTranslator.Core.Pipeline;
using AvgTranslator.Core.Storage;

namespace AvgTranslator.Desktop;

public sealed partial class MainWindow
{
    // An estimate of retained managed text/row data, not the process working set. Active and unsaved projects are pinned.
    private const long ProjectContentCacheLimit = 256L * 1024 * 1024;

    private async Task SaveNavigationSettingsAsync()
    {
        await settingsWriteLock.WaitAsync();
        try
        {
            if (!navigationSettingsDirty) return;
            await LocalFiles.WriteJsonAsync(Path.Combine(dataDirectory, "settings.json"), globalSettings);
            navigationSettingsDirty = false;
        }
        finally { settingsWriteLock.Release(); }
    }

    private async Task OpenGameAsync(GameWorkspace game)
    {
        if (game.ContentLoad is { } pending) { await pending; return; }
        if (!game.HasSavedAnalysis || game.Session is not null || game.Busy || closing || changingDataDirectory
            || game.IsBeingRemoved || game.ContentState == ProjectContentState.Unavailable || !games.Contains(game)) return;
        var cancellation = new CancellationTokenSource();
        var token = cancellation.Token;
        var storagePipeline = pipeline;
        var projectStore = projects;
        var store = CreateAdapterStore(game);
        var toolkit = new EngineReferenceToolkit(Path.Combine(store.RootDirectory, "engine-tools"));
        game.ContentLoadCancellation = cancellation;
        game.ContentState = ProjectContentState.Loading;
        game.ContentError = null;
        if (selectedGame == game) RenderSelectedGame();
        game.ContentLoad = LoadAsync();
        try { await game.ContentLoad; }
        finally { game.ContentLoad = null; TrimProjectContentCache(); }

        async Task LoadAsync()
        {
            string? diagnostic = null;
            try
            {
                var loaded = await Task.Run(() => LoadSavedSessionAsync(game, store, toolkit,
                    (_, message) => diagnostic = message, storagePipeline, token), token);
                token.ThrowIfCancellationRequested();
                if (!games.Contains(game) || closing || changingDataDirectory || game.IsBeingRemoved) return;
                if (loaded is null)
                    throw new InvalidDataException(diagnostic ?? "没有可读取的完整提取结果，请点击重新识别。已保存译文仍会保留。");
                game.Session = loaded;
                game.ContentState = ProjectContentState.Loaded;
                // Old projects acquire their small summary once; reading content never starts a task or changes its outcome.
                await RememberAnalysisAsync(game, loaded, token, projectStore);
            }
            catch (OperationCanceledException) when (token.IsCancellationRequested)
            { game.ContentState = game.Session is null ? ProjectContentState.NotLoaded : ProjectContentState.Loaded; }
            catch (Exception error)
            {
                Program.WriteStartupError(error);
                if (game.Session is null)
                {
                    game.ContentState = ProjectContentState.Unavailable;
                    game.ContentError = "已有结果未能加载：" + error.Message + " 请主动重新识别，或检查项目文件后重新打开程序。";
                }
                else game.ContentError = "文本已加载，但项目摘要未能保存：" + error.Message;
            }
            finally
            {
                game.ContentLoadCancellation = null;
                cancellation.Dispose();
                if (selectedGame == game && !closing && !changingDataDirectory && !game.IsBeingRemoved) RenderSelectedGame();
                else UpdateControls();
            }
        }
    }

    private async Task RememberAnalysisAsync(GameWorkspace game, GameSession loaded, CancellationToken token,
        GameProjectStore? projectStore = null)
    {
        var fingerprint = Path.GetFileName(loaded.SourceDirectory);
        var summary = game.Project.Analysis;
        if (summary is null || summary.AdapterId != loaded.Adapter.Id || summary.AdapterVersion != loaded.Adapter.Version
            || summary.SourceFingerprint != fingerprint || summary.UnitCount != loaded.Extraction.Units.Count)
        {
            summary = new(loaded.Adapter.Id, loaded.Adapter.Version, fingerprint, loaded.Extraction.Units.Count,
                loaded.Extraction.Units.Select(unit => unit.ResourcePath).Distinct(StringComparer.Ordinal).Count(),
                loaded.Probe.EngineId, DateTimeOffset.UtcNow);
            var project = game.Project with { AdapterId = loaded.Adapter.Id, AdapterVersion = loaded.Adapter.Version, Analysis = summary };
            game.Project = project;
            var destination = projectStore ?? projects;
            await game.EnqueueWrite(() => destination.SaveAsync(project, token));
        }
        game.HasSavedAnalysis = true;
        RefreshProjectCacheSize(game);
    }

    private void RefreshProjectCacheSize(GameWorkspace game)
    {
        long size = 0;
        foreach (var unit in game.Session?.Extraction.Units ?? [])
            size += 256L + 2L * (unit.Id.Length + unit.SourceText.Length + unit.Context.Length + unit.ResourcePath.Length
                + unit.Locator.Length + (unit.Speaker?.Length ?? 0));
        foreach (var row in game.Rows ?? [])
            size += 800L + 2L * ((row.ModelText?.Length ?? 0) + (row.Edit?.Text.Length ?? 0) + (row.PreviousEdit?.Text.Length ?? 0));
        game.CachedContentBytes = size;
    }

    private void TrimProjectContentCache()
    {
        var total = games.Sum(game => game.CachedContentBytes);
        if (total <= ProjectContentCacheLimit) return;
        foreach (var game in games.Where(game => game != selectedGame && !game.Busy && game.ContentLoad is null
            && game.LoadingRows is null && !game.EditsDirty && !game.SelectionDirty && game.EditSaveError is null)
            .OrderBy(game => game.LastViewed))
        {
            total -= game.CachedContentBytes;
            ForgetWorkbenchCache(game);
            game.Session = null;
            game.Rows = null;
            game.RowsSession = null;
            game.RowsById.Clear();
            game.Edits.Clear();
            game.Candidates.Clear();
            game.PatchTextHashes = null;
            game.PatchIndexDirectory = null;
            game.CachedContentBytes = 0;
            if (game.ContentState == ProjectContentState.Loaded) game.ContentState = ProjectContentState.NotLoaded;
            if (total <= ProjectContentCacheLimit) break;
        }
    }
}
