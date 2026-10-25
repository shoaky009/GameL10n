using AvgTranslator.Core.Pipeline;
using AvgTranslator.Core.Storage;
using AvgTranslator.Desktop.Settings;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace AvgTranslator.Desktop;

public sealed partial class MainWindow
{
    private bool changingDataDirectory;
    private sealed record LoadedData(string Directory, DesktopSettings Settings, TranslationPipeline Pipeline,
        GameProjectStore Projects, IReadOnlyList<GameWorkspace> Games, int LoadErrors);

    private async Task<LoadedData> ReadDataDirectoryAsync(string directory)
    {
        var settings = await LocalFiles.ReadRecoverableJsonAsync<DesktopSettings>(Path.Combine(directory, "settings.json")) ?? new();
        var projectStore = new GameProjectStore(directory);
        var storagePipeline = new TranslationPipeline(directory);
        var errors = new List<string>();
        var loaded = await Task.Run(async () => (await projectStore.ListSummariesAsync(onError: (id, error) =>
            { errors.Add(id); Program.WriteStartupError(error); }))
            .Select(summary => new GameWorkspace(summary.ToProject(), Path.Combine(directory, "games", summary.Id))
                { HasSavedAnalysis = summary.Analysis is not null }).ToList());
        if (loaded.Count == 0 && Directory.Exists(settings.GameDirectory))
        {
            var project = projectStore.Create(settings.GameDirectory, DefaultGameSettings(settings));
            var existing = await projectStore.GetAsync(project.Id);
            if (existing?.IsRemoved != true)
            {
                project = existing ?? project;
                await projectStore.SaveAsync(project);
                var game = new GameWorkspace(project, projectStore.GetProjectDirectory(project.Id));
                loaded.Add(game);
            }
        }
        await Task.WhenAll(loaded.Select(game => game.DurableWrites));
        return new(directory, settings, storagePipeline, projectStore, loaded, errors.Count);
    }

    private void ApplyDataDirectory(LoadedData data, bool selectProject = false)
    {
        switchingGames = true;
        try
        {
            selectedGame = null;
            settingsGame = null;
            games.Clear();
            dataDirectory = data.Directory;
            pipeline = data.Pipeline;
            projects = data.Projects;
            globalSettings = data.Settings;
            navigationSettingsDirty = false;
            coordinator = new(Math.Clamp(globalSettings.MaximumTranslations, 1, 8));
            apiConfiguration = globalSettings.ReadApiConfiguration();
            foreach (var game in data.Games) games.Add(game);
            ProjectSearchBox.Text = "";
            if (selectProject)
            {
                selectedGame = games.FirstOrDefault(game => game.Id == globalSettings.LastGameId) ?? games.FirstOrDefault();
                if (selectedGame is { MetadataLoaded: true } game) LoadGameSettings(game);
            }
            ApiSettingsInfo.IsOpen = SharedApiInfo.IsOpen = false;
            DataDirectoryBox.Text = dataDirectory;
            ConcurrentGamesBox.Value = globalSettings.MaximumTranslations;
            DefaultTokenBudgetBox.Value = globalSettings.AdaptationTokenBudget;
            DefaultMaximumTurnsBox.Value = globalSettings.AdaptationMaximumTurns;
            FooterText.Text = "全局数据位置：" + dataDirectory + (data.LoadErrors > 0 ? $"；{data.LoadErrors} 个项目记录无法读取，详情见程序日志。" : "");
            RefreshApiProfiles();
            RefreshCatalog();
            RenderSelectedGame();
        }
        finally { switchingGames = false; RefreshProjectList(preservePosition: false); }
    }

