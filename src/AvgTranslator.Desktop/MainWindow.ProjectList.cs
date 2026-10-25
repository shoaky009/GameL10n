using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace AvgTranslator.Desktop;

public sealed partial class MainWindow
{
    private GameWorkspace[] visibleGames = [];
    private bool updatingProjectList;
    private long nextTaskOrder;
    private int projectListRevision;

    private void InitializeProjectList()
    {
        GameList.ItemsSource = visibleGames;
        games.CollectionChanged += (_, _) =>
        {
            if (!switchingGames && !removingGames) RefreshProjectList();
        };
    }

    private void ProjectSearch_TextChanged(object sender, TextChangedEventArgs e)
    {
        if (GameList is not null && !switchingGames) RefreshProjectList(preservePosition: false);
    }

    // Refilter only on query/membership/task-boundary changes. Progress notifications update their own row.
    private void RefreshProjectList(bool preservePosition = true)
    {
        var query = ProjectSearchBox.Text.Trim();
        var selected = GameList.SelectedItems.OfType<GameWorkspace>().ToHashSet();
        var scroll = FindLibraryScrollViewer(GameList);
        var offset = preservePosition ? scroll?.VerticalOffset ?? 0 : 0;
        var first = (GameList.ItemsPanelRoot as ItemsStackPanel)?.FirstVisibleIndex ?? -1;
        var anchor = preservePosition && offset > 0 && first >= 0 && first < visibleGames.Length ? visibleGames[first] : null;
        var ordered = games.Where(game => query.Length == 0 || game.Name.Contains(query, StringComparison.OrdinalIgnoreCase)
                || game.GameDirectory.Contains(query, StringComparison.OrdinalIgnoreCase))
            .OrderByDescending(game => game.Busy)
            .ThenBy(game => game.Busy ? game.TaskOrder : long.MaxValue)
            .ThenByDescending(game => game.Project.UpdatedAt)
            .ThenBy(game => game.Name, StringComparer.CurrentCultureIgnoreCase)
            .ThenBy(game => game.Id, StringComparer.Ordinal).ToArray();
        if (visibleGames.SequenceEqual(ordered)) { UpdateProjectListStatus(); return; }
        updatingProjectList = true;
        try
        {
            visibleGames = ordered;
            GameList.ItemsSource = visibleGames;
            if (GameList.SelectionMode == ListViewSelectionMode.Single)
                GameList.SelectedItem = visibleGames.FirstOrDefault(selected.Contains);
            else foreach (var game in visibleGames.Where(selected.Contains)) GameList.SelectedItems.Add(game);
        }
        finally { updatingProjectList = false; }
        UpdateProjectListStatus();
        var revision = ++projectListRevision;
        GameList.DispatcherQueue.TryEnqueue(() =>
        {
            if (revision != projectListRevision) return;
            if (anchor is not null && visibleGames.Contains(anchor)) GameList.ScrollIntoView(anchor, ScrollIntoViewAlignment.Leading);
            else FindLibraryScrollViewer(GameList)?.ChangeView(null, offset, null, true);
        });
    }

    private void UpdateProjectListStatus()
    {
        GameEmptyText.Visibility = visibleGames.Length == 0 ? Visibility.Visible : Visibility.Collapsed;
        GameEmptyText.Text = games.Count == 0 ? "添加游戏目录，开始识别和翻译。" : "没有匹配的项目，请调整搜索。";
        var running = games.Count(game => game.Busy);
        ActiveTasksText.Text = (ProjectSearchBox.Text.Trim().Length == 0 ? $"{games.Count:N0} 个项目" : $"{visibleGames.Length:N0} / {games.Count:N0} 个项目")
            + (running > 0 ? $" · {running} 个任务置顶" : "");
    }

    private async Task EnsureProjectMetadataAsync(GameWorkspace game)
    {
        if (game.MetadataLoaded) return;
        if (game.MetadataLoad is { } pending) { await pending; return; }
        var projectStore = projects;
        var storagePipeline = pipeline;
        game.MetadataLoad = ReadAsync();
        if (selectedGame == game) RenderSelectedGame();
        try { await game.MetadataLoad; }
        finally { game.MetadataLoad = null; if (selectedGame == game) RenderSelectedGame(); }

        async Task ReadAsync()
        {
            var messages = new List<string>();
            try
            {
                await Task.Run(async () =>
                {
                    var project = await projectStore.GetAsync(game.Id);
                    if (project is null || project.IsRemoved) throw new FileNotFoundException("项目记录已不存在，请检查项目数据文件夹。");
                    game.Project = project;
                    await LoadSummaryAsync(game, storagePipeline, messages.Add);
                });
                game.MetadataLoaded = true;
                game.ContentError = null;
            }
            catch (Exception error)
            {
                Program.WriteStartupError(error);
                game.ContentError = "项目记录无法读取：" + error.Message;
                game.Stage = "项目文件不可用";
            }
            foreach (var message in messages) Log(game, message);
            game.Notify();
            if (selectedGame == game && globalPage is null && game.MetadataLoaded && !closing && !changingDataDirectory)
                LoadGameSettings(game);
        }
    }
}
