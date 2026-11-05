using System.Diagnostics;
using System.Text.Json;
using GameL10n.Core;
using GameL10n.Core.Adapters;
using GameL10n.Core.Development;
using GameL10n.Core.Isolation;
using GameL10n.Core.Patching;
using GameL10n.Core.Pipeline;
using GameL10n.Core.Providers;
using GameL10n.Core.Storage;
using GameL10n.Core.Translation;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;

namespace GameL10n.Desktop;

public sealed partial class MainWindow
{
    private async void Analyze_Click(object sender, RoutedEventArgs e)
    {
        if (selectedGame is { } game) await AnalyzeAsync(game);
    }

    private Task AnalyzeAsync(GameWorkspace game) => StartWorkAsync(game, GameTaskKind.Analysis, "识别游戏资源", async token =>
    {
        var watch = Stopwatch.StartNew();
        var store = CreateAdapterStore(game);
        Action<string, string> report = (stage, message) => game.ReportProgress?.Invoke(stage, message);
        var engineToolkit = new EngineReferenceToolkit(Path.Combine(store.RootDirectory, "engine-tools"), reportProgress: report);
        await FlushEditsAsync(game);
        game.BuiltinProbes.Clear();
        game.ReuseFailures = [];
        IProgress<string> messages = new Progress<string>(message => Log(game, message));
        var runId = game.RunId;
        var request = new AdapterDiscoveryRequest(game.Id, game.GameDirectory, adapters,
            PinnedAdapterId: game.Project.AdapterId, PinnedAdapterVersion: game.Project.AdapterVersion);
        var discovery = new AdapterDiscoveryService(store, FindToolchain, new AdapterProcessRunner(), engineToolkit);
        var analysis = await Task.Run(() => new GameAnalysisService(pipeline, discovery).AnalyzeAsync(request, token,
            report, messages.Report, detail => LogDiscoveryDiagnostic(game, runId, detail)), token);
        game.BuiltinProbes.AddRange(analysis.Discovery.BuiltinProbes);
        game.ReuseFailures = analysis.Discovery.Failures;
        var nextSession = analysis.Session;
        if (nextSession is null)
        {
            SetStage(game, "未找到可用适配器", game.HasSavedAnalysis ? "上一次成功提取的结果仍可使用。" : "可选择智能识别与适配继续分析。");
            Log(game, "常规识别未匹配，可以使用“智能识别与适配”分析此游戏。" + (game.HasSavedAnalysis ? "上一次提取结果已保留。" : ""));
        }
        else
        {
            ForgetWorkbenchCache(game);
            game.Rows = null; game.RowsById.Clear(); game.RowsSession = null; game.Candidates.Clear();
            game.Session = nextSession;
            game.HasSavedAnalysis = true;
            game.ContentState = ProjectContentState.Loaded;
            game.ContentError = null;
            await CompleteAnalysisAsync(game, watch, restored: false, token);
        }
    });

    private static async Task LogDiscoveryDiagnostic(GameWorkspace game, string? runId, object detail)
    {
        var line = $"[{DateTime.Now:HH:mm:ss.fff}] [识别诊断] "
            + JsonSerializer.Serialize(new { schemaVersion = 1, runId, at = DateTimeOffset.UtcNow, entry = detail }) + "\n";
        var path = Path.Combine(game.Directory, "logs", $"{DateTime.Now:yyyyMMdd}.log");
        try
        {
            await game.EnqueueWrite(async () =>
            {
                Directory.CreateDirectory(Path.GetDirectoryName(path)!);
                await File.AppendAllTextAsync(path, line);
            });
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException) { Debug.WriteLine(error); }
    }

