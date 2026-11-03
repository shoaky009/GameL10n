using GameL10n.Core;
using GameL10n.Core.Adapters;
using GameL10n.Core.Development;
using GameL10n.Core.Pipeline;
using GameL10n.Core.Providers;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace GameL10n.Desktop;

public sealed partial class MainWindow
{
    private EngineKnowledgeStore KnowledgeStore() => new(Path.Combine(dataDirectory, "extensions", "engine-knowledge"));

    private async void EngineReview_Click(object sender, RoutedEventArgs e)
    {
        if (selectedGame is not { Busy: false } game) return;
        try
        {
            await SaveSettingsAsync();
            var provider = CreateAnalysisProvider(game, ResolveGameApi(game).Resolve(ApiPurpose.GameAnalysis));
            var knowledgeStore = KnowledgeStore();
            await StartWorkAsync(game, GameTaskKind.Summary, "总结引擎经验", async token =>
            {
                using var projectLock = AcquireGameFileLock(game);
                var runId = game.RunId;
                string? lastStage = null;
                var progress = new Progress<EngineReviewProgress>(update =>
                {
                    if (!game.AcceptProgress || game.RunId != runId) return;
                    SetStage(game, update.Stage, update.Message);
                    if (lastStage != update.Stage) Log(game, update.Message);
                    lastStage = update.Stage;
                });
                var result = await Task.Run(() => new EngineReviewService(provider, knowledgeStore)
                    .ReviewAsync(game.Directory, game.Id, game.Name, token, progress), token);
                game.EngineReview = result;
                SetStage(game, result.Reused ? "已复用上次总结" : "总结完成");
                Log(game, result.Reused ? "证据和全局经验未变化，本次未调用模型。" : result.EntryId is null
                    ? $"总结完成，没有新增引擎经验；本次使用 {result.Usage.TotalTokens:N0} Token。"
                    : $"已更新全局引擎经验 {result.EntryId}；本次使用 {result.Usage.TotalTokens:N0} Token。");
                if (selectedGame == game) { RenderEngineReview(game); EngineReviewPanel.IsExpanded = true; }
                RefreshEngineKnowledge();
            });
        }
        catch (Exception error) { Log(game, error.Message); }
    }

    private void RenderEngineReview(GameWorkspace? game)
    {
        var review = game?.EngineReview;
        EngineReviewPanel.Visibility = review is null ? Visibility.Collapsed : Visibility.Visible;
        EngineReviewSummaryText.Text = review is null ? "" : review.Summary
            + (review.EntryId is null ? "" : "\n\n全局经验：" + review.EntryId)
            + (review.MainProgramSuggestions is { Count: > 0 } ? "\n\n主程序改进建议（尚未实施）：\n" + string.Join("\n", review.MainProgramSuggestions) : "");
        EngineReviewUsageText.Text = review is null ? "" : $"{review.ReviewedAt.ToLocalTime():yyyy-MM-dd HH:mm} · 总结 Token：{review.Usage.TotalTokens:N0}"
            + (review.Usage.Estimated ? "（含估算）" : "") + (review.Reused ? " · 本次复用，未调用模型" : "");
    }

    private int engineKnowledgeReadVersion;
    private string? selectedKnowledgeId;
    private EngineKnowledgeEntry? selectedKnowledgeEntry;
    private bool renderingLibraryKnowledge;

    private void RefreshEngineKnowledge() => RefreshCatalog();

    private void RenderLibraryKnowledge(AdapterFamily family, bool changedFamily)
    {
        var keys = new HashSet<string>(StringComparer.OrdinalIgnoreCase) { family.AdapterId, family.Key, family.Name };
        var related = engineKnowledgeSummaries.Where(entry => entry.Family.AdapterId == family.AdapterId || entry.Aliases.Any(keys.Contains))
            .OrderBy(entry => entry.Summary, StringComparer.CurrentCultureIgnoreCase).Select(entry => new EngineKnowledgeRow(entry)).ToArray();
        var previousId = changedFamily ? null : selectedKnowledgeId;
        renderingLibraryKnowledge = true;
        try
        {
            EngineKnowledgeList.ItemsSource = related;
            EngineKnowledgeList.SelectedItem = related.FirstOrDefault(row => row.Data.Id == previousId);
        }
        finally { renderingLibraryKnowledge = false; }
        CatalogKnowledgeCountText.Text = related.Length == 0 ? "暂无相关全局经验。可在有分析记录的项目中点击“总结”。"
            : $"{related.Length:N0} 条相关经验 · 启停对下一次智能识别与继续适配生效";
        if (EngineKnowledgeList.SelectedItem is not EngineKnowledgeRow row)
            ClearLibraryKnowledgeSelection();
        else if (selectedKnowledgeEntry is { } entry && entry.Revision == row.Data.Revision)
            CatalogKnowledgeEnabledBox.IsChecked = row.Data.Enabled;
        else _ = LoadLibraryKnowledgeAsync(row.Data.Id);
    }