    private async void ChangeDataDirectory_Click(object sender, RoutedEventArgs e)
    {
        if (!initialized || closing || changingDataDirectory || changingAdapterLibrary || SavingEngineLibrary || savingApiProfiles || pendingApiSelections > 0) return;
        changingDataDirectory = true;
        UpdateControls();
        FileStream? targetLock = null;
        try
        {
            var directory = await PickFolderAsync();
            if (directory is null) return;
            directory = Path.TrimEndingDirectorySeparator(Path.GetFullPath(directory));
            if (DataDirectoryTransfer.Same(dataDirectory, directory))
            { ShowDataStatus("当前已在使用这个保存位置。", InfoBarSeverity.Informational); return; }
            DataDirectoryTransfer.ValidateDestination(dataDirectory, directory, games.Select(game => game.GameDirectory));
            var dialog = new ContentDialog
            {
                XamlRoot = RootGrid.XamlRoot, Title = "是否迁移现有数据？",
                Content = new TextBlock
                {
                    Text = $"新位置：{directory}\n\n迁移并切换：复制当前项目、译文、补丁、日志和 API 配置。首次迁移需要空目录，中断后可选择同一目录继续；原数据会保留。\n\n仅切换：使用新位置已有的数据；空目录将显示为空游戏库，不复制当前数据。\n\n切换后立即生效。运行中的任务会先暂停并保存进度，之后可手动继续。"
                        + (DataLocation.IsEnvironmentOverride ? "\n\n当前由环境变量指定目录，本次切换仅对当前运行生效；下次启动仍以环境变量为准。" : ""),
                    TextWrapping = TextWrapping.Wrap
                },
                PrimaryButtonText = "迁移并切换", SecondaryButtonText = "仅切换", CloseButtonText = "取消",
                DefaultButton = ContentDialogButton.Primary
            };
            var choice = await dialog.ShowAsync();
            if (choice == ContentDialogResult.None) return;
            var migrate = choice == ContentDialogResult.Primary;
            // Lock the destination before pausing work. A busy target cannot disturb current tasks.
            targetLock = DataDirectoryTransfer.AcquireLock(directory, allowIncompleteTransfer: migrate);
            ShowDataStatus("正在暂停任务并保存进度…", InfoBarSeverity.Informational);
            DataDirectoryProgress.Visibility = Visibility.Visible;
            foreach (var game in games) game.Cancellation?.Cancel();
            foreach (var game in games) game.ContentLoadCancellation?.Cancel();
            await Task.WhenAll(games.Select(game => game.RunningTask ?? Task.CompletedTask));
            await Task.WhenAll(games.Select(game => game.ContentLoad ?? Task.CompletedTask));
            await Task.WhenAll(games.Select(game => game.MetadataLoad ?? Task.CompletedTask));
            await SaveNavigationSettingsAsync();
            await settingsWriteLock.WaitAsync();
            settingsWriteLock.Release();
            await FlushAllEditsAsync();
            await Task.WhenAll(games.Select(game => game.LoadingRows ?? Task.CompletedTask));
                await Task.WhenAll(games.Select(game => game.DurableWrites));

            var previous = new LoadedData(dataDirectory, globalSettings, pipeline, projects, games.ToArray(), 0);
            if (migrate)
            {
                ShowDataStatus("正在复制并校验数据…", InfoBarSeverity.Informational);
                var progress = new Progress<DataTransferProgress>(value =>
                {
                    if (changingDataDirectory && dataDirectory == previous.Directory)
                        ShowDataStatus($"正在复制并校验数据：{value.Files:N0} 个文件，{value.Bytes / 1048576d:N1} MB。", InfoBarSeverity.Informational);
                });
                await Task.Run(() => DataDirectoryTransfer.CopyAsync(dataDirectory, directory, progress));
            }
            ShowDataStatus("正在加载新位置的数据…", InfoBarSeverity.Informational);
            var loaded = await ReadDataDirectoryAsync(directory);
            if (loaded.Games.Any(game => DataDirectoryTransfer.Contains(game.GameDirectory, directory)
                || DataDirectoryTransfer.Contains(directory, game.GameDirectory)))
                throw new InvalidOperationException("所选数据目录与记录的游戏安装目录重叠，请选择独立的数据目录。");
            await DataLocation.SaveAsync(directory);
            try { ApplyDataDirectory(loaded, selectProject: true); }
            catch
            {
                await DataLocation.SaveAsync(previous.Directory);
                ApplyDataDirectory(previous, selectProject: true);
                throw;
            }
            var oldLock = applicationLock;
            applicationLock = targetLock;
            targetLock = null;
            oldLock?.Dispose();
            if (!migrate) sessionApiKeys.Clear();
            ShowDataStatus((migrate ? "数据已迁移并立即切换，原目录仍保留。" : "已立即切换到新位置，原目录的数据仍保留。")
                + "无需重启；已暂停的任务可在对应项目中继续。"
                + (DataLocation.IsEnvironmentOverride ? "当前使用环境变量，本次切换仅对当前运行生效。" : ""), InfoBarSeverity.Success);
        }
        catch (Exception error)
        {
            Program.WriteStartupError(error);
            ShowDataStatus("未能切换保存位置，继续使用原目录。" + error.Message, InfoBarSeverity.Error);
        }
        finally
        {
            targetLock?.Dispose();
            changingDataDirectory = false;
            DataDirectoryProgress.Visibility = Visibility.Collapsed;
            UpdateApiControls();
        }
    }

    private void ShowDataStatus(string message, InfoBarSeverity severity)
    {
        ApplicationSettingsInfo.Message = message;
        ApplicationSettingsInfo.Severity = severity;
        ApplicationSettingsInfo.IsOpen = true;
    }
}