    private async Task<GameSession?> LoadSavedSessionAsync(GameWorkspace game, LocalAdapterPackageStore store,
        EngineReferenceToolkit toolkit, Action<string, string> report, TranslationPipeline storagePipeline, CancellationToken token)
    {
        SourceSnapshot? snapshot;
        try { snapshot = await LocalFiles.ReadRecoverableJsonAsync<SourceSnapshot>(Path.Combine(game.Directory, "source.json"), token); }
        catch (Exception error) when (error is System.Text.Json.JsonException or InvalidDataException)
        { report("读取已有结果", "保存的识别记录无法读取，请点击重新识别。已保存译文仍会保留。"); return null; }
        if (snapshot is null) return null;
        report("加载已保存文本", "正在加载上次使用的适配器和文本记录。");
        IGameAdapter? adapter = adapters.Where(item => item is not VnTextPatchAdapter)
            .Concat(VnTextPatchAdapters.CreateAll()).Append(new SiglusAdapter(toolkit, reportProgress: report))
            .FirstOrDefault(item => item.Id == snapshot.AdapterId && item.Version == snapshot.AdapterVersion);
        if (adapter is null)
        {
            if (!Directory.Exists(store.GetVersionDirectory(snapshot.AdapterId, snapshot.AdapterVersion)))
                adapter = new UnavailableGameAdapter(snapshot.AdapterId, snapshot.AdapterVersion);
            else
            {
                var package = store.LoadVersion(snapshot.AdapterId, snapshot.AdapterVersion);
                adapter = new GeneratedGameAdapter(package, FindToolchain(), new AdapterProcessRunner(), store.WorkDirectory, toolkit, report);
            }
        }
        return await storagePipeline.LoadSessionAsync(game.GameDirectory, adapter, token, game.Id, report);
    }

    private async Task<IReadOnlyList<TranslationUnit>?> LoadAdaptationBaselineAsync(GameWorkspace game, CancellationToken token)
    {
        if (game.Session is { } loaded) return loaded.Extraction.Units;
        SetStage(game, "读取已有文本", "正在读取已保存的文本身份，用于保留译文和校对记录。");
        var snapshot = await LocalFiles.ReadRecoverableJsonAsync<SourceSnapshot>(Path.Combine(game.Directory, "source.json"), token);
        if (snapshot is null) return null;
        var fingerprint = LocalFiles.HashText(JsonSerializer.Serialize(snapshot.Files.OrderBy(file => file.RelativePath)));
        var extraction = await Task.Run(() => new ExtractionStore(game.Directory)
            .LoadAsync(fingerprint, snapshot.AdapterId, snapshot.AdapterVersion, token), token);
        if (extraction is null)
            throw new InvalidDataException("已有文本记录不完整，请先重新识别再修复适配，以保留译文与校对记录的匹配关系。已保存译文未改动。");
        return extraction.Units;
    }

    private async Task CompleteAnalysisAsync(GameWorkspace game, Stopwatch watch, bool restored, CancellationToken token)
    {
        var loaded = game.Session!;
        await RememberAnalysisAsync(game, loaded, token);
        SetStage(game, "加载项目结果", "文本已就绪，正在读取补丁和任务记录。");
        if (!restored)
            foreach (var warning in loaded.Extraction.Warnings.Distinct(StringComparer.Ordinal)) Log(game, warning);
        game.Result = await pipeline.LoadLatestResultAsync(game.Directory, token);
        if (game.Result?.SourceFingerprint != Path.GetFileName(loaded.SourceDirectory)) game.Result = null;
        SetStage(game, game.Progress is null ? "可以开始翻译" : "可以继续翻译");
        Log(game, $"{(restored ? "已恢复" : "已加载")} {loaded.Extraction.Units.Count:N0} 条文本，用时 {watch.Elapsed.TotalSeconds:F1} 秒。"
            + (restored ? "游戏资源有更新时请点击“重新识别”。" : ""));
    }

    private async void Translate_Click(object sender, RoutedEventArgs e)
    {
        if (selectedGame is not { } game) return;
        try
        {
            var scope = await LocalFiles.ReadJsonAsync<TranslationScope>(Path.Combine(game.Directory, "translation-scope.json"));
            var resumeWithoutPatch = scope is { Completed: false, BuildPatchAfter: true }
                && game.Session?.Adapter is UnavailableGameAdapter;
            if (scope is { Completed: false, UnitIds: { } ids })
            {
                var choice = await new ContentDialog
                {
                    XamlRoot = RootGrid.XamlRoot, Title = "继续上次翻译",
                    Content = (scope.RetranslationId is not null
                        ? $"上次重新翻译选择了 {ids.Count:N0} 条文本。仅继续本次尚未成功的文本，已完成结果不会重复请求。"
                        : $"上次任务选择了 {ids.Count:N0} 条文本。已保存译文会继续复用。")
                        + (resumeWithoutPatch ? "\n引擎适配器版本不可用，本次继续翻译原选区，暂不生成补丁。" : ""),
                    PrimaryButtonText = "继续原选区", SecondaryButtonText = "改为全量翻译", CloseButtonText = "取消"
                }.ShowAsync();
                if (choice == ContentDialogResult.None || selectedGame != game) return;
                if (choice == ContentDialogResult.Secondary) scope = null;
            }
            if (resumeWithoutPatch && scope is not null)
                Log(game, "引擎适配器版本不可用，将继续原选区的翻译并保留进度，本次不生成补丁。");
            await TranslateAsync(scope is { Completed: false } ? scope.UnitIds : null,
                scope is { Completed: false, BuildPatchAfter: true } && !resumeWithoutPatch,
                scope is { Completed: false } ? scope.RetranslationId : null);
        }
        catch (Exception error) { ShowError(error); }
    }

