using System.Collections.ObjectModel;
using System.Diagnostics;
using System.Text.Json;
using AvgTranslator.Core;
using AvgTranslator.Core.Adapters;
using AvgTranslator.Core.Development;
using AvgTranslator.Core.Pipeline;
using AvgTranslator.Core.Providers;
using AvgTranslator.Core.Storage;
using AvgTranslator.Desktop.Settings;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.Windows.Storage.Pickers;

namespace AvgTranslator.Desktop;

public sealed partial class MainWindow : Window
{
    private string dataDirectory;
    private TranslationPipeline pipeline;
    private GameProjectStore projects;
    private GameTaskCoordinator coordinator = new(2);
    private readonly HttpClient httpClient = new() { Timeout = TimeSpan.FromMinutes(3) };
    private readonly List<IGameAdapter> adapters = [new Sfa16AosV2Adapter(), .. VnTextPatchAdapters.CreateAll()];
    private readonly ObservableCollection<GameWorkspace> games = [];
    private GameWorkspace? selectedGame;
    private GameWorkspace? settingsGame;
    private GameSession? session => selectedGame?.Session;
    private bool busy => selectedGame?.Busy == true;
    private bool initialized;
    private bool switchingGames;
    private bool closing;
    private bool allowClose;
    private FileStream? applicationLock;
    private DesktopSettings globalSettings = new();
    private bool navigationSettingsDirty;
    private readonly SemaphoreSlim settingsWriteLock = new(1, 1);

    public MainWindow()
    {
        InitializeComponent();
        RootGrid.ActualThemeChanged += (_, _) => UpdateTitleBarTheme();
        UpdateTitleBarTheme();
        InitializeWorkbench();
        AppWindow.Resize(new Windows.Graphics.SizeInt32(1440, 960));
        dataDirectory = DataLocation.Resolve();
        pipeline = new TranslationPipeline(dataDirectory);
        projects = new GameProjectStore(dataDirectory);
        InitializeProjectList();
        RootGrid.Loaded += Loaded;
        RootGrid.SizeChanged += (_, _) => SidebarColumn.Width = new GridLength(RootGrid.ActualWidth < 1200 ? 190 : 240);
        AppWindow.Closing += async (_, args) =>
        {
            if (allowClose) return;
            args.Cancel = true;
            if (closing || changingDataDirectory || removingGames || changingAdapterLibrary || SavingEngineLibrary) return;
            closing = true;
            UpdateControls();
            FooterText.Text = "正在保存任务，请稍候…";
            try
            {
                foreach (var game in games) game.ContentLoadCancellation?.Cancel();
                await Task.WhenAll(games.Select(game => game.ContentLoad ?? Task.CompletedTask));
                await Task.WhenAll(games.Select(game => game.MetadataLoad ?? Task.CompletedTask));
                await FlushAllEditsAsync();
                if (initialized && globalPage is null) await SaveSettingsAsync();
                if (initialized) await SaveNavigationSettingsAsync();
                await settingsWriteLock.WaitAsync();
                settingsWriteLock.Release();
                foreach (var game in games) game.Cancellation?.Cancel();
                await Task.WhenAll(games.Select(game => game.RunningTask ?? Task.CompletedTask));
                await Task.WhenAll(games.Select(game => game.LoadingRows ?? Task.CompletedTask));
                await Task.WhenAll(games.Select(game => game.DurableWrites));
            }
            catch (Exception error)
            {
                Program.WriteStartupError(error); closing = false; UpdateControls();
                FooterText.Text = "保存未完成，窗口已保留，请重试：" + error.Message;
                return;
            }
            allowClose = true; Close();
        };
        Closed += (_, _) => { applicationLock?.Dispose(); httpClient.Dispose(); };
    }

    private void UpdateTitleBarTheme() => AppWindow.TitleBar.PreferredTheme = RootGrid.ActualTheme == ElementTheme.Dark
        ? Microsoft.UI.Windowing.TitleBarTheme.Dark : Microsoft.UI.Windowing.TitleBarTheme.Light;

