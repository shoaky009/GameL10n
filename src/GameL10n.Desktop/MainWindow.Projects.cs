using GameL10n.Core.Pipeline;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace GameL10n.Desktop;

public sealed partial class MainWindow
{
    private bool removingGames;

    private void SetWindowActivity(string? message)
    {
        WindowActivityPanel.Visibility = message is null ? Visibility.Collapsed : Visibility.Visible;
        WindowActivityRing.IsActive = message is not null;
        WindowActivityText.Text = message ?? "";
    }

    private Action<string, string> CreateWorkReporter(GameWorkspace game)
    {
        var runId = game.RunId;
        var gate = new object();
        (string Stage, string Message) latest = ("", "");
        var queued = false;
        return (stage, message) =>
        {
            lock (gate)
            {
                latest = (stage, message);
                if (queued) return;
                queued = true;
            }
            // Coalesce fast file/download updates; don't rebuild the UI once per resource.
            if (!DispatcherQueue.TryEnqueue(() =>
            {
                (string Stage, string Message) update;
                lock (gate) { update = latest; queued = false; }
                if (game.AcceptProgress && game.Busy && game.RunId == runId && games.Contains(game))
                    SetStage(game, update.Stage, update.Message);
            })) lock (gate) queued = false;
        };
    }

    private void MultiSelectGames_Changed(object sender, RoutedEventArgs e)
    {
        if (!initialized || switchingGames || removingGames) return;
        switchingGames = true;
        try
        {
            GameList.SelectionMode = MultiSelectGamesButton.IsChecked == true ? ListViewSelectionMode.Multiple : ListViewSelectionMode.Single;
            if (GameList.SelectionMode == ListViewSelectionMode.Single) GameList.SelectedItem = null;
            else GameList.SelectedItems.Clear();
            if (selectedGame is not null && globalPage is null) GameList.SelectedItem = selectedGame;
        }
        finally { switchingGames = false; UpdateControls(); }
    }