    private async Task TranslateAsync(IReadOnlyList<string>? trialUnitIds = null, bool buildPatchAfter = false, string? retranslationId = null)
    {
        var game = selectedGame;
        if (game?.Session is not { } capturedSession || game.Busy) return;
        var ownsRetranslation = false;
        try
        {
            if (buildPatchAfter && capturedSession.Adapter is UnavailableGameAdapter unavailable)
                throw new GeneratedAdapterException(unavailable.Message);
            await FlushEditsAsync(game);
            await SaveSettingsAsync();
            await EnsureWorkbenchAsync(game);
            if (changingDataDirectory || closing || !games.Contains(game) || game.Busy || !ReferenceEquals(game.Session, capturedSession)) return;
            var settings = game.Project.Settings;
            var providers = ResolveGameApi(game).ResolveTranslationChain().Select(profile => (ITranslationProvider)CreateProvider(game, profile)).ToArray();
            var policy = ReadPolicy(settings);
            if (retranslationId is not null)
            {
                ownsRetranslation = game.Retranslating = true;
                if (selectedGame == game) UpdateWorkbenchControls();
                await FlushEditsAsync(game);
            }
            await StartWorkAsync(game, GameTaskKind.Translation, retranslationId is null ? "准备翻译" : "准备重新翻译所选", async token =>
            {
                Log(game, "文本翻译 API 链：" + string.Join(" → ", providers.Select(provider => provider.DisplayName)));
                var scope = new TranslationScope(trialUnitIds, buildPatchAfter, false, retranslationId);
                await LocalFiles.WriteJsonAsync(Path.Combine(game.Directory, "translation-scope.json"), scope, token);
                game.HasPendingTranslation = true;
                var progress = TranslationProgress(game);
                var options = new TranslationOptions(settings.BatchSize, UnitIds: trialUnitIds)
                { TranslationUpdates = TranslationUpdates(game, retranslationId), FailureUpdates = TranslationFailureUpdates(game), RetranslationId = retranslationId };
                TranslationRunSummary summary;
                try { summary = await Task.Run(() => pipeline.TranslateOnlyAsync(capturedSession, providers, policy, options, progress, token)); }
                finally { if (retranslationId is not null) await RefreshSavedRetranslationsAsync(game); }
                if (buildPatchAfter && summary.Failed == 0)
                {
                    await FlushEditsAsync(game);
                    game.Result = await Task.Run(() => pipeline.BuildFromSavedAsync(capturedSession, policy, trialUnitIds, progress: progress, ct: token));
                    await LoadPatchIndexAsync(game);
                    foreach (var warning in game.Result.Warnings.Distinct(StringComparer.Ordinal)) Log(game, warning);
                }
                await LocalFiles.WriteJsonAsync(Path.Combine(game.Directory, "translation-scope.json"), scope with { Completed = summary.Failed == 0 });
                game.HasPendingTranslation = summary.Failed > 0;
                SetStage(game, summary.Failed > 0 ? $"翻译结束，{summary.Failed} 条失败，可继续重试"
                    : buildPatchAfter ? "试译补丁已生成" : "翻译完成，可继续校对");
            });
            if (buildPatchAfter && selectedGame == game && game.Error is null) ProjectTabs.SelectedItem = PatchTab;
        }
        catch (Exception error) { game.Error = error.Message; Log(game, error.Message); SetStage(game, "需要处理"); }
        finally { if (ownsRetranslation) game.Retranslating = false; if (selectedGame == game) UpdateWorkbenchControls(); }
    }