    private async void Loaded(object sender, RoutedEventArgs e)
    {
        if (initialized) return;
        try
        {
            applicationLock = DataDirectoryTransfer.AcquireLock(dataDirectory);
            ApplyDataDirectory(await ReadDataDirectoryAsync(dataDirectory));
            initialized = true;
            RefreshApiProfiles();
            SetWindowActivity(null);
            GameList.SelectedItem = games.FirstOrDefault(game => game.Id == globalSettings.LastGameId) ?? games.FirstOrDefault();
            UpdateControls();
        }
        catch (Exception error)
        {
            Program.WriteStartupError(error);
            FooterText.Text = "无法打开数据目录：" + error.Message;
            GameInfoBar.Title = "无法打开数据目录";
            GameInfoBar.Message = FooterText.Text;
            GameInfoBar.Severity = InfoBarSeverity.Error;
            UpdateControls();
            SetWindowActivity(null);
        }
    }

    private async Task LoadSummaryAsync(GameWorkspace game, TranslationPipeline storagePipeline, Action<string> report)
    {
        game.HasSavedAnalysis = game.Project.Analysis is not null || File.Exists(Path.Combine(game.Directory, "source.json"));
        async Task<T?> ReadSummaryAsync<T>(string name)
        {
            try { return await LocalFiles.ReadRecoverableJsonAsync<T>(Path.Combine(game.Directory, name)); }
            catch (Exception error) when (error is IOException or InvalidDataException or JsonException or UnauthorizedAccessException)
            { Program.WriteStartupError(error); report("部分任务记录无法读取，已保存的译文仍可继续使用。"); return default; }
        }
        var state = await ReadSummaryAsync<SavedTaskState>("task-state.json");
        game.EngineReview = await ReadSummaryAsync<EngineReviewResult>("engine-review.json");
        game.HasPendingTranslation = await ReadSummaryAsync<TranslationScope>("translation-scope.json") is { Completed: false };
        if (state is not null && state.Kind != GameTaskKind.Loading)
        {
            game.Stage = state.Running ? "可继续" : state.Stage;
            game.Progress = state.Progress;
            game.Error = state.Error;
            game.Kind = state.Kind;
        }
        else game.Stage = game.Project.TaskSummary is { } savedTask ? savedTask.Running ? "可继续" : savedTask.Stage
            : game.HasSavedAnalysis ? "已提取，可继续处理" : "等待识别";
        var adaptationPath = Path.Combine(game.Directory, "adaptation", "current", "session.json");
        game.HasAdaptationSession = File.Exists(adaptationPath);
        if (game.HasAdaptationSession)
        {
            try
            {
                // Read only UI metadata; do not retain the model conversation for every loaded project.
                var adaptation = await LocalFiles.ReadJsonAsync<SavedAdaptationSummary>(adaptationPath);
                game.AdaptationUsage = adaptation?.Usage;
                game.AdaptationSummary = adaptation?.Summary;
                game.AdaptationProgressNote = adaptation?.ProgressNote;
                game.AdaptationLastBreakthrough = adaptation?.LastBreakthrough;
                game.AdaptationStatus = adaptation?.LastStatus ?? AdaptationStatus.Incomplete;
                game.AdaptationBlocker = adaptation?.Blocker;
                game.AdaptationInstruction = adaptation?.UserInstruction;
                game.AdaptationObjective = adaptation?.Objective;
                game.PendingTextCoverage = adaptation?.TextCoverage?.Pending ?? [];
                game.PendingRuntimeChecks = adaptation?.TextCoverage?.RuntimeChecks ?? [];
                game.AdaptationNextAction = string.IsNullOrWhiteSpace(adaptation?.NextAction)
                    ? AutomaticAdaptationService.NextActionFor(game.AdaptationStatus.Value, adaptation?.Blocker) : adaptation.NextAction;
            }
            catch (Exception error) when (error is IOException or JsonException or UnauthorizedAccessException)
            {
                Program.WriteStartupError(error);
                game.AdaptationStatus = AdaptationStatus.Incomplete;
                game.AdaptationNextAction = "适配会话无法读取。点击“继续适配”，可根据保留的源码和工作文件重新分析。";
            }
        }
        var job = await ReadSummaryAsync<JsonElement?>("job.json");
        if (job is { ValueKind: JsonValueKind.Object } element && element.TryGetProperty("progress", out var progress)
            && (state is null || state.Running || (element.TryGetProperty("updatedAt", out var timestamp)
                && timestamp.TryGetDateTimeOffset(out var updated) && updated > state.UpdatedAt)))
        {
            try { game.Progress = progress.Deserialize<PipelineProgress>(LocalFiles.JsonOptions); game.Stage = "可继续"; }
            catch (JsonException error) { Program.WriteStartupError(error); }
        }
        try { game.Result = await storagePipeline.LoadLatestResultAsync(game.Directory); }
        catch (Exception error) when (error is IOException or InvalidDataException or JsonException or UnauthorizedAccessException)
        { Program.WriteStartupError(error); report("上次补丁记录无法读取，可以使用已保存的译文重新生成。"); }
        if (state is null && game.Result is not null && game.Progress is null) game.Stage = "补丁已生成";
    }