    private void ClearLibraryKnowledgeSelection()
    {
        engineKnowledgeReadVersion++;
        selectedKnowledgeId = null;
        selectedKnowledgeEntry = null;
        CatalogKnowledgeProgress.Visibility = Visibility.Collapsed;
        CatalogKnowledgeEnabledBox.IsChecked = false;
        CatalogKnowledgeEnabledBox.IsEnabled = false;
        CatalogKnowledgeDetailText.Text = "选择经验以查看适用条件与分析指引。";
    }

    private async void EngineKnowledge_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (renderingLibraryKnowledge) return;
        await LoadLibraryKnowledgeAsync((EngineKnowledgeList.SelectedItem as EngineKnowledgeRow)?.Data.Id);
    }

    private async Task LoadLibraryKnowledgeAsync(string? id)
    {
        var version = ++engineKnowledgeReadVersion;
        selectedKnowledgeEntry = null;
        selectedKnowledgeId = id;
        CatalogKnowledgeEnabledBox.IsEnabled = false;
        if (id is null)
        {
            CatalogKnowledgeProgress.Visibility = Visibility.Collapsed;
            CatalogKnowledgeDetailText.Text = "选择经验以查看适用条件与分析指引。";
            return;
        }
        CatalogKnowledgeDetailText.Text = "正在读取分析指引…";
        CatalogKnowledgeProgress.Visibility = Visibility.Visible;
        var store = KnowledgeStore();
        var directory = dataDirectory;
        bool IsCurrent() => version == engineKnowledgeReadVersion && directory == dataDirectory && selectedKnowledgeId == id;
        try
        {
            var entry = await Task.Run(() => store.ReadEntry(id));
            if (!IsCurrent()) return;
            if (entry is null) throw new FileNotFoundException("这条经验的文件已不存在，索引记录仍保留。");
            selectedKnowledgeEntry = entry;
            CatalogKnowledgeEnabledBox.IsChecked = entry.Enabled;
            CatalogKnowledgeEnabledBox.IsEnabled = !SavingEngineLibrary;
            CatalogKnowledgeDetailText.Text = $"{entry.Summary}\n\n适用条件\n{entry.Conditions}\n\n分析指引\n{entry.Guidance}\n\n来源游戏："
                + string.Join("、", entry.Origins.Select(origin => origin.Title)) + $"\n辅助工具：{entry.Tools.Count:N0} 个"
                + (entry.Tools.Count == 0 ? "" : "\n\n" + string.Join("\n\n", entry.Tools.Select(tool => $"{tool.Name}\n{tool.Description}\n{tool.Usage}")));
        }
        catch (Exception error)
        {
            if (IsCurrent()) CatalogKnowledgeDetailText.Text = "读取经验失败：" + error.Message;
        }
        finally { if (IsCurrent()) CatalogKnowledgeProgress.Visibility = Visibility.Collapsed; }
    }

    private async void CatalogKnowledgeEnabled_Click(object sender, RoutedEventArgs e)
    {
        if (selectedKnowledgeEntry is not { } entry || changingDataDirectory || SavingEngineLibrary || closing) return;
        var enabled = CatalogKnowledgeEnabledBox.IsChecked == true;
        var directory = dataDirectory;
        CatalogKnowledgeEnabledBox.IsEnabled = false;
        engineKnowledgeSaving = true;
        UpdateControls();
        UpdateLibraryActions();
        try
        {
            var store = KnowledgeStore();
            await Task.Run(() => store.SetEnabledAsync(entry.Id, enabled));
            if (directory != dataDirectory) return;
            engineKnowledgeSummaries = engineKnowledgeSummaries.Select(summary => summary.Id == entry.Id ? summary with { Enabled = enabled } : summary).ToArray();
            if (selectedKnowledgeId == entry.Id) selectedKnowledgeEntry = entry with { Enabled = enabled };
            if (selectedEngineFamily is { } family) RenderLibraryKnowledge(family.Data.Family, false);
            ShowLibraryStatus("已保存经验启用状态，对下一次智能识别或继续适配生效。", InfoBarSeverity.Success);
        }
        catch (Exception error)
        {
            if (directory != dataDirectory) return;
            if (selectedKnowledgeId == entry.Id) CatalogKnowledgeEnabledBox.IsChecked = entry.Enabled;
            ShowLibraryStatus("保存经验状态失败：" + error.Message, InfoBarSeverity.Error);
        }
        finally
        {
            engineKnowledgeSaving = false;
            if (directory == dataDirectory) CatalogKnowledgeEnabledBox.IsEnabled = selectedKnowledgeEntry is not null;
            UpdateControls();
            UpdateLibraryActions();
        }
    }

    private sealed class EngineKnowledgeRow(EngineKnowledgeSummary data)
    {
        public EngineKnowledgeSummary Data { get; } = data;
        public string Summary => Data.Summary;
        public string StatusLabel => (Data.Enabled ? "已启用" : "已停用") + $" · 辅助工具 {Data.ToolCount:N0} 个";
    }
}