    private async void RemoveGames_Click(object sender, RoutedEventArgs e)
    {
        if (!initialized || removingGames || switchingGames || changingDataDirectory || changingAdapterLibrary || closing) return;
        var targets = GameList.SelectedItems.OfType<GameWorkspace>().ToArray();
        if (targets.Length == 0) return;
        removingGames = true;
        UpdateControls();
        var removed = 0;
        var failures = new List<string>();
        var confirmed = false;
        try
        {
            var deleteData = new CheckBox
            {
                Content = new TextBlock { Text = "同时删除对应项目数据文件夹", TextWrapping = TextWrapping.Wrap },
                IsChecked = false
            };
            var content = new StackPanel { Spacing = 12 };
            content.Children.Add(new TextBlock
            {
                Text = "默认只从列表移除，文件保留；重新添加同一游戏目录可继续使用译文和设置。\n勾选删除后，将永久删除项目的译文、补丁、日志、原文备份和安装记录。游戏安装目录不会被删除，已安装补丁会保留，但无法再通过此项目自动恢复原版。",
                TextWrapping = TextWrapping.Wrap
            });
            content.Children.Add(new ScrollViewer
            {
                MaxHeight = 220, Content = new TextBlock
                {
                    Text = string.Join("\n\n", targets.Select(game => game.Name + "\n" + game.Directory)),
                    TextWrapping = TextWrapping.Wrap, IsTextSelectionEnabled = true
                }
            });
            content.Children.Add(deleteData);
            if (targets.Any(game => game.Busy)) content.Children.Add(new TextBlock
            {
                Text = "所选项目有任务正在运行。确认后会先暂停这些任务并保存进度。", TextWrapping = TextWrapping.Wrap
            });
            var dialog = new ContentDialog
            {
                XamlRoot = RootGrid.XamlRoot, Title = $"移除 {targets.Length} 个游戏项目？", Content = content,
                PrimaryButtonText = "移除", CloseButtonText = "取消", DefaultButton = ContentDialogButton.Close
            };
            if (await dialog.ShowAsync() != ContentDialogResult.Primary) return;
            confirmed = true;
            foreach (var game in targets) game.IsBeingRemoved = true;
            var deleteFiles = deleteData.IsChecked == true;
            SetWindowActivity("正在暂停所选项目并保存进度…");
            if (selectedGame is not null && globalPage is null) await SaveSettingsAsync();
            foreach (var game in targets) game.Cancellation?.Cancel();
            foreach (var game in targets) game.ContentLoadCancellation?.Cancel();
            await Task.WhenAll(targets.Select(game => game.RunningTask ?? Task.CompletedTask));
            await Task.WhenAll(targets.Select(game => game.ContentLoad ?? Task.CompletedTask));
            await Task.WhenAll(targets.Select(game => game.MetadataLoad ?? Task.CompletedTask));
            await Task.WhenAll(targets.Select(game => game.LoadingRows ?? Task.CompletedTask));
            foreach (var game in targets) await FlushEditsAsync(game);
            await Task.WhenAll(targets.Select(game => game.DurableWrites));

            // Clear legacy startup selection before deletion, so an interruption cannot re-import a removed project.
            await settingsWriteLock.WaitAsync();
            try
            {
                if (targets.Any(game => game.Id == globalSettings.LastGameId
                    || string.Equals(game.GameDirectory, globalSettings.GameDirectory, StringComparison.OrdinalIgnoreCase)))
                {
                    globalSettings = globalSettings with { LastGameId = null, GameDirectory = "" };
                    await GameL10n.Core.Storage.LocalFiles.WriteJsonAsync(Path.Combine(dataDirectory, "settings.json"), globalSettings, DesktopJson.TypeInfo<Settings.DesktopSettings>(DesktopJson.Storage));
                }
            }
            finally { settingsWriteLock.Release(); }

            foreach (var game in targets)
            {
                SetWindowActivity((deleteFiles ? "正在删除项目数据：" : "正在移除项目：") + game.Name);
                try
                {
                    await Task.Run(() => projects.RemoveAsync(game.Id, deleteFiles));
                    if (deleteFiles)
                    {
                        try { await CreateAdapterStore(game).ForgetGameAsync(game.Id); }
                        catch (Exception error) when (error is IOException or InvalidDataException or UnauthorizedAccessException or System.Text.Json.JsonException)
                        {
                            Program.WriteStartupError(error);
                            failures.Add(game.Name + "：项目数据已删除，但扩展版本引用记录未能移除：" + error.Message);
                        }
                    }
                    ForgetWorkbenchCache(game);
                    game.Session = null;
                    if (selectedGame == game) selectedGame = null;
                    games.Remove(game);
                    removed++;
                }
                catch (Exception error)
                {
                    Program.WriteStartupError(error);
                    game.Error = error.Message;
                    SetStage(game, "移除未完成");
                    failures.Add(game.Name + "：" + error.Message);
                }
            }
            selectedGame ??= games.FirstOrDefault();
            if (selectedGame is { MetadataLoaded: true } && globalPage is null) LoadGameSettings(selectedGame);
            MultiSelectGamesButton.IsChecked = false;
            GameList.SelectionMode = ListViewSelectionMode.Single;
            GameList.SelectedItem = null;
            if (globalPage is null) GameList.SelectedItem = selectedGame;
            RenderSelectedGame();
            RefreshCatalog();
            FooterText.Text = $"已移除 {removed} 个项目，" + (deleteFiles ? "对应项目数据已删除。" : "项目文件已保留。")
                + (failures.Count > 0 ? $"另有 {failures.Count} 个项目未能移除。" : "");
            if (failures.Count > 0) await new ContentDialog
            {
                XamlRoot = RootGrid.XamlRoot, Title = "部分项目未能移除", CloseButtonText = "关闭",
                Content = new ScrollViewer { MaxHeight = 350, Content = new TextBlock
                    { Text = string.Join("\n\n", failures), TextWrapping = TextWrapping.Wrap, IsTextSelectionEnabled = true } }
            }.ShowAsync();
        }
        catch (Exception error) { ShowError(error); }
        finally
        {
            foreach (var game in targets) game.IsBeingRemoved = false;
            removingGames = false;
            RefreshProjectList();
            SetWindowActivity(null);
            UpdateControls();
        }
        if (confirmed && globalPage is null && selectedGame is { Session: null, Busy: false } next)
        {
            await EnsureProjectMetadataAsync(next);
            if (selectedGame == next && globalPage is null && next.MetadataLoaded) await OpenGameAsync(next);
        }
    }
}
