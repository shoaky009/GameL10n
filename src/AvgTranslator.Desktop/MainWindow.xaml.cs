using System.Diagnostics;
using AvgTranslator.Core;
using AvgTranslator.Core.Adapters;
using AvgTranslator.Core.Development;
using AvgTranslator.Core.Isolation;
using AvgTranslator.Core.Patching;
using AvgTranslator.Core.Pipeline;
using AvgTranslator.Core.Providers;
using AvgTranslator.Core.Storage;
using AvgTranslator.Core.Translation;
using AvgTranslator.Desktop.Settings;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Windows.Storage.Pickers;

namespace AvgTranslator.Desktop;

public sealed partial class MainWindow : Window
{
    private readonly string dataDirectory;
    private readonly TranslationPipeline pipeline;
    private readonly HttpClient httpClient = new() { Timeout = TimeSpan.FromMinutes(3) };
    private readonly List<IGameAdapter> adapters = [new Sfa16AosV2Adapter()];
    private GameSession? session;
    private PipelineResult? result;
    private CancellationTokenSource? taskCancellation;
    private CancellationTokenSource? adaptationCancellation;
    private bool busy;
    private bool initialized;
    private readonly SemaphoreSlim settingsWriteLock = new(1, 1);

    public MainWindow()
    {
        InitializeComponent();
        AppWindow.Resize(new Windows.Graphics.SizeInt32(1060, 820));
        dataDirectory = Environment.GetEnvironmentVariable("AVG_TRANSLATOR_DATA") is { Length: > 0 } configuredData
            ? Path.GetFullPath(configuredData)
            : Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "AvgTranslator");
        pipeline = new TranslationPipeline(dataDirectory, Path.Combine(AppContext.BaseDirectory, "logs"));
        RootGrid.Loaded += Loaded;
        GamePathBox.TextChanged += (_, _) =>
        {
            if (busy || !initialized) return;
            session = null; result = null;
            PatchPathBox.Text = "";
            PatchScopeText.Text = "尚未生成补丁。可先选择少量文本试译，检查游戏中的显示效果。";
            InstallButton.Content = "安装汉化补丁";
            PatchInstallInfo.IsOpen = false;
            InstallTargetText.Text = "安装位置：" + GamePathBox.Text.Trim();
            UpdateControls();
        };
        Closed += (_, _) => { taskCancellation?.Cancel(); adaptationCancellation?.Cancel(); };
    }

    private async void Loaded(object sender, RoutedEventArgs e)
    {
        if (initialized) return;
        try
        {
            var settings = await LocalFiles.ReadJsonAsync<DesktopSettings>(Path.Combine(dataDirectory, "settings.json"));
            if (settings is not null)
            {
                GamePathBox.Text = settings.GameDirectory;
                apiConfiguration = settings.ReadApiConfiguration();
                LanguageBox.Text = settings.TargetLanguage;
                StyleBox.Text = settings.Style;
                TerminologyBox.Text = settings.Terminology;
                BatchSizeBox.Value = settings.BatchSize;
            }
            FooterText.Text = "任务与译文保存在：" + dataDirectory;
        }
        catch (Exception error) { Log("读取设置失败：" + error.Message); }
        initialized = true;
        RefreshApiProfiles();
        RefreshCatalog();
        UpdateControls();
    }

    private async void Browse_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            var picker = new FolderPicker();
            picker.FileTypeFilter.Add("*");
            WinRT.Interop.InitializeWithWindow.Initialize(picker, WinRT.Interop.WindowNative.GetWindowHandle(this));
            var folder = await picker.PickSingleFolderAsync();
            if (folder is not null) { GamePathBox.Text = folder.Path; await AnalyzeAsync(); }
        }
        catch (Exception error) { ShowError(error); }
    }

    private async void Analyze_Click(object sender, RoutedEventArgs e) => await AnalyzeAsync();

    private async Task AnalyzeAsync()
    {
        if (busy) return;
        BeginTask();
        session = null; result = null;
        PatchPathBox.Text = "";
        try
        {
            var gamePath = Path.GetFullPath(GamePathBox.Text.Trim());
            if (!Directory.Exists(gamePath)) throw new DirectoryNotFoundException("请选择存在的游戏目录。");
            StageText.Text = "识别与提取";
            var candidates = new List<IGameAdapter>(adapters);
            var store = new LocalAdapterPackageStore(Path.Combine(dataDirectory, "extensions"));
            var active = store.LoadActive(TranslationPipeline.GameId(gamePath));
            if (active is not null)
                candidates.Insert(0, new GeneratedGameAdapter(active, FindToolchain(), new WindowsAppContainerProcessRunner(), store.WorkDirectory));
            var catalogDiagnostics = new List<string>();
            foreach (var package in store.ListReusablePackages(catalogDiagnostics))
                if (!candidates.Any(candidate => candidate.Id == package.Id && candidate.Version == package.Version))
                    candidates.Add(new GeneratedGameAdapter(package, FindToolchain(), new WindowsAppContainerProcessRunner(), store.WorkDirectory));
            foreach (var diagnostic in catalogDiagnostics) Log(diagnostic);
            var token = taskCancellation!.Token;
            foreach (var adapter in candidates)
            {
                try
                {
                    if (await Task.Run(() => adapter.ProbeAsync(gamePath, token), token) is null) continue;
                    var analyzed = await Task.Run(() => pipeline.AnalyzeAsync(gamePath, adapter, token), token);
                    if (adapter is GeneratedGameAdapter generated &&
                        (active?.Id != generated.Id || active.Version != generated.Version))
                    {
                        StageText.Text = "验证已有模板与此游戏的兼容性";
                        var verification = await Task.Run(() => new GeneratedAdapterVerifier().VerifyAsync(generated,
                            analyzed.SourceDirectory, Path.Combine(store.WorkDirectory, "reuse-" + Guid.NewGuid().ToString("N")), token), token);
                        foreach (var diagnostic in verification.Diagnostics) Log(diagnostic);
                        if (!verification.Passed) continue;
                        await store.RecordCompatibilityAsync(generated.Package.Family!, new GameCompatibility(analyzed.Id,
                            Path.GetFileName(Path.TrimEndingDirectorySeparator(gamePath)), "模板声明的资源集合", generated.Version,
                            analyzed.Sources.ToDictionary(file => file.RelativePath, file => file.Sha256), "resource-roundtrip-passed"), token);
                        await store.PublishAndActivateAsync(analyzed.Id, generated.Package, verification.Diagnostics, token);
                        Log("已复用资源模板，无需调用模型开发适配器：" + generated.Id);
                    }
                    session = analyzed;
                    break;
                }
                catch (Exception error) when (adapter is GeneratedGameAdapter && error is not OperationCanceledException)
                {
                    Log($"扩展 {adapter.Id} 未匹配或验证失败：{error.Message}");
                }
            }
            if (session is null)
            {
                GameInfoBar.Severity = InfoBarSeverity.Warning;
                GameInfoBar.Title = "尚无适配此游戏的扩展";
                GameInfoBar.Message = "可使用自动适配分析受限资源样本；失败时保留诊断，不修改游戏文件。";
                StageText.Text = "等待自动适配";
                CountsText.Text = "—";
                Log("现有 Adapter 未识别此游戏。自动适配在单独任务中运行。");
            }
            else
            {
                GameInfoBar.Severity = InfoBarSeverity.Success;
                GameInfoBar.Title = session.Probe.Description;
                GameInfoBar.Message = $"已提取 {session.Extraction.Units.Count:N0} 个文本单元。";
                StageText.Text = "可以开始翻译";
                CountsText.Text = $"0 / 0 / 0（共 {session.Extraction.Units.Count:N0} 条）";
                foreach (var warning in session.Extraction.Warnings) Log(warning);
                result = await LocalFiles.ReadJsonAsync<PipelineResult>(Path.Combine(session.ProjectDirectory, "latest-result.json"));
                if (result?.SourceFingerprint != Path.GetFileName(session.SourceDirectory)) result = null;
                if (result is not null) DisplayResult(result);
            }
            await SaveSettingsAsync();
            RefreshCatalog();
        }
        catch (OperationCanceledException) { Log("识别已取消。"); }
        catch (Exception error) { ShowError(error); }
        finally { EndTask(); }
    }

    private async void Translate_Click(object sender, RoutedEventArgs e) => await TranslateAsync();

    private async Task TranslateAsync(IReadOnlyList<string>? trialUnitIds = null)
    {
        if (session is null || busy) return;
        BeginTask();
        try
        {
            var provider = CreateProvider(ApiPurpose.Translation);
            var policy = ReadPolicy();
            var captured = session;
            var batchSize = double.IsFinite(BatchSizeBox.Value) ? (int)BatchSizeBox.Value : 24;
            await SaveSettingsAsync();
            var progress = new Progress<PipelineProgress>(update =>
            {
                StageText.Text = update.Stage;
                CountsText.Text = $"{update.Completed:N0} / {update.CacheHits:N0} / {update.TranslationTokens:N0}";
                TaskProgress.IsIndeterminate = false;
                TaskProgress.Value = update.Total > 0 ? 100.0 * update.Completed / update.Total : 0;
                // The exception handler displays terminal failures once; progress still supplies final counts.
                if (update.Stage != "失败") Log(update.Message);
            });
            result = await Task.Run(() => pipeline.RunAsync(captured, provider, policy,
                new RunOptions(batchSize, TrialUnitIds: trialUnitIds), progress, taskCancellation!.Token));
            DisplayResult(result);
            RefreshCatalog();
        }
        catch (OperationCanceledException) { StageText.Text = "已暂停"; Log("已保存完成的译文，再次开始可继续。"); }
        catch (TranslationRetriesExhaustedException) { StageText.Text = "已暂停"; }
        catch (TranslationRefusedException) { StageText.Text = "已暂停"; }
        catch (Exception error) { ShowError(error); }
        finally { EndTask(); }
    }

    private async void Adapt_Click(object sender, RoutedEventArgs e)
    {
        if (adaptationCancellation is not null) { adaptationCancellation.Cancel(); return; }
        try
        {
            var path = Path.GetFullPath(GamePathBox.Text.Trim());
            if (!Directory.Exists(path)) throw new DirectoryNotFoundException("游戏目录不存在。");
            var profile = apiConfiguration.Resolve(ApiPurpose.GameAnalysis);
            var provider = CreateProvider(ApiPurpose.GameAnalysis);
            var samples = FindSamples(path);
            if (samples.Length == 0) throw new InvalidDataException("未找到可直接分析的小型文本或脚本资源。当前自动适配不上传完整大型资源包，诊断为：缺少可验证的受限样本。");
            var dialog = new ContentDialog { XamlRoot = RootGrid.XamlRoot, Title = "分析资源样本",
                Content = $"将以下文件作为适配样本发送到“{profile.Name}”（{new Uri(profile.Endpoint).Host}），总计最多 128 KB：\n\n" + string.Join("\n", samples) +
                    "\n\n最多尝试 2 次，适配预算 16,000 Token。生成的扩展必须经过隔离构建和回填测试。",
                PrimaryButtonText = "开始自动适配", CloseButtonText = "取消", DefaultButton = ContentDialogButton.Close };
            if (await dialog.ShowAsync() != ContentDialogResult.Primary) return;
            var store = new LocalAdapterPackageStore(Path.Combine(dataDirectory, "extensions"));
            var compiler = new GeneratedAdapterCompiler(FindToolchain(), new WindowsAppContainerProcessRunner());
            adaptationCancellation = new();
            AdaptButton.Content = "取消适配";
            UpdateControls();
            Log("自动适配已开始：" + path);
            var adaptation = await Task.Run(() => new AutomaticAdaptationService(provider, compiler, store)
                .AdaptAsync(new AdaptationRequest(TranslationPipeline.GameId(path), path, samples, adapters), adaptationCancellation.Token));
            foreach (var diagnostic in adaptation.Diagnostics) Log(diagnostic);
            RefreshCatalog();
            Log($"适配状态：{adaptation.Status}；开发 Token：{adaptation.Usage.TotalTokens:N0}；诊断目录：{adaptation.WorkspacePath}");
            if (adaptation.Adapter is not null && string.Equals(path, GamePathBox.Text.Trim(), StringComparison.OrdinalIgnoreCase) && !busy)
                await AnalyzeAsync();
        }
        catch (Exception error) { ShowError(error); }
        finally
        {
            adaptationCancellation?.Dispose(); adaptationCancellation = null;
            AdaptButton.Content = "自动适配未知格式";
            UpdateControls();
        }
    }

    private void CatalogSearch_TextChanged(object sender, TextChangedEventArgs e)
    {
        if (initialized) RefreshCatalog();
    }

    private void RefreshCatalog_Click(object sender, RoutedEventArgs e) => RefreshCatalog();

    private void RefreshCatalog()
    {
        try
        {
            var diagnostics = new List<string>();
            var entries = new LocalAdapterPackageStore(Path.Combine(dataDirectory, "extensions"))
                .ReadCatalog(adapters.Select(adapter => adapter.CatalogEntry).OfType<AdapterCatalogEntry>(), CatalogSearchBox.Text, diagnostics);
            CatalogText.Text = entries.Count == 0 ? "没有匹配的引擎、资源模板或游戏记录。" : string.Join("\n\n", entries.Select(entry =>
                $"{entry.Family.Name}\n{entry.Family.AdapterId} · {entry.Version}\n" + string.Join("\n", entry.Games.Select(game =>
                    $"  {game.Title} · {game.Variant}\n  {StructureLabel(game.StructureStatus)}；游戏运行：{(game.RuntimeStatus == "passed" ? "已验证" : "未验证")}"))));
            foreach (var diagnostic in diagnostics) Log(diagnostic);
        }
        catch (Exception error) { CatalogText.Text = "读取兼容记录失败：" + error.Message; }
    }

    private static string StructureLabel(string status) => status switch
    {
        "sample-roundtrip-passed" => "样本回填通过",
        "resource-roundtrip-passed" => "资源回填通过",
        "patch-validated" => "汉化产物结构校验通过",
        _ => "结构验证：" + status
    };

    private async void Install_Click(object sender, RoutedEventArgs e)
    {
        if (session is null || result is null || busy) return;
        BeginTask();
        PatchInstallInfo.IsOpen = true;
        PatchInstallInfo.Severity = InfoBarSeverity.Informational;
        PatchInstallInfo.Title = "正在安装";
        PatchInstallInfo.Message = "正在备份并写入：" + session.GameDirectory;
        try
        {
            var installed = await new PatchInstaller().InstallAsync(session.GameDirectory, result.PatchDirectory, taskCancellation!.Token);
            Log($"补丁已安装：新增或替换 {installed.InstalledFiles} 个文件，跳过 {installed.UnchangedFiles} 个相同文件，保留 {installed.BackupFiles.Count} 个备份。");
            StageText.Text = "补丁已安装";
            PatchInstallInfo.Severity = InfoBarSeverity.Success;
            PatchInstallInfo.Title = result.IsTrial ? "试译补丁已安装" : "汉化补丁已安装";
            PatchInstallInfo.Message = $"已安装到 {session.GameDirectory}，新增或替换 {installed.InstalledFiles} 个文件。" +
                (result.LaunchExecutable is { } executable ? $"请从游戏目录运行 {executable}。" : "请从游戏目录启动游戏。");
        }
        catch (OperationCanceledException)
        {
            PatchInstallInfo.Severity = InfoBarSeverity.Warning;
            PatchInstallInfo.Title = "安装已暂停";
            PatchInstallInfo.Message = "再次点击安装将根据记录继续。";
            Log(PatchInstallInfo.Message);
        }
        catch (Exception error)
        {
            PatchInstallInfo.Severity = InfoBarSeverity.Error;
            PatchInstallInfo.Title = "补丁未安装完成";
            PatchInstallInfo.Message = error.Message;
            ShowError(error);
            try
            {
                await LocalFiles.WriteJsonAsync(Path.Combine(session.ProjectDirectory, "installation-error.json"), new
                {
                    failedAt = DateTimeOffset.UtcNow, session.GameDirectory, result.PatchDirectory, error = error.ToString()
                });
            }
            catch (Exception logError) when (logError is IOException or UnauthorizedAccessException)
            {
                Log("无法保存安装诊断：" + logError.Message);
            }
        }
        finally { EndTask(); }
    }

    private async void Restore_Click(object sender, RoutedEventArgs e)
    {
        if (busy) return;
        BeginTask();
        PatchInstallInfo.IsOpen = false;
        try
        {
            var restored = await new PatchInstaller().RestoreAsync(GamePathBox.Text.Trim(), taskCancellation!.Token);
            Log($"已恢复 {restored.RestoredFiles} 个原文件，移除 {restored.RemovedFiles} 个补丁新增文件。" +
                (restored.Conflicts.Count > 0 ? "以下文件在安装后被其他程序修改，已保留：" + string.Join("、", restored.Conflicts) : ""));
            StageText.Text = restored.Conflicts.Count > 0 ? "恢复存在冲突" : "已恢复原版";
        }
        catch (OperationCanceledException) { Log("恢复已暂停；再次恢复可继续。"); }
        catch (Exception error) { ShowError(error); }
        finally { EndTask(); }
    }

    private void Cancel_Click(object sender, RoutedEventArgs e) => taskCancellation?.Cancel();
    private void OpenPatch_Click(object sender, RoutedEventArgs e)
    {
        if (result is not null && Directory.Exists(result.PatchDirectory))
            Process.Start(new ProcessStartInfo(result.PatchDirectory) { UseShellExecute = true });
    }
    private async void SaveSettings_Click(object sender, RoutedEventArgs e)
    {
        try { await SaveSettingsAsync(); ShowApiStatus("翻译设置已保存。"); }
        catch (Exception error) { ShowError(error); }
    }

    private OpenAiCompatibleProvider CreateProvider(ApiPurpose purpose)
    {
        var profile = apiConfiguration.Resolve(purpose);
        if (string.IsNullOrWhiteSpace(profile.Endpoint) || string.IsNullOrWhiteSpace(profile.Model))
            throw new ArgumentException($"请在“API 与术语”中补全“{profile.Name}”的地址与模型名称。");
        var provider = new OpenAiCompatibleProvider(httpClient, new ProviderSettings(profile.Endpoint,
            ReadProfileKey(profile), profile.Model, profile.Protocol), Path.Combine(AppContext.BaseDirectory, "logs"));
        Log($"{(purpose == ApiPurpose.GameAnalysis ? "游戏分析" : "文本翻译")}使用 API：{profile.Name} · {profile.Description}");
        return provider;
    }
    private TranslationPolicy ReadPolicy()
    {
        var terms = new Dictionary<string, string>(StringComparer.Ordinal);
        foreach (var line in TerminologyBox.Text.Split('\n', StringSplitOptions.TrimEntries | StringSplitOptions.RemoveEmptyEntries))
        {
            var separator = line.IndexOf('=');
            if (separator < 1 || separator == line.Length - 1) throw new ArgumentException("术语表每行必须为：原文=译文。");
            if (!terms.TryAdd(line[..separator].Trim(), line[(separator + 1)..].Trim())) throw new ArgumentException("术语表有重复的原文。");
        }
        return new TranslationPolicy(LanguageBox.Text.Trim(), StyleBox.Text.Trim(), terms);
    }
    private async Task SaveSettingsAsync(Func<ApiProfileConfiguration, ApiProfileConfiguration>? updateApiConfiguration = null)
    {
        await settingsWriteLock.WaitAsync();
        try
        {
            var nextApiConfiguration = updateApiConfiguration?.Invoke(apiConfiguration) ?? apiConfiguration;
            await LocalFiles.WriteJsonAsync(Path.Combine(dataDirectory, "settings.json"), new DesktopSettings
            {
                GameDirectory = GamePathBox.Text.Trim(), TargetLanguage = LanguageBox.Text.Trim(),
                Style = StyleBox.Text, Terminology = TerminologyBox.Text,
                BatchSize = double.IsFinite(BatchSizeBox.Value) ? (int)BatchSizeBox.Value : 24,
                ApiConfiguration = nextApiConfiguration
            });
            apiConfiguration = nextApiConfiguration;
        }
        finally { settingsWriteLock.Release(); }
    }
    private void BeginTask() { busy = true; taskCancellation = new(); TaskProgress.IsIndeterminate = true; UpdateControls(); }
    private void EndTask() { busy = false; taskCancellation?.Dispose(); taskCancellation = null; TaskProgress.IsIndeterminate = false; UpdateControls(); }
    private void UpdateControls()
    {
        BrowseButton.IsEnabled = AnalyzeButton.IsEnabled = GamePathBox.IsEnabled = !busy;
        TranslateButton.IsEnabled = TrialButton.IsEnabled = !busy && session is not null;
        CancelButton.IsEnabled = busy;
        AdaptButton.IsEnabled = adaptationCancellation is not null || (!busy && session is null && Directory.Exists(GamePathBox.Text.Trim()));
        InstallButton.IsEnabled = !busy && result is not null && session is not null;
        OpenPatchButton.IsEnabled = result is not null;
        RestoreButton.IsEnabled = !busy && Directory.Exists(GamePathBox.Text.Trim());
    }
    private void DisplayResult(PipelineResult value)
    {
        PatchInstallInfo.IsOpen = false;
        InstallTargetText.Text = "安装位置：" + (session?.GameDirectory ?? GamePathBox.Text.Trim());
        PatchScopeText.Text = value.IsTrial
            ? $"试译补丁 · 已选文本 {value.TranslatedUnits ?? 0:N0} / 全游戏 {value.TotalUnits:N0} 条。其余文本保留原文。"
            : $"全量汉化补丁 · 共 {value.TotalUnits:N0} 条文本。";
        InstallButton.Content = value.IsTrial ? "安装试译补丁" : "安装汉化补丁";
        PatchPathBox.Text = value.PatchDirectory;
        LaunchText.Text = value.LaunchExecutable is null ? "按原游戏方式启动。" : "安装后启动：" + value.LaunchExecutable;
        ValidationText.Text = $"结构校验：{(value.Validation.Passed ? "通过" : "未通过")}　游戏运行验证：{(value.Validation.RuntimeStatus == "passed" ? "通过" : "未执行")}";
        foreach (var warning in value.Warnings) Log(warning);
    }
    private void ShowError(Exception error)
    {
        GameInfoBar.Severity = InfoBarSeverity.Error; GameInfoBar.Title = "任务未完成"; GameInfoBar.Message = error.Message;
        StageText.Text = "需要处理"; Log(error.Message);
    }
    private void Log(string message)
    {
        LogBox.Text += $"[{DateTime.Now:HH:mm:ss}] {message}\n";
        if (LogBox.Text.Length > 40_000) LogBox.Text = LogBox.Text[^30_000..];
        LogBox.SelectionStart = LogBox.Text.Length;
    }
    private static string[] FindSamples(string root)
    {
        var extensions = new HashSet<string>([".json", ".csv", ".xml", ".txt", ".scr", ".ks", ".rpy", ".dat", ".bin", ".aos", ".scn"], StringComparer.OrdinalIgnoreCase);
        var options = new EnumerationOptions { RecurseSubdirectories = true, MaxRecursionDepth = 3, AttributesToSkip = FileAttributes.ReparsePoint, IgnoreInaccessible = true };
        var selected = new List<string>(); long bytes = 0;
        foreach (var file in Directory.EnumerateFiles(root, "*", options).Take(20_000)
            .Where(f => extensions.Contains(Path.GetExtension(f))).OrderBy(f => f, StringComparer.OrdinalIgnoreCase))
        {
            var relative = Path.GetRelativePath(root, file);
            if (relative.Split(Path.DirectorySeparatorChar).Any(p => p.StartsWith('.'))) continue;
            var length = new FileInfo(file).Length;
            if (length == 0 || length > 64 * 1024 || bytes + length > 128 * 1024) continue;
            selected.Add(relative); bytes += length;
            if (selected.Count == 4) break;
        }
        return selected.ToArray();
    }
    private static PinnedDotNetToolchain FindToolchain()
    {
        for (var directory = new DirectoryInfo(AppContext.BaseDirectory); directory is not null; directory = directory.Parent)
        {
            foreach (var root in new[] { Path.Combine(directory.FullName, ".tools", "dotnet"), Path.Combine(directory.FullName, "toolchain") })
                if (File.Exists(Path.Combine(root, "dotnet.exe")) && Directory.Exists(Path.Combine(root, "sdk", "10.0.402")))
                    return new PinnedDotNetToolchain(root, "10.0.402", "10.0.13");
        }
        throw new IsolationUnavailableException("自动适配所需的固定版本编译工具未安装。请使用带 toolchain 的完整发行包；已支持游戏的翻译不需要此组件。");
    }
}