    private GameTranslationSettings DefaultGameSettings() => DefaultGameSettings(globalSettings);

    private static GameTranslationSettings DefaultGameSettings(DesktopSettings settings) => new()
    {
        TargetLanguage = settings.TargetLanguage, Style = settings.Style,
        Terminology = settings.Terminology, BatchSize = settings.BatchSize
    };

    private async void GameList_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        UpdateControls();
        if (!initialized || updatingProjectList || switchingGames || changingDataDirectory || removingGames || MultiSelectGamesButton.IsChecked == true
            || GameList.SelectedItem is not GameWorkspace next || (next == selectedGame && globalPage is null && next.MetadataLoaded)) return;
        switchingGames = true;
        UpdateControls();
        try
        {
            if (globalPage is null && selectedGame is not null) await SaveSettingsAsync(persistNavigation: false);
            selectedGame = next;
            next.LastViewed = DateTimeOffset.UtcNow;
            if (globalSettings.LastGameId != next.Id)
            {
                globalSettings = globalSettings with { LastGameId = next.Id, GameDirectory = next.GameDirectory };
                navigationSettingsDirty = true;
            }
            if (next.MetadataLoaded) LoadGameSettings(next);
            ApiSettingsInfo.IsOpen = false;
            ShowPage(null);
            RenderSelectedGame();
            TrimProjectContentCache();
        }
        catch (Exception error) { ShowError(error); return; }
        finally { switchingGames = false; UpdateControls(); }
        await EnsureProjectMetadataAsync(next);
        if (selectedGame == next && globalPage is null && next.MetadataLoaded && next.Session is null && !next.Busy) await OpenGameAsync(next);
    }

    private void LoadGameSettings(GameWorkspace game)
    {
        var settings = game.Project.Settings;
        apiConfiguration = ResolveGameApi(game);
        LanguageBox.Text = settings.TargetLanguage;
        StyleBox.Text = settings.Style;
        TerminologyBox.Text = settings.Terminology;
        BatchSizeBox.Value = settings.BatchSize;
        RefreshProjectAdaptationSettings();
        RefreshApiProfiles();
        settingsGame = game;
    }

    private async void Browse_Click(object sender, RoutedEventArgs e)
    {
        if (!initialized || closing || changingDataDirectory || changingAdapterLibrary) return;
        try
        {
            var folder = await PickFolderAsync();
            if (folder is null) return;
            string path = Path.TrimEndingDirectorySeparator(Path.GetFullPath(folder));
            var existing = games.FirstOrDefault(game => game.GameDirectory.Equals(path, StringComparison.OrdinalIgnoreCase));
            if (existing is not null) { ProjectSearchBox.Text = ""; MultiSelectGamesButton.IsChecked = false; GameList.SelectedItem = existing; return; }
            SetWindowActivity("正在添加游戏项目…");
            var project = projects.Create(path, DefaultGameSettings());
            project = (await projects.GetAsync(project.Id) ?? project) with { IsRemoved = false, UpdatedAt = DateTimeOffset.UtcNow };
            await projects.SaveAsync(project);
            var workspace = new GameWorkspace(project, projects.GetProjectDirectory(project.Id));
            ProjectSearchBox.Text = "";
            games.Add(workspace);
            MultiSelectGamesButton.IsChecked = false;
            SetWindowActivity(null);
            GameList.SelectedItem = workspace;
        }
        catch (Exception error) { ShowError(error); }
        finally { if (!switchingGames) SetWindowActivity(null); }
    }

    private async Task<string?> PickFolderAsync()
    {
        var picker = new FolderPicker(AppWindow.Id);
        return (await picker.PickSingleFolderAsync())?.Path;
    }

    private async void SaveSettings_Click(object sender, RoutedEventArgs e)
    {
        try { await SaveSettingsAsync(); ShowApiStatus("当前项目设置已保存，后续任务使用新设置。"); }
        catch (Exception error) { ShowApiStatus(error.Message, true); }
    }

    private async Task SaveSettingsAsync(Func<GameTranslationSettings, GameTranslationSettings>? updateSettings = null,
        bool persistNavigation = true)
    {
        var game = selectedGame;
        if (game is null || !game.MetadataLoaded || game != settingsGame) return;
        await FlushEditsAsync(game);
        var language = LanguageBox.Text.Trim();
        var style = StyleBox.Text;
        var terminology = TerminologyBox.Text;
        var batchSize = double.IsFinite(BatchSizeBox.Value) ? (int)BatchSizeBox.Value : 24;
        var useGlobalLimits = UseGlobalAdaptationLimitsBox.IsChecked == true;
        if (!useGlobalLimits) ValidateAdaptationValues(ProjectTokenBudgetBox.Value, ProjectMaximumTurnsBox.Value);
        long? budget = useGlobalLimits ? null : (long)ProjectTokenBudgetBox.Value;
        int? turns = useGlobalLimits ? null : (int)ProjectMaximumTurnsBox.Value;
        await settingsWriteLock.WaitAsync();
        try
        {
            var settings = game.Project.Settings with
            {
                TargetLanguage = language, Style = style, Terminology = terminology, BatchSize = batchSize,
                AdaptationTokenBudget = budget, AdaptationMaximumTurns = turns
            };
            settings = updateSettings?.Invoke(settings) ?? settings;
            var nextGlobal = globalSettings with
            {
                LastGameId = game.Id,
                GameDirectory = game.GameDirectory
            };
            if (persistNavigation && (navigationSettingsDirty || nextGlobal != globalSettings))
            {
                await LocalFiles.WriteJsonAsync(Path.Combine(dataDirectory, "settings.json"), nextGlobal);
                globalSettings = nextGlobal;
                navigationSettingsDirty = false;
            }
            var previousProject = game.Project;
            if (settings == previousProject.Settings)
            {
                if (selectedGame == game) apiConfiguration = ResolveGameApi(game);
                return;
            }
            var project = previousProject with { Settings = settings, UpdatedAt = DateTimeOffset.UtcNow };
            // Background discovery may publish adapter metadata while the ordered write is pending.
            game.Project = project;
            try { await game.EnqueueWrite(() => projects.SaveAsync(project)); }
            catch
            {
                if (ReferenceEquals(game.Project, project)) game.Project = previousProject;
                throw;
            }
            if (selectedGame == game) apiConfiguration = ResolveGameApi(game);
        }
        finally { settingsWriteLock.Release(); }
    }

    private async void SaveApplicationSettings_Click(object sender, RoutedEventArgs e)
    {
        if (changingDataDirectory || closing) return;
        try
        {
            await settingsWriteLock.WaitAsync();
            try
            {
                globalSettings = globalSettings with { MaximumTranslations = double.IsFinite(ConcurrentGamesBox.Value) ? (int)Math.Clamp(ConcurrentGamesBox.Value, 1, 8) : 2 };
                await LocalFiles.WriteJsonAsync(Path.Combine(dataDirectory, "settings.json"), globalSettings);
            }
            finally { settingsWriteLock.Release(); }
            ShowDataStatus("并行翻译数量已保存，下次启动时生效。", InfoBarSeverity.Success);
        }
        catch (Exception error) { ShowDataStatus(error.Message, InfoBarSeverity.Error); }
    }

    private void OpenGameData_Click(object sender, RoutedEventArgs e) => OpenDirectory(selectedGame?.Directory);
    private void OpenLogs_Click(object sender, RoutedEventArgs e) => OpenDirectory(selectedGame is { } game ? Path.Combine(game.Directory, "logs") : Path.Combine(dataDirectory, "logs"));
    private void OpenPatch_Click(object sender, RoutedEventArgs e) => OpenDirectory(selectedGame?.Result?.PatchDirectory);
    private static void OpenDirectory(string? path)
    {
        if (path is null) return;
        Directory.CreateDirectory(path);
        Process.Start(new ProcessStartInfo(path) { UseShellExecute = true });
    }

    private void UpdateControls()
    {
        var canNavigate = initialized && !closing && !switchingGames && !changingDataDirectory && !changingAdapterLibrary && !savingApiProfiles && pendingApiSelections == 0 && !removingGames;
        BrowseButton.IsEnabled = GameList.IsEnabled = GlobalNavigation.IsEnabled = ProjectSearchBox.IsEnabled = canNavigate;
        var canUseProject = canNavigate && selectedGame?.MetadataLoaded == true;
        MultiSelectGamesButton.IsEnabled = canNavigate && games.Count > 0;
        RemoveGamesButton.IsEnabled = canNavigate && GameList.SelectedItems.Count > 0;
        RemoveGamesButton.Content = GameList.SelectedItems.Count > 1 ? $"移除 ({GameList.SelectedItems.Count})" : "移除";
        ChangeDataDirectoryButton.IsEnabled = canNavigate && !SavingEngineLibrary;
        SaveApplicationSettingsButton.IsEnabled = ConcurrentGamesBox.IsEnabled = canNavigate;
        DefaultTokenBudgetBox.IsEnabled = DefaultMaximumTurnsBox.IsEnabled = SaveGlobalLimitsButton.IsEnabled = canNavigate;
        ProjectTabs.IsEnabled = canUseProject;
        AnalyzeButton.IsEnabled = canUseProject && !busy;
        AnalyzeButton.Content = selectedGame?.Project.AdapterId is not null ? "重新识别" : "常规识别";
        TranslateButton.IsEnabled = TrialButton.IsEnabled = canUseProject && !busy && session is not null;
        CancelButton.IsEnabled = busy && !closing;
        AdaptButton.IsEnabled = canUseProject && !busy;
        EngineReviewButton.IsEnabled = canUseProject && !busy
            && (selectedGame?.HasAdaptationSession == true || selectedGame?.Project.AdapterId is not null);
        AdaptButton.Content = selectedGame is { HasAdaptationSession: true } || selectedGame?.Project.AdapterId is not null
            ? "继续智能适配" : "智能识别与适配";
        InstallButton.IsEnabled = canUseProject && selectedGame?.Result is not null && !busy;
        OpenPatchButton.IsEnabled = selectedGame?.Result is not null;
        RestoreButton.IsEnabled = canUseProject && !busy;
        OpenGameDataButton.IsEnabled = canNavigate && selectedGame is not null;
        UpdateProjectListStatus();
        UpdateWorkbenchControls();
    }

    private void RenderSelectedGame()
    {
        var game = selectedGame;
        RenderEngineReview(game);
        GameNameText.Text = game?.Name ?? "选择或添加游戏";
        GamePathBox.Text = game?.GameDirectory ?? "";
        StageText.Text = game?.Stage ?? "等待导入";
        TaskMessageText.Text = game?.MetadataLoad is not null ? "正在读取项目设置与上次任务状态，可继续切换项目。"
            : game?.ContentLoading == true && !game.Busy ? "正在读取上次保存的文本结果，可继续切换项目。" : game?.StageMessage ?? "";
        TaskMessageText.Visibility = string.IsNullOrWhiteSpace(TaskMessageText.Text) ? Visibility.Collapsed : Visibility.Visible;
        TaskErrorInfo.IsOpen = game?.ContentError is not null || game?.Error is not null;
        TaskErrorInfo.Title = game?.ContentError is not null ? game.MetadataLoaded ? "已有结果未能完整加载" : "项目记录无法读取" : "任务需要处理";
        TaskErrorInfo.Message = game?.ContentError ?? game?.Error ?? "";
        ProjectActivityPanel.Visibility = game?.Busy == true ? Visibility.Visible : Visibility.Collapsed;
        ProjectActivityRing.IsActive = game?.Busy == true;
        ProjectActivityTitle.Text = game?.Stage ?? "";
        ProjectActivityMessage.Text = string.IsNullOrWhiteSpace(game?.StageMessage)
            ? "任务正在后台运行，可以切换到其他项目。" : game.StageMessage;
        CountsText.Text = game?.Progress is { } p ? $"本次成功 {p.Completed:N0} / {p.Total:N0} 条" + (p.Failed > 0 ? $" · 失败 {p.Failed:N0} 条" : "") + $" · 复用 {p.CacheHits:N0} 条 · {p.TranslationTokens:N0} Token"
            : game?.Project.Analysis is { } count ? $"共 {count.UnitCount:N0} 条文本" : session is { } s ? $"共 {s.Extraction.Units.Count:N0} 条文本" : "—";
        AdaptationPanel.Visibility = game?.HasAdaptationSession == true || game?.AdaptationUsage is not null ? Visibility.Visible : Visibility.Collapsed;
        AdaptationUsageText.Text = game?.AdaptationUsage is { } usage
            ? $"智能适配累计 Token：{usage.TotalTokens:N0}" + (usage.Estimated ? "（含估算）" : "") : "已保留适配工作文件。";
        AdaptationSummaryText.Text = game?.AdaptationSummary ?? "可以继续已有适配任务。";
        var progressNote = game?.AdaptationProgressNote;
        AdaptationProgressTitle.Text = progressNote is null ? "适配进展" : "适配进展 · " + progressNote.Title;
        AdaptationProgressText.Text = progressNote?.Message ?? "暂无进展说明。有关键变化时，模型可在这里更新。";
        AdaptationProgressTimeText.Text = progressNote is null ? ""
            : $"模型说明 · 最近更新 {progressNote.UpdatedAt.ToLocalTime():MM-dd HH:mm:ss}";
        var breakthrough = game?.AdaptationLastBreakthrough;
        AdaptationBreakthroughText.Visibility = breakthrough is not null && breakthrough != progressNote ? Visibility.Visible : Visibility.Collapsed;
        AdaptationBreakthroughText.Text = breakthrough is null ? ""
            : $"最近突破（{breakthrough.UpdatedAt.ToLocalTime():MM-dd HH:mm}）：{breakthrough.Message}";
        AdaptationBlockerText.Visibility = !busy && game?.AdaptationBlocker is not null ? Visibility.Visible : Visibility.Collapsed;
        AdaptationBlockerText.Text = game?.AdaptationBlocker?.Reason ?? "";
        TaskProgress.IsIndeterminate = game?.Busy == true && (game.Progress is null || game.Kind != GameTaskKind.Translation);
        TaskProgress.Value = game?.Progress is { Total: > 0 } progress ? 100.0 * Math.Min(progress.Total, progress.Completed + progress.Failed) / progress.Total : 0;
        var hasTranslationFailures = game?.Progress is { Failed: > 0 };
        GameInfoBar.Severity = game?.Error is not null ? InfoBarSeverity.Error : hasTranslationFailures ? InfoBarSeverity.Warning : session is null ? InfoBarSeverity.Informational : InfoBarSeverity.Success;
        GameInfoBar.Title = game?.Error is not null ? "任务需要处理" : hasTranslationFailures ? "有翻译失败的文本" : session?.Probe.Description ?? "识别游戏后可选择文本试译";
        GameInfoBar.Message = game?.Error ?? (hasTranslationFailures ? "已保存成功译文。可筛选“翻译失败”查看原因，再继续任务或人工校对。" : session is null ? "可以切换游戏，后台任务会继续运行。" : $"共 {session.Extraction.Units.Count:N0} 条文本。");
        if (game is { Busy: true, Error: null })
        {
            GameInfoBar.Severity = InfoBarSeverity.Informational;
            GameInfoBar.Title = game.Stage;
            GameInfoBar.Message = ProjectActivityMessage.Text;
        }
        if (game is { Busy: false, HasAdaptationSession: true, Session: null, AdaptationStatus: { } adaptationStatus })
        {
            GameInfoBar.Title = AdaptationStage(adaptationStatus);
            GameInfoBar.Message = game.AdaptationNextAction ?? AutomaticAdaptationService.NextActionFor(adaptationStatus);
            GameInfoBar.Severity = adaptationStatus is AdaptationStatus.Failed or AdaptationStatus.Unavailable ? InfoBarSeverity.Error
                : adaptationStatus is AdaptationStatus.WaitingForUser or AdaptationStatus.Unsupported ? InfoBarSeverity.Warning : InfoBarSeverity.Informational;
        }
        UpdateLog(game?.Log.ToString() ?? "");
        if (game is { Busy: false, Error: null, Session: not null, PendingTextCoverage.Count: > 0 })
        {
            GameInfoBar.Severity = InfoBarSeverity.Informational;
            GameInfoBar.Title = "已支持部分文字";
            GameInfoBar.Message = $"已提取 {game.Session.Extraction.Units.Count:N0} 条，可继续翻译；还有 {game.PendingTextCoverage.Count} 项文字覆盖待办，详见识别说明。";
        }
        PatchInstallInfo.IsOpen = game is { Kind: GameTaskKind.Installation } && (game.Busy || game.RunId is not null);
        PatchInstallInfo.Severity = game?.Error is not null ? InfoBarSeverity.Error : game?.Busy == true ? InfoBarSeverity.Informational : InfoBarSeverity.Success;
        PatchInstallInfo.Message = game?.Error ?? (game?.Kind == GameTaskKind.Installation ? game.Stage : "");
        PatchPathBox.Text = game?.Result?.PatchDirectory ?? "";
        InstallTargetText.Text = "安装位置：" + game?.GameDirectory;
        if (game?.Result is { } result) DisplayResult(result);
        else
        {
            PatchScopeText.Text = "尚未生成补丁。可先选择少量文本试译。";
            LaunchText.Text = "启动方式将在生成补丁后显示。";
            ValidationText.Text = "结构校验：未执行　游戏运行验证：未执行";
        }
        RecognitionDetailsText.Text = game?.Project.Analysis is { } analysis
            ? $"引擎：{analysis.EngineId}\n适配器：{analysis.AdapterId} · {analysis.AdapterVersion}\n已提取 {analysis.UnitCount:N0} 条文本 · {analysis.ResourceCount:N0} 个脚本 / 资源"
            : game?.HasSavedAnalysis == true ? "已有成功提取的文本记录。首次打开时读取保存的结果。"
            : "请选择“常规识别”匹配已有适配器，或选择“智能识别与适配”。";
        RecognitionWarningsText.Text = session is { } recognized && recognized.Extraction.Warnings.Count > 0
            ? string.Join("\n\n", recognized.Extraction.Warnings.Distinct()) : "暂无额外限制说明。生成补丁后仍需进入游戏验证显示效果。";
        if (game?.PendingTextCoverage.Count > 0)
            RecognitionWarningsText.Text += "\n\n尚未覆盖的文字：\n" + string.Join("\n", game.PendingTextCoverage);
        if (game?.PendingRuntimeChecks.Count > 0)
            RecognitionWarningsText.Text += "\n\n进入游戏后需验证：\n" + string.Join("\n", game.PendingRuntimeChecks);
        RenderWorkbenchSurface();
        UpdateControls();
        if (game?.Session is not null && (ReferenceEquals(ProjectTabs.SelectedItem, WorkbenchTab) || ReferenceEquals(ProjectTabs.SelectedItem, PatchTab)))
            LoadVisibleWorkbench(game);
    }

    private async void LoadVisibleWorkbench(GameWorkspace game)
    {
        try { await EnsureWorkbenchAsync(game); }
        catch (Exception error)
        {
            if (selectedGame == game) { WorkbenchEmptyText.Text = "译文加载失败：" + error.Message; WorkbenchEmptyText.Visibility = Visibility.Visible; }
            Program.WriteStartupError(error);
        }
    }

    private void DisplayResult(PipelineResult value)
    {
        PatchScopeText.Text = value.IsTrial ? $"试译补丁 · 已选 {value.TranslatedUnits ?? 0:N0} / 共 {value.TotalUnits:N0} 条，其余保留原文。" : $"全量汉化补丁 · 共 {value.TotalUnits:N0} 条文本。";
        InstallButton.Content = value.IsTrial ? "安装试译补丁" : "安装汉化补丁";
        PatchPathBox.Text = value.PatchDirectory;
        LaunchText.Text = value.LaunchExecutable is null ? "按原游戏方式启动。" : "安装后启动：" + value.LaunchExecutable;
        ValidationText.Text = $"结构校验：{(value.Validation.Passed ? "通过" : "未通过")}　游戏运行验证：{(value.Validation.RuntimeStatus == "passed" ? "通过" : "未执行")}";
    }

    private void SetStage(GameWorkspace game, string stage, string? message = null)
    {
        game.Stage = stage;
        game.StageMessage = message ?? "";
        game.Notify();
        if (game == selectedGame) RenderSelectedGame();
        else UpdateControls();
    }

    private void ShowError(Exception error)
    {
        if (selectedGame is { } game) { game.Error = error.Message; SetStage(game, "需要处理"); Log(game, error.Message); }
        else { GameInfoBar.Severity = InfoBarSeverity.Error; GameInfoBar.Title = "操作未完成"; GameInfoBar.Message = error.Message; }
    }

    private void Log(string message) { if (selectedGame is { } game) Log(game, message); }

    private void UpdateLog(string text)
    {
        if (LogBox.Text == text) return;
        LogBox.Text = text;
        DispatcherQueue.TryEnqueue(Microsoft.UI.Dispatching.DispatcherQueuePriority.Low, () =>
        {
            LogScrollViewer.UpdateLayout();
            LogScrollViewer.ChangeView(null, LogScrollViewer.ScrollableHeight, null, disableAnimation: true);
        });
    }

    private void LogScrollViewer_SizeChanged(object sender, SizeChangedEventArgs e) =>
        LogScrollViewer.ChangeView(null, LogScrollViewer.ScrollableHeight, null, disableAnimation: true);

    private void Log(GameWorkspace game, string message)
    {
        string line = $"[{DateTime.Now:HH:mm:ss}] {message}\n";
        game.Log.Append(line);
        if (game.Log.Length > 40_000) game.Log.Remove(0, game.Log.Length - 30_000);
        if (game == selectedGame) UpdateLog(game.Log.ToString());
        string path = Path.Combine(game.Directory, "logs", $"{DateTime.Now:yyyyMMdd}.log");
        _ = game.EnqueueWrite(async () =>
        {
            Directory.CreateDirectory(Path.GetDirectoryName(path)!);
            await File.AppendAllTextAsync(path, line);
        });
    }

    private sealed record SavedTaskState(string RunId, GameTaskKind Kind, string Stage, bool Running,
        PipelineProgress? Progress, string? Error, GameTranslationSettings Settings, DateTimeOffset UpdatedAt);

    private sealed record SavedAdaptationSummary(TokenUsage? Usage, string? Summary, AdaptationStatus? LastStatus,
        string? NextAction, AdaptationBlocker? Blocker, string? UserInstruction,
        AdaptationProgressNote? ProgressNote = null, AdaptationProgressNote? LastBreakthrough = null,
        TextCoverage? TextCoverage = null, AdaptationObjective? Objective = null);
}