    private async void Adapt_Click(object sender, RoutedEventArgs e)
    {
        var game = selectedGame;
        if (game is null || game.Busy) return;
        try
        {
            await SaveSettingsAsync();
            var api = ResolveGameApi(game);
            var profile = api.Profiles.FirstOrDefault(item => item.Id == api.AnalysisProfileId);
            var createModel = CreateAnalysisModelFactory(game, profile);
            if (changingDataDirectory || closing || !games.Contains(game)) return;
            var continuing = File.Exists(Path.Combine(game.Directory, "adaptation", "current", "session.json"));
            var improving = game.Project.AdapterId is not null;
            var adaptationTitle = continuing || improving ? "继续智能适配" : "智能识别与适配";
            var defaultObjective = game.AdaptationObjective ?? (improving
                ? !string.IsNullOrWhiteSpace(game.AdaptationInstruction) || !string.IsNullOrWhiteSpace(game.Error)
                    ? AdaptationObjective.Repair : AdaptationObjective.ExpandCoverage
                : AdaptationObjective.Develop);
            var objective = new ComboBox { Header = "本次目标", HorizontalAlignment = HorizontalAlignment.Stretch };
            foreach (var (value, label) in new[]
            {
                (AdaptationObjective.Repair, "修复现有问题"),
                (AdaptationObjective.ExpandCoverage, "补全遗漏文字"),
                (AdaptationObjective.Develop, "识别并建立适配")
            })
            {
                var item = new ComboBoxItem { Content = label, Tag = value };
                objective.Items.Add(item);
                if (value == defaultObjective) objective.SelectedItem = item;
            }
            var objectiveDescription = new HelpButton { VerticalAlignment = VerticalAlignment.Bottom, Margin = new Thickness(0, 0, 0, 2) };
            AutomationProperties.SetName(objectiveDescription, "适配目标说明");
            Grid.SetColumn(objectiveDescription, 1);
            var objectiveField = new Grid { ColumnSpacing = 6,
                ColumnDefinitions = { new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) }, new ColumnDefinition { Width = GridLength.Auto } },
                Children = { objective, objectiveDescription } };
            void UpdateObjectiveDescription() => objectiveDescription.Text = (AdaptationObjective)((ComboBoxItem)objective.SelectedItem).Tag switch
            {
                AdaptationObjective.Repair => "修复编码、字体、启动或回填问题，保留已有文本与译文的匹配关系；完成相关检查后先生成补丁试用。其他遗漏文字留待单独补全。",
                AdaptationObjective.ExpandCoverage => "调查并补齐对白、菜单、界面等遗漏文字。请在下方说明未汉化的位置或原文。",
                _ => "识别引擎并复用已有工具，建立可提取、回填和生成补丁的适配器。"
            };
            objective.SelectionChanged += (_, _) => UpdateObjectiveDescription();
            UpdateObjectiveDescription();
            var effectiveLimits = ResolveGameAdaptationLimits(game);
            var useGlobalLimits = new CheckBox { Content = "适配预算与轮数使用全局默认",
                IsChecked = game.Project.Settings.AdaptationTokenBudget is null && game.Project.Settings.AdaptationMaximumTurns is null };
            var budget = new NumberBox { Header = "本次分析 Token 预算", Value = effectiveLimits.MaximumTokens, Minimum = 1000, Maximum = 20_000_000,
                SpinButtonPlacementMode = NumberBoxSpinButtonPlacementMode.Inline };
            var turns = new NumberBox { Header = "本次最多模型轮数", Value = effectiveLimits.MaximumTurns, Minimum = 1, Maximum = 400,
                SpinButtonPlacementMode = NumberBoxSpinButtonPlacementMode.Inline };
            void UpdateLimitInputs()
            {
                var inherited = useGlobalLimits.IsChecked == true;
                budget.IsEnabled = turns.IsEnabled = !inherited;
                if (inherited)
                {
                    budget.Value = globalSettings.AdaptationTokenBudget;
                    turns.Value = globalSettings.AdaptationMaximumTurns;
                }
            }
            useGlobalLimits.Click += (_, _) => UpdateLimitInputs();
            UpdateLimitInputs();
            var instruction = new TextBox { Header = "补充要求或运行反馈（可选）", AcceptsReturn = true, TextWrapping = TextWrapping.Wrap,
                MinHeight = 90, MaxHeight = 180, MaxLength = 6000,
                Text = game.AdaptationInstruction ?? "",
                PlaceholderText = "例如：对话已翻译，但装备名称、技能说明和菜单仍是原文；或补充中文显示问题。" };
            var content = new StackPanel { Spacing = 12, MaxWidth = 480 };
            content.Children.Add(new TextBlock { TextWrapping = TextWrapping.Wrap,
                Text = "先尝试本地组件组合和验收；能完成时不调用模型。\n"
                    + (profile is null ? "尚未配置分析 API；需要模型接手时会保存进度，配置后可继续。"
                        : $"需要模型接手时使用：{profile.Name} · {profile.Model}\n必要的资源片段和工具结果会发送到 {profile.Endpoint}。智能体可查找公开工具，在本机编写和执行解析代码。")
                    + (continuing ? "\n沿用已有源码和适配进度，按本次目标继续。"
                        : improving ? "\n复用现有适配器，按本次目标处理。" : "\n适配通过后可选择少量文本试译。")
                    + "\n适配成功后会自动重新提取文本，已提交且仍兼容的译文继续复用。" });
            if (continuing && game.AdaptationStatus is not (AdaptationStatus.Activated or AdaptationStatus.Reused)
                && !string.IsNullOrWhiteSpace(game.AdaptationNextAction))
                content.Children.Add(new TextBlock { TextWrapping = TextWrapping.Wrap,
                    Text = (game.AdaptationBlocker is { } blocker ? "当前卡点：" + blocker.Reason + "\n" : "")
                        + "下一步：" + game.AdaptationNextAction });
            content.Children.Add(objectiveField);
            content.Children.Add(instruction);
            var budgetHelp = new HelpButton { Text = "Token 或轮数任一达到上限时暂停并保存进度。取消勾选可为当前项目单独设置，两项设置均会记住。全局默认可在“API 与任务默认值”中修改。" };
            AutomationProperties.SetName(budgetHelp, "本次适配预算说明");
            content.Children.Add(new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6,
                Children = { useGlobalLimits, budgetHelp } });
            content.Children.Add(budget);
            content.Children.Add(turns);
            var dialog = new ContentDialog { XamlRoot = RootGrid.XamlRoot, Title = adaptationTitle,
                Content = new ScrollViewer { Content = content, MaxHeight = 540, VerticalScrollBarVisibility = ScrollBarVisibility.Auto },
                PrimaryButtonText = continuing || improving ? "继续适配" : "开始适配", CloseButtonText = "取消", DefaultButton = ContentDialogButton.Primary };
            var dialogResult = await dialog.ShowAsync();
            if (changingDataDirectory || closing || !games.Contains(game)) return;
            var validBudget = double.IsFinite(budget.Value) && budget.Value is >= 1000 and <= 20_000_000 && budget.Value == Math.Truncate(budget.Value);
            var validTurns = double.IsFinite(turns.Value) && turns.Value is >= 1 and <= 400 && turns.Value == Math.Truncate(turns.Value);
            if (validBudget && validTurns)
            {
                long? budgetOverride = useGlobalLimits.IsChecked == true ? null : (long)budget.Value;
                int? turnsOverride = useGlobalLimits.IsChecked == true ? null : (int)turns.Value;
                if (game.Project.Settings.AdaptationTokenBudget != budgetOverride
                    || game.Project.Settings.AdaptationMaximumTurns != turnsOverride)
                {
                    await SaveSettingsAsync(current => current with
                        { AdaptationTokenBudget = budgetOverride, AdaptationMaximumTurns = turnsOverride });
                    RefreshProjectAdaptationSettings();
                }
            }
            if (dialogResult != ContentDialogResult.Primary || game.Busy) return;
            if (!validBudget || !validTurns) throw new InvalidDataException("请输入有效的适配预算和整数轮数。");
            var limits = new AdaptationLimits((int)turns.Value, (long)budget.Value);
            game.AdaptationInstruction = instruction.Text.Trim();
            game.AdaptationObjective = (AdaptationObjective)((ComboBoxItem)objective.SelectedItem).Tag;
            bool available = false;
            await StartWorkAsync(game, GameTaskKind.Adaptation, adaptationTitle, async token =>
            {
                using var projectLock = AcquireGameFileLock(game);
                var store = CreateAdapterStore(game);
                var runtime = new AdapterRuntime(FindToolchain(), new AdapterProcessRunner());
                var runId = game.RunId;
                game.AdaptationBlocker = null;
                string? lastStage = null;
                var progress = new Progress<AdaptationProgress>(update =>
                {
                    if (!game.AcceptProgress || !game.Busy || game.RunId != runId) return;
                    game.AdaptationUsage = update.Usage;
                    game.AdaptationSummary = update.Message;
                    game.AdaptationProgressNote = update.ProgressNote;
                    game.AdaptationLastBreakthrough = update.LastBreakthrough;
                    var message = update.ToolName is { } toolName ? "调用工具：" + toolName : update.Message;
                    SetStage(game, update.Stage, message);
                    if (update.ToolName is not null || lastStage != update.Stage && update.Stage != "智能分析")
                        Log(game, message);
                    lastStage = update.Stage;
                });
                var baseline = game.AdaptationObjective == AdaptationObjective.Repair
                    ? await LoadAdaptationBaselineAsync(game, token) : null;
                var adaptation = await Task.Run(() => new AutomaticAdaptationService(createModel, runtime, store)
                    .AdaptAsync(new AdaptationRequest(game.Id, game.GameDirectory, adapters, limits, game.AdaptationInstruction,
                        game.BuiltinProbes.ToArray(), ImproveExisting: improving, Objective: game.AdaptationObjective,
                        BaselineUnits: baseline, ReuseFailures: game.ReuseFailures), token, progress));
                game.AdaptationUsage = adaptation.Usage;
                game.HasAdaptationSession = File.Exists(Path.Combine(game.Directory, "adaptation", "current", "session.json"));
                game.AdaptationSummary = adaptation.Summary;
                game.AdaptationProgressNote = adaptation.ProgressNote;
                game.AdaptationLastBreakthrough = adaptation.LastBreakthrough;
                game.AdaptationStatus = adaptation.Status;
                game.AdaptationNextAction = adaptation.NextAction;
                game.PendingTextCoverage = adaptation.PendingCoverage ?? [];
                game.PendingRuntimeChecks = adaptation.PendingRuntimeChecks ?? [];
                game.AdaptationBlocker = adaptation.Blocker;
                available = adaptation.Adapter is not null;
                if (!available)
                {
                    if (adaptation.Diagnostics.Count > 0)
                        await LocalFiles.WriteJsonAsync(Path.Combine(game.Directory, "logs", "adaptation-" + game.RunId + ".json"),
                            new { timestamp = DateTimeOffset.UtcNow, status = adaptation.Status.ToString(), adaptation.Summary, adaptation.Diagnostics, adaptation.Usage });
                    game.Error = adaptation.Status is AdaptationStatus.Failed or AdaptationStatus.Unavailable ? game.AdaptationSummary : null;
                    SetStage(game, AdaptationStage(adaptation.Status));
                    Log(game, game.Stage + "。" + adaptation.NextAction);
                    if (game.Error is not null) Log(game, game.Error);
                    return;
                }
                Log(game, $"适配器已就绪，智能适配累计 {adaptation.Usage.TotalTokens:N0} Token；可以选择文本试译。"
                    + (game.PendingTextCoverage.Count > 0 ? "仍有部分文字未覆盖，可继续适配。" : ""));
                SetStage(game, game.PendingTextCoverage.Count > 0 ? "部分文字可试译" : "适配完成");
            });
            if (available && !closing) await AnalyzeAsync(game);
        }
        catch (Exception error) { game.Error = error.Message; Log(game, error.Message); SetStage(game, "需要处理"); }
    }

    private static string AdaptationStage(AdaptationStatus status) => status switch
    {
        AdaptationStatus.WaitingForUser => "适配需要补充信息或操作",
        AdaptationStatus.Unsupported => "当前方案无法完成适配",
        AdaptationStatus.Cancelled or AdaptationStatus.BudgetExhausted => "已暂停，可继续适配",
        AdaptationStatus.Failed or AdaptationStatus.Unavailable => "适配遇到错误",
        AdaptationStatus.Activated or AdaptationStatus.Reused => "适配已完成，可以试译",
        _ => "适配尚未完成，可继续"
    };

    private async void Install_Click(object sender, RoutedEventArgs e)
    {
        var game = selectedGame;
        if (game?.Result is not { } result || game.Busy) return;
        await StartWorkAsync(game, GameTaskKind.Installation, "安装补丁", async token =>
        {
            using var projectLock = AcquireGameFileLock(game);
            var installed = await Task.Run(() => CreatePatchInstaller(game).InstallAsync(game.GameDirectory, result.PatchDirectory, token));
            Log(game, $"补丁已安装：写入 {installed.InstalledFiles} 个文件，跳过 {installed.UnchangedFiles} 个相同文件。" +
                (result.LaunchExecutable is { } executable ? "启动游戏请运行：" + executable : "按原游戏方式启动。"));
            SetStage(game, "补丁已安装");
        });
    }

    private async void Restore_Click(object sender, RoutedEventArgs e)
    {
        var game = selectedGame;
        if (game is null || game.Busy) return;
        await StartWorkAsync(game, GameTaskKind.Installation, "恢复原版", async token =>
        {
            using var projectLock = AcquireGameFileLock(game);
            var restored = await Task.Run(() => CreatePatchInstaller(game).RestoreAsync(game.GameDirectory, token));
            Log(game, $"已恢复 {restored.RestoredFiles} 个原文件，移除 {restored.RemovedFiles} 个补丁文件。");
            if (restored.Conflicts.Count > 0) throw new IOException("以下文件无法安全恢复，已保留现状：" + string.Join("、", restored.Conflicts));
            SetStage(game, "已恢复原版");
        });
    }

    private async Task StartWorkAsync(GameWorkspace game, GameTaskKind kind, string label, Func<CancellationToken, Task> work)
    {
        if (!game.MetadataLoaded || game.Busy || closing || changingDataDirectory || changingAdapterLibrary || game.IsBeingRemoved || !games.Contains(game)) return;
        game.Cancellation = new();
        game.TaskOrder = ++nextTaskOrder;
        RefreshProjectList();
        game.RunId = Guid.NewGuid().ToString("N");
        game.Kind = kind;
        game.Error = null;
        game.AcceptProgress = true;
        game.ReportProgress = CreateWorkReporter(game);
        var settings = game.Project.Settings;
        var cancellation = game.Cancellation;
        SetStage(game, "等待执行", kind switch
        {
            GameTaskKind.Analysis => "等待常规识别任务开始，其他游戏的识别完成后会自动继续。",
            GameTaskKind.Adaptation => "等待智能适配任务开始，其他游戏的适配完成后会自动继续。",
            GameTaskKind.Summary => "等待分析名额，将总结本项目并维护全局引擎经验。",
            GameTaskKind.Translation => "等待可用翻译名额，已有任务完成后会自动继续。",
            _ => "正在准备项目任务。"
        });
        game.RunningTask = ExecuteAsync();
        await game.RunningTask;
        async Task ExecuteAsync()
        {
            try
            {
                if (game.ContentLoad is { } loadingContent) await loadingContent;
                if (game.LoadingRows is { } loadingRows) await loadingRows;
                cancellation.Token.ThrowIfCancellationRequested();
                await PersistTaskAsync(game, true, settings);
                await coordinator.RunAsync(game.Id, kind, async token =>
                {
                    SetStage(game, label);
                    Log(game, label);
                    await PersistTaskAsync(game, true, settings);
                    await work(token);
                    return true;
                }, cancellation.Token);
            }
            catch (OperationCanceledException)
            {
                game.AcceptProgress = false;
                SetStage(game, "已暂停", kind == GameTaskKind.Analysis && game.HasSavedAnalysis ? "上一次成功提取的结果仍可使用。" : "");
                Log(game, "任务已暂停，已保存的结果可继续使用。");
            }
            catch (Exception error)
            {
                game.AcceptProgress = false;
                game.Error = error.Message;
                SetStage(game, error is TranslationRetriesExhaustedException or TranslationRefusedException ? "已暂停" : "需要处理",
                    kind == GameTaskKind.Analysis && game.HasSavedAnalysis ? "上一次成功提取的结果仍可使用。" : "");
                if (error is not TranslationRetriesExhaustedException and not TranslationRefusedException) Log(game, error.Message);
                string diagnostic = Path.Combine(game.Directory, "logs", "error-" + game.RunId + ".json");
                try { await LocalFiles.WriteJsonAsync(diagnostic, new { timestamp = DateTimeOffset.UtcNow, operation = label, error = error.ToString() }); }
                catch (Exception logError) when (logError is IOException or UnauthorizedAccessException) { Program.WriteStartupError(logError); }
            }
            finally
            {
                game.AcceptProgress = false;
                game.ReportProgress = null;
                try { await PersistTaskAsync(game, false, settings); }
                catch (Exception error) { game.Error = "任务状态未能保存：" + error.Message; Program.WriteStartupError(error); }
                game.Cancellation = null;
                RefreshProjectList();
                cancellation.Dispose();
                RefreshProjectCacheSize(game);
                TrimProjectContentCache();
                game.Notify();
                if (selectedGame == game) RenderSelectedGame();
                else UpdateControls();
                if (kind is GameTaskKind.Analysis or GameTaskKind.Adaptation or GameTaskKind.Summary or GameTaskKind.Patching)
                    RefreshCatalog();
            }
        }
    }

    private Task PersistTaskAsync(GameWorkspace game, bool running, GameTranslationSettings settings)
    {
        var state = new SavedTaskState(game.RunId!, game.Kind, game.Stage, running, game.Progress, game.Error, settings, DateTimeOffset.UtcNow);
        var project = game.Project with { TaskSummary = new(game.Stage, running, game.Progress?.Completed ?? 0, game.Progress?.Total ?? 0) };
        game.Project = project;
        var projectStore = projects;
        return game.EnqueueWrite(async () =>
        {
            await LocalFiles.WriteJsonAsync(Path.Combine(game.Directory, "task-state.json"), state);
            await projectStore.SaveAsync(project);
        });
    }

    private void Cancel_Click(object sender, RoutedEventArgs e) => selectedGame?.Cancellation?.Cancel();
    private static FileStream AcquireGameFileLock(GameWorkspace game) => new(Path.Combine(game.Directory, ".task.lock"), FileMode.OpenOrCreate, FileAccess.ReadWrite, FileShare.None);
    private static PatchInstaller CreatePatchInstaller(GameWorkspace game) => new(game.Directory);
    private LocalAdapterPackageStore CreateAdapterStore(GameWorkspace game) => new(Path.Combine(dataDirectory, "extensions"), Path.Combine(game.Directory, "adaptation"));
    private ApiProfileConfiguration ResolveGameApi(GameWorkspace game) =>
        game.Project.Settings.ResolveApiConfiguration(globalSettings.ReadApiConfiguration());

    private ScheduledProvider CreateProvider(GameWorkspace game, ApiProfile profile)
    {
        var settings = ReadProviderSettings(profile);
        var provider = new OpenAiCompatibleProvider(httpClient, settings,
            Path.Combine(game.Directory, "logs")) { DisplayName = profile.Name };
        return new(provider, ProviderAccount(settings));
    }

    private IAdaptationAgentModel CreateAnalysisProvider(GameWorkspace game, ApiProfile profile) =>
        CreateAnalysisModelFactory(game, profile)();

    private Func<IAdaptationAgentModel> CreateAnalysisModelFactory(GameWorkspace game, ApiProfile? profile)
    {
        // Capture settings and session keys on the UI thread; local work does not require a usable API.
        ProviderSettings settings;
        try
        {
            if (profile is null) throw new InvalidOperationException("请配置全局默认分析 API，或在“项目设置”中单独选择。");
            settings = ReadProviderSettings(profile);
        }
        catch (Exception error) when (error is ArgumentException or FormatException or InvalidOperationException or System.Security.Cryptography.CryptographicException)
        { return () => throw new InvalidOperationException(error.Message, error); }
        var name = profile.Name;
        var logs = Path.Combine(game.Directory, "logs");
        return () => new ScheduledAgentModel(new OpenAiCompatibleAgentModel(httpClient, settings, logs)
            { DisplayName = name }, ProviderAccount(settings));
    }

    private ProviderSettings ReadProviderSettings(ApiProfile profile)
    {
        if (string.IsNullOrWhiteSpace(profile.Endpoint) || string.IsNullOrWhiteSpace(profile.Model))
            throw new ArgumentException($"请补全“{profile.Name}”的 API 地址与模型名称。");
        var key = ReadProfileKey(profile);
        return new(profile.Endpoint, key, profile.Model, profile.Protocol, profile.AnalysisReasoningEffort, profile.TranslationFormat);
    }

    private static string ProviderAccount(ProviderSettings settings) =>
        LocalFiles.HashText(settings.Endpoint.GetLeftPart(UriPartial.Authority) + "\n" + settings.ApiKey);

    private static TranslationPolicy ReadPolicy(GameTranslationSettings settings)
    {
        var terms = new Dictionary<string, string>(StringComparer.Ordinal);
        foreach (var line in settings.Terminology.Split('\n', StringSplitOptions.TrimEntries | StringSplitOptions.RemoveEmptyEntries))
        {
            var separator = line.IndexOf('=');
            if (separator < 1 || separator == line.Length - 1) throw new ArgumentException("术语表每行必须为：原文=译文。");
            if (!terms.TryAdd(line[..separator].Trim(), line[(separator + 1)..].Trim())) throw new ArgumentException("术语表有重复的原文。");
        }
        return new(settings.TargetLanguage, settings.Style, terms);
    }

    private static PinnedDotNetToolchain FindToolchain()
    {
        for (var directory = new DirectoryInfo(AppContext.BaseDirectory); directory is not null; directory = directory.Parent)
            foreach (var root in new[] { Path.Combine(directory.FullName, ".tools", "dotnet"), Path.Combine(directory.FullName, "toolchain") })
                if (File.Exists(Path.Combine(root, "dotnet.exe")) && Directory.Exists(Path.Combine(root, "sdk", "10.0.402"))) return new(root, "10.0.402", "10.0.13");
        throw new AdapterProcessUnavailableException("请使用带 toolchain 的完整发行包，以启用自动适配。");
    }
}
