using System.Runtime.CompilerServices;
using System.Text.Json;
using GameL10n.Core;
using GameL10n.Core.Pipeline;
using GameL10n.Core.Storage;
using GameL10n.Core.Translation;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;

namespace GameL10n.Desktop;

public sealed partial class MainWindow
{
    private const int TextPageSize = 80;
    private readonly ConditionalWeakTable<GameWorkspace, ProjectWorkbenchViewState> workbenchViews = new();
    private GameWorkspace? displayedWorkbench;
    private TranslationRow[]? displayedRows;
    private TranslationRow[] filteredTexts = [];
    private TranslationRow? editingRow;
    private int textPage;
    private int filterRevision;
    private bool renderingTexts;
    private bool renderingEditor;
    private int textPageRenderRevision;
    private bool textScrollRestorePending;
    private DispatcherQueueTimer? textFilterTimer;
    private DispatcherQueueTimer? editSaveTimer;
    private CancellationTokenSource? textFiltering;

    private void InitializeWorkbench()
    {
        textFilterTimer = DispatcherQueue.CreateTimer();
        textFilterTimer.Interval = TimeSpan.FromMilliseconds(180);
        textFilterTimer.IsRepeating = false;
        textFilterTimer.Tick += async (_, _) => await RefreshTextFilterAsync();
        editSaveTimer = DispatcherQueue.CreateTimer();
        editSaveTimer.Interval = TimeSpan.FromMilliseconds(500);
        editSaveTimer.IsRepeating = false;
        editSaveTimer.Tick += async (_, _) =>
        {
            try { await FlushAllEditsAsync(); }
            catch (Exception error) { EditorSaveText.Text = "未保存：" + error.Message + "。请点击保存重试。"; }
        };
        AddShortcut(Windows.System.VirtualKey.S, Windows.System.VirtualKeyModifiers.Control, async () =>
        { try { await FlushAllEditsAsync(); } catch (Exception error) { EditorSaveText.Text = "未保存：" + error.Message; } });
        AddShortcut(Windows.System.VirtualKey.K, Windows.System.VirtualKeyModifiers.Control,
            () => { ProjectTabs.SelectedItem = WorkbenchTab; TextSearchBox.Focus(FocusState.Programmatic); });
        AddShortcut(Windows.System.VirtualKey.Enter, Windows.System.VirtualKeyModifiers.Control,
            () => ConfirmReview_Click(this, new RoutedEventArgs()));
        AddShortcut(Windows.System.VirtualKey.Up, Windows.System.VirtualKeyModifiers.Menu, () => NavigateText(-1));
        AddShortcut(Windows.System.VirtualKey.Down, Windows.System.VirtualKeyModifiers.Menu, () => NavigateText(1));
    }

    private void AddShortcut(Windows.System.VirtualKey key, Windows.System.VirtualKeyModifiers modifiers, Action action)
    {
        var shortcut = new KeyboardAccelerator { Key = key, Modifiers = modifiers };
        shortcut.Invoked += (_, args) =>
        {
            if (globalPage is not null || selectedGame is null || closing || switchingGames) return;
            action(); args.Handled = true;
        };
        RootGrid.KeyboardAccelerators.Add(shortcut);
    }

    private static string WorkbenchPolicy(GameWorkspace game) => JsonSerializer.Serialize(new
    { game.Project.Settings.TargetLanguage, game.Project.Settings.Style, game.Project.Settings.Terminology });

    private async Task EnsureWorkbenchAsync(GameWorkspace game)
    {
        while (game.Session is { } captured && !closing && !changingDataDirectory && !game.IsBeingRemoved && games.Contains(game))
        {
            var policyKey = WorkbenchPolicy(game);
            if (game.Rows is not null && ReferenceEquals(game.RowsSession, captured) && game.RowsPolicy == policyKey) return;
            if (game.Busy && game.Kind == GameTaskKind.Translation && game.Rows is not null && game.RowsPolicy != policyKey) return;
            if (game.LoadingRows is { } pending)
            {
                await pending;
                if (ReferenceEquals(game.LoadingRows, pending)) game.LoadingRows = null;
                continue;
            }
            var policy = ReadPolicy(game.Project.Settings);
            var memoryDirectory = Path.Combine(dataDirectory, "memory", game.Id);
            var loading = LoadAsync();
            game.LoadingRows = loading;
            if (selectedGame == game) UpdateWorkbenchControls();
            try { await loading; }
            finally
            {
                if (ReferenceEquals(game.LoadingRows, loading)) game.LoadingRows = null;
                TrimProjectContentCache();
                if (selectedGame == game) UpdateWorkbenchControls();
            }

            bool IsCurrent() => !closing && !changingDataDirectory && !game.IsBeingRemoved && games.Contains(game)
                && ReferenceEquals(game.Session, captured) && policyKey == WorkbenchPolicy(game);

            async Task LoadAsync()
            {
                if (selectedGame == game && game.Rows is null)
                {
                    WorkbenchEmptyText.Text = "正在读取已保存的译文与校对记录…";
                    WorkbenchEmptyText.Visibility = Visibility.Visible;
                }
                await FlushEditsAsync(game);
                if (!IsCurrent()) return;
                var editRevision = game.EditRevision;
                var prepared = await Task.Run(async () =>
                {
                    var memory = new TranslationMemory(Path.Combine(game.Directory, "memory"), memoryDirectory);
                    await memory.InitializeAsync();
                    var edits = await TranslationEdits.LoadAsync(game.Directory);
                    var failures = await pipeline.LoadTranslationFailuresAsync(captured, policy);
                    var selection = await LocalFiles.ReadRecoverableJsonAsync<string[]>(Path.Combine(game.Directory, "trial-selection.json"));
                    var rows = new TranslationRow[captured.Extraction.Units.Count];
                    for (var i = 0; i < rows.Length; i++)
                    {
                        var unit = captured.Extraction.Units[i];
                        var row = new TranslationRow(unit, i + 1, captured.Adapter)
                        {
                            ModelText = await memory.FindAsync(unit, policy),
                            TranslationFailure = failures.GetValueOrDefault(unit.Id),
                            Edit = edits.Find(unit, policy.TargetLanguage),
                            PreviousEdit = edits.Entries.GetValueOrDefault(unit.Id)
                        };
                        row.Refresh(); rows[i] = row;
                    }
                    return (Rows: rows, RowsById: rows.ToDictionary(row => row.Unit.Id, StringComparer.Ordinal),
                        Edits: new Dictionary<string, TranslationEdit>(edits.Entries, StringComparer.Ordinal),
                        Candidates: new Dictionary<string, TranslationEdit>(edits.Candidates ?? new Dictionary<string, TranslationEdit>(), StringComparer.Ordinal),
                        Selection: selection);
                });
                if (!IsCurrent()) return;
                await LoadPatchIndexAsync(game);
                if (!IsCurrent() || editRevision != game.EditRevision) return;

                InvalidateWorkbenchCache(game);
                game.Rows = prepared.Rows;
                game.RowsById = prepared.RowsById;
                game.Edits = prepared.Edits;
                game.Candidates = prepared.Candidates;
                game.RowsSession = captured;
                game.RowsPolicy = policyKey;
                if (game.ActiveTextId is null)
                {
                    var recent = prepared.Rows.Where(row => row.Edit is not null).MaxBy(row => row.Edit!.UpdatedAt);
                    if (recent is not null)
                        game.ActiveTextId = recent.Reviewed && recent.Number < prepared.Rows.Length
                            ? prepared.Rows[recent.Number].Unit.Id : recent.Unit.Id;
                }
                if (game.SelectedTexts.Count == 0 && !game.SelectionDirty)
                {
                    foreach (var id in prepared.Selection ?? []) if (game.RowsById.ContainsKey(id)) game.SelectedTexts.Add(id);
                }
                game.SelectedTexts.IntersectWith(game.RowsById.Keys);
                foreach (var row in game.Rows) row.Selected = game.SelectedTexts.Contains(row.Unit.Id);
                RefreshProjectCacheSize(game);
                if (selectedGame == game) RenderWorkbenchSurface();
            }
        }
    }

    private async void ProjectTabs_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (!initialized || renderingTexts || !e.AddedItems.OfType<TabViewItem>().Any() || selectedGame is not { } game) return;
        game.ProjectTab = ProjectTabs.SelectedIndex;
        if (!ReferenceEquals(ProjectTabs.SelectedItem, WorkbenchTab) && !ReferenceEquals(ProjectTabs.SelectedItem, PatchTab)) return;
        try { await EnsureWorkbenchAsync(game); }
        catch (Exception error)
        {
            if (selectedGame == game)
            {
                WorkbenchEmptyText.Text = "译文加载失败：" + error.Message;
                WorkbenchEmptyText.Visibility = Visibility.Visible;
            }
            Program.WriteStartupError(error);
        }
    }

    private void OpenWorkbench_Click(object sender, RoutedEventArgs e) => ProjectTabs.SelectedItem = WorkbenchTab;

    private void RenderWorkbenchSurface()
    {
        if (TextRowsList is null) return;
        var game = selectedGame;
        var policy = game is null ? null : WorkbenchPolicy(game);
        var changed = displayedWorkbench != game || !ReferenceEquals(displayedRows, game?.Rows)
            || (game is not null && WorkbenchView(game).Policy != policy);
        if (changed)
        {
            CaptureWorkbenchView();
            textFilterTimer?.Stop();
            filterRevision++;
            textFiltering?.Cancel();
            textPageRenderRevision++;
            textScrollRestorePending = false;
            displayedWorkbench = game; displayedRows = game?.Rows;
            var view = game is null ? null : WorkbenchView(game);
            if (view is not null && (!ReferenceEquals(view.Rows, game!.Rows) || view.Policy != policy))
            {
                view.ForgetRows();
                view.Rows = game.Rows;
                view.Policy = policy;
                view.ResourcePaths = game.Rows?.Select(row => row.Unit.ResourcePath).Distinct(StringComparer.Ordinal).ToArray();
            }
            filteredTexts = [];
            textPage = view?.Page ?? 0;
            renderingTexts = true;
            try
            {
                ProjectTabs.SelectedIndex = game?.ProjectTab ?? 0;
                TextSearchBox.Text = view?.SearchText ?? "";
                ResourceFilterBox.Items.Clear();
                ResourceFilterBox.Items.Add(new ComboBoxItem { Content = "全部脚本" });
                foreach (var path in view?.ResourcePaths ?? [])
                    ResourceFilterBox.Items.Add(new ComboBoxItem { Content = path, Tag = path });
                if (view?.ResourcePath is { } savedPath && !(view.ResourcePaths?.Contains(savedPath, StringComparer.Ordinal) ?? false))
                    ResourceFilterBox.Items.Add(new ComboBoxItem { Content = savedPath, Tag = savedPath });
                SelectWorkbenchFilter(ResourceFilterBox, view?.ResourcePath);
                SelectWorkbenchFilter(StatusFilterBox, view?.Status ?? "all");
                SelectWorkbenchFilter(KindFilterBox, view?.Kind ?? "all");
                OnlySelectedTextsBox.IsChecked = view?.OnlySelected ?? false;
            }
            finally { renderingTexts = false; }
            editingRow = game?.ActiveTextId is { } id ? game.RowsById.GetValueOrDefault(id) : null;
            RenderEditor();
            if (game?.Rows is { } rows && view is not null)
            {
                if (HasCurrentTextFilter(view, rows, policy!)) RestoreWorkbenchFilter(view);
                else
                {
                    TextRowsList.ItemsSource = null;
                    WorkbenchEmptyText.Text = "正在筛选文本…";
                    WorkbenchEmptyText.Visibility = Visibility.Visible;
                    _ = RefreshTextFilterAsync();
                }
            }
            else TextRowsList.ItemsSource = null;
        }
        if (game?.Rows is null) RenderWorkbenchEmpty(game);
        UpdateWorkbenchControls();
        UpdatePatchNotice();
    }

    private ProjectWorkbenchViewState WorkbenchView(GameWorkspace game) => workbenchViews.GetValue(game, static _ => new());

    private static void SelectWorkbenchFilter(ComboBox box, string? value)
    {
        box.SelectedItem = box.Items.OfType<ComboBoxItem>().FirstOrDefault(item => item.Tag as string == value) ?? box.Items[0];
    }

    private void CaptureWorkbenchView(bool captureScroll = true)
    {
        if (displayedWorkbench is not { } game) return;
        var view = WorkbenchView(game);
        view.SearchText = TextSearchBox.Text;
        view.ResourcePath = (ResourceFilterBox.SelectedItem as ComboBoxItem)?.Tag as string;
        view.Kind = (KindFilterBox.SelectedItem as ComboBoxItem)?.Tag as string ?? "all";
        view.Status = (StatusFilterBox.SelectedItem as ComboBoxItem)?.Tag as string ?? "all";
        view.OnlySelected = OnlySelectedTextsBox.IsChecked == true;
        view.Page = textPage;
        if (captureScroll && !textScrollRestorePending && FindTextScrollViewer(TextRowsList) is { } scroll) view.ScrollOffset = scroll.VerticalOffset;
    }

    private void InvalidateWorkbenchCache(GameWorkspace game)
    {
        var view = WorkbenchView(game);
        view.ContentRevision++;
        view.FilteredRevision = view.SummaryRevision = -1;
        if (displayedWorkbench != game) return;
        filterRevision++;
        textFiltering?.Cancel();
    }

    private void ForgetWorkbenchCache(GameWorkspace game)
    {
        if (displayedWorkbench == game)
        {
            CaptureWorkbenchView();
            textFilterTimer?.Stop();
            textFiltering?.Cancel();
            filterRevision++;
            textPageRenderRevision++;
            textScrollRestorePending = false;
            displayedRows = null;
            filteredTexts = [];
            editingRow = null;
            TextRowsList.ItemsSource = null;
        }
        if (workbenchViews.TryGetValue(game, out var view)) view.ForgetRows();
    }

    private void RenderWorkbenchEmpty(GameWorkspace? game)
    {
        WorkbenchEmptyText.Text = game?.ContentLoading == true ? "正在读取已保存的项目数据…"
            : game?.ContentError is { } error ? "保存的项目数据无法读取：" + error
            : game?.LoadingRows is not null || game?.Session is not null ? "正在读取已保存的译文与校对记录…"
            : game?.HasSavedAnalysis == true ? "尚未载入已保存的项目数据。"
            : "请点击“常规识别”或“智能适配”开始识别游戏，再查看文本。";
        WorkbenchEmptyText.Visibility = Visibility.Visible;
        WorkbenchSummaryText.Text = game?.ContentLoading == true ? "正在读取保存的项目文本。" : "识别完成后显示项目文本。";
        TextsPageText.Text = "";
        PreviousTextsButton.IsEnabled = NextTextsButton.IsEnabled = false;
    }

    private static bool HasCurrentTextFilter(ProjectWorkbenchViewState view, TranslationRow[] rows, string policy) =>
        ReferenceEquals(view.Rows, rows) && view.Policy == policy && view.FilteredRows is not null
        && view.FilteredRevision == view.ContentRevision && view.FilteredOptions == view.Filter;

    private void RestoreWorkbenchFilter(ProjectWorkbenchViewState view)
    {
        filteredTexts = view.FilteredRows!;
        textPage = Math.Clamp(view.Page, 0, Math.Max(0, (filteredTexts.Length - 1) / TextPageSize));
        RenderTextPage(restoreScroll: true);
        UpdateWorkbenchSummary();
    }

    private void UpdateWorkbenchControls()
    {
        var game = selectedGame;
        var canUse = initialized && !closing && !switchingGames && !changingDataDirectory && !removingGames;
        var ready = canUse && game?.Rows is not null;
        var canEdit = ready && game?.LoadingRows is null && game?.RowsPolicy == WorkbenchPolicy(game!) && editingRow is not null && ReferenceEquals(game?.RowsSession, game?.Session)
            && game?.Retranslating != true && (!busy || game?.Kind == GameTaskKind.Translation || game?.Kind == GameTaskKind.Patching);
        TranslationEditorBox.IsEnabled = SaveEditButton.IsEnabled = canEdit;
        CandidateButton.IsEnabled = canEdit && !busy;
        CandidateButton.Content = editingRow is { } active && game?.Candidates.GetValueOrDefault(active.Unit.Id) is { } candidate
            && candidate.SourceSignature == active.Signature && candidate.Language == game.Project.Settings.TargetLanguage ? "查看候选译文" : "重新翻译并对比";
        ConfirmReviewButton.IsEnabled = canEdit && editingRow?.Text is not null && editingRow.Issues.Count == 0;
        BulkReviewMenuItem.IsEnabled = ready && !game!.Retranslating && game.SelectedTexts.Count > 0;
        MarkUnreviewedButton.Visibility = editingRow?.Reviewed == true ? Visibility.Visible : Visibility.Collapsed;
        MarkUnreviewedButton.IsEnabled = canEdit;
        ModelDraftExpander.IsEnabled = editingRow?.ModelText is not null;
        ShowPatchTextsButton.IsEnabled = canUse && game?.PatchTextHashes is not null;
        GeneratePatchButton.IsEnabled = BuildSavedPatchButton.IsEnabled = canUse && session is not null && !busy;
        RecordRuntimePassedButton.IsEnabled = RecordRuntimeFailedButton.IsEnabled = canUse && game?.Result is not null && !busy;
        TrialButton.IsEnabled = TrialPatchButton.IsEnabled = ready && !busy && game!.SelectedTexts.Count > 0;
        RetranslateSelectedButton.IsEnabled = TrialButton.IsEnabled;
        OpenWorkbenchButton.IsEnabled = canUse && session is not null;
        TranslateButton.Content = game?.HasPendingTranslation == true ? "继续上次翻译" : "开始全量翻译";
    }

    private void TextSearch_TextChanged(object sender, TextChangedEventArgs e) => WorkbenchFilterChanged();
    private void TextFilter_Changed(object sender, SelectionChangedEventArgs e) => WorkbenchFilterChanged();
    private void OnlySelectedTexts_Click(object sender, RoutedEventArgs e) => WorkbenchFilterChanged();
    private void WorkbenchFilterChanged()
    {
        if (renderingTexts) return;
        textPage = 0;
        if (displayedWorkbench is { } game)
        {
            var view = WorkbenchView(game);
            view.Page = 0;
            view.ScrollOffset = 0;
            view.RevealActiveText = false;
        }
        QueueTextFilter(captureScroll: false);
    }
    private void QueueTextFilter(bool captureScroll = true)
    {
        if (!initialized || renderingTexts || textFilterTimer is null) return;
        CaptureWorkbenchView(captureScroll);
        filterRevision++;
        textFiltering?.Cancel();
        textFilterTimer.Stop(); textFilterTimer.Start();
    }

    private async Task RefreshTextFilterAsync()
    {
        textFilterTimer?.Stop();
        var game = selectedGame;
        if (game?.Rows is not { } rows || displayedWorkbench != game) return;
        var view = WorkbenchView(game);
        var policy = WorkbenchPolicy(game);
        var filter = view.Filter;
        if (HasCurrentTextFilter(view, rows, policy))
        {
            RestoreWorkbenchFilter(view);
            return;
        }
        var contentRevision = view.ContentRevision;
        var revision = ++filterRevision;
        textFiltering?.Cancel();
        using var cancellation = new CancellationTokenSource(); textFiltering = cancellation;
        var selected = filter.OnlySelected ? game.SelectedTexts.ToHashSet(StringComparer.Ordinal) : null;
        try
        {
            var matches = filter.MatchesAll ? rows : await Task.Run(() => rows.Where(row =>
            {
                cancellation.Token.ThrowIfCancellationRequested();
                if (filter.ResourcePath is not null && row.Unit.ResourcePath != filter.ResourcePath) return false;
                if (filter.Kind != "all" && row.Unit.Kind != filter.Kind) return false;
                if (selected is not null && !selected.Contains(row.Unit.Id)) return false;
                if (!(filter.Status switch
                {
                    "missing" => row.Text is null, "failed" => row.Failure is not null, "pending" => row.Text is not null && !row.Reviewed,
                    "edited" => row.Edit is { IsGenerated: false }, "reviewed" => row.Reviewed, "invalid" => row.Issues.Count > 0, _ => true
                })) return false;
                return filter.Query.Length == 0 || row.Source.Contains(filter.Query, StringComparison.OrdinalIgnoreCase)
                    || row.Text?.Contains(filter.Query, StringComparison.OrdinalIgnoreCase) == true
                    || row.Unit.Speaker?.Contains(filter.Query, StringComparison.OrdinalIgnoreCase) == true
                    || row.Unit.ResourcePath.Contains(filter.Query, StringComparison.OrdinalIgnoreCase)
                    || row.Unit.Locator.Contains(filter.Query, StringComparison.OrdinalIgnoreCase);
            }).ToArray(), cancellation.Token);
            if (revision != filterRevision || selectedGame != game || !ReferenceEquals(game.Rows, rows)
                || policy != WorkbenchPolicy(game) || contentRevision != view.ContentRevision || filter != view.Filter) return;
            filteredTexts = matches;
            view.Rows = rows;
            view.Policy = policy;
            view.FilteredRows = matches;
            view.FilteredOptions = filter;
            view.FilteredRevision = contentRevision;
            if (view.RevealActiveText)
            {
                var activeIndex = editingRow is null ? -1 : Array.IndexOf(matches, editingRow);
                if (activeIndex >= 0) textPage = activeIndex / TextPageSize;
                view.RevealActiveText = false;
            }
            var page = Math.Clamp(textPage, 0, Math.Max(0, (matches.Length - 1) / TextPageSize));
            if (textPage != page) view.ScrollOffset = 0;
            textPage = page;
            RenderTextPage(restoreScroll: true);
            // Keep the active draft open even when an edit moves it outside the selected filter.
            if (editingRow is null && matches.Length > 0) ActivateText(matches[0]);
            UpdateWorkbenchSummary();
        }
        catch (OperationCanceledException) { }
        catch (Exception error)
        {
            if (revision == filterRevision && selectedGame == game)
            {
                WorkbenchEmptyText.Text = "筛选失败：" + error.Message;
                WorkbenchEmptyText.Visibility = Visibility.Visible;
            }
        }
        finally { if (ReferenceEquals(textFiltering, cancellation)) textFiltering = null; }
    }

    private void RenderTextPage(bool restoreScroll = false)
    {
        var page = filteredTexts.Skip(textPage * TextPageSize).Take(TextPageSize).ToArray();
        renderingTexts = true;
        try { TextRowsList.ItemsSource = page; if (editingRow is not null && page.Contains(editingRow)) TextRowsList.SelectedItem = editingRow; }
        finally { renderingTexts = false; }
        WorkbenchEmptyText.Visibility = filteredTexts.Length == 0 ? Visibility.Visible : Visibility.Collapsed;
        WorkbenchEmptyText.Text = "没有匹配的文本，试试其他脚本、状态或关键词。";
        TextsPageText.Text = $"{textPage + 1} / {Math.Max(1, (filteredTexts.Length + TextPageSize - 1) / TextPageSize)} 页 · {filteredTexts.Length:N0} 条";
        PreviousTextsButton.IsEnabled = textPage > 0;
        NextTextsButton.IsEnabled = (textPage + 1) * TextPageSize < filteredTexts.Length;
        var renderRevision = ++textPageRenderRevision;
        textScrollRestorePending = false;
        if (selectedGame is not { } game) return;
        var view = WorkbenchView(game);
        view.Page = textPage;
        if (!restoreScroll) return;
        textScrollRestorePending = true;
        DispatcherQueue.TryEnqueue(DispatcherQueuePriority.Low, () =>
        {
            if (renderRevision != textPageRenderRevision || displayedWorkbench != game || selectedGame != game) return;
            TextRowsList.UpdateLayout();
            FindTextScrollViewer(TextRowsList)?.ChangeView(null, view.ScrollOffset, null, disableAnimation: true);
            textScrollRestorePending = false;
        });
    }

    private static ScrollViewer? FindTextScrollViewer(DependencyObject parent)
    {
        for (var i = 0; i < VisualTreeHelper.GetChildrenCount(parent); i++)
        {
            var child = VisualTreeHelper.GetChild(parent, i);
            if (child is ScrollViewer scroll) return scroll;
            if (FindTextScrollViewer(child) is { } nested) return nested;
        }
        return null;
    }

    private void UpdateWorkbenchSummary()
    {
        if (selectedGame is not { Rows: { } rows } game) return;
        var view = WorkbenchView(game);
        if (view.SummaryRevision != view.ContentRevision || view.Summary is null)
        {
            view.Summary = $"{rows.Length:N0} 条 · 已译 {rows.Count(row => row.Text is not null):N0} · 已校对 {rows.Count(row => row.Reviewed):N0} · 问题 {rows.Count(row => row.Issues.Count > 0):N0}";
            view.SummaryRevision = view.ContentRevision;
        }
        WorkbenchSummaryText.Text = $"{view.Summary} · 已选 {game.SelectedTexts.Count:N0}";
        UpdateWorkbenchControls();
    }

    private void PreviousTexts_Click(object sender, RoutedEventArgs e) { if (textPage > 0) { textPage--; ResetTextPageScroll(); RenderTextPage(restoreScroll: true); } }
    private void NextTexts_Click(object sender, RoutedEventArgs e) { if ((textPage + 1) * TextPageSize < filteredTexts.Length) { textPage++; ResetTextPageScroll(); RenderTextPage(restoreScroll: true); } }
    private void ResetTextPageScroll() { if (selectedGame is { } game) WorkbenchView(game).ScrollOffset = 0; }
    private void TextRowCheck_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not CheckBox { Tag: TranslationRow row } check || selectedGame is not { } game) return;
        row.Selected = check.IsChecked == true;
        if (row.Selected) game.SelectedTexts.Add(row.Unit.Id); else game.SelectedTexts.Remove(row.Unit.Id);
        SaveSelectionLater(game);
        UpdateWorkbenchSummary(); if (OnlySelectedTextsBox.IsChecked == true) QueueTextFilter();
    }
    private void SelectPageTexts_Click(object sender, RoutedEventArgs e)
    {
        if (selectedGame is not { } game) return;
        foreach (var row in filteredTexts.Skip(textPage * TextPageSize).Take(TextPageSize))
        { game.SelectedTexts.Add(row.Unit.Id); row.Selected = true; row.Refresh(); }
        SaveSelectionLater(game);
        UpdateWorkbenchSummary();
    }
    private void SelectMatchingTexts_Click(object sender, RoutedEventArgs e)
    {
        if (selectedGame is not { } game) return;
        foreach (var row in filteredTexts) { game.SelectedTexts.Add(row.Unit.Id); row.Selected = true; }
        CaptureWorkbenchView(); RenderTextPage(restoreScroll: true); SaveSelectionLater(game); UpdateWorkbenchSummary();
    }

    private async void BulkReview_Click(object sender, RoutedEventArgs e)
    {
        if (selectedGame is not { Rows: { } rows, Retranslating: false } game) return;
        var captured = game.Session;
        var valid = rows.Where(row => game.SelectedTexts.Contains(row.Unit.Id) && row.Text is not null && row.Issues.Count == 0 && !row.Reviewed).ToArray();
        if (valid.Length == 0) return;
        try
        {
            var answer = await new ContentDialog
            {
                XamlRoot = RootGrid.XamlRoot, Title = $"确认 {valid.Length:N0} 条译文已校对？",
                Content = $"请确认已经检查过这些译文的含义和表达。未翻译、有格式问题或已校对的 {game.SelectedTexts.Count - valid.Length:N0} 条文本会跳过。",
                PrimaryButtonText = "确认已校对", CloseButtonText = "取消"
            }.ShowAsync();
            if (answer != ContentDialogResult.Primary || selectedGame != game || game.Retranslating || !ReferenceEquals(game.Session, captured)) return;
            foreach (var row in valid) SetEdit(game, row, row.Text!, true, refresh: false);
            await FlushEditsAsync(game); RenderEditorValidation(); UpdateWorkbenchSummary(); QueueTextFilter();
        }
        catch (Exception error) { EditorSaveText.Text = "批量校对未保存：" + error.Message; }
    }

    private async void MarkUnreviewed_Click(object sender, RoutedEventArgs e)
    {
        if (selectedGame is not { } game || editingRow is not { Text: { } text } row) return;
        try { SetEdit(game, row, text, false); await FlushEditsAsync(game); }
        catch (Exception error) { EditorSaveText.Text = "校对状态未保存：" + error.Message; }
    }
    private void ClearSelectedTexts_Click(object sender, RoutedEventArgs e)
    {
        if (selectedGame is not { } game) return;
        foreach (var id in game.SelectedTexts)
            if (game.RowsById.TryGetValue(id, out var row)) { row.Selected = false; row.Refresh(); }
        game.SelectedTexts.Clear(); SaveSelectionLater(game); UpdateWorkbenchSummary(); QueueTextFilter();
    }

    private void SaveSelectionLater(GameWorkspace game)
    {
        game.SelectionDirty = true; game.SelectionRevision++;
        if (WorkbenchView(game).OnlySelected) InvalidateWorkbenchCache(game);
        editSaveTimer?.Stop(); editSaveTimer?.Start();
    }

    private void TextRows_SelectionChanged(object sender, SelectionChangedEventArgs e)
    { if (!renderingTexts && TextRowsList.SelectedItem is TranslationRow row) ActivateText(row); }
    private void ActivateText(TranslationRow row)
    {
        editingRow = row;
        if (selectedGame is { } game) game.ActiveTextId = row.Unit.Id;
        renderingTexts = true;
        try { if (TextRowsList.Items.Contains(row)) TextRowsList.SelectedItem = row; }
        finally { renderingTexts = false; }
        RenderEditor(); UpdateWorkbenchControls();
    }

    private void RenderEditor()
    {
        renderingEditor = true;
        try
        {
            var row = editingRow;
            EditorLocationText.Text = row is null ? "选择一条文本开始校对" : $"{row.Unit.ResourcePath}\n位置：{row.Unit.Locator} · {row.Kind}";
            EditorSourceText.Text = row?.Source ?? "";
            TranslationEditorBox.Text = row?.Text ?? "";
            ModelDraftText.Text = row?.ModelText ?? "没有模型初稿。";
            ModelDraftExpander.IsExpanded = false;
            EditorContextText.Text = "";
            if (row is not null && selectedGame?.Rows is { } rows)
            {
                var before = row.Number > 1 && rows[row.Number - 2].Unit.ResourcePath == row.Unit.ResourcePath ? rows[row.Number - 2].Source : "（本资源首条文本）";
                var after = row.Number < rows.Length && rows[row.Number].Unit.ResourcePath == row.Unit.ResourcePath ? rows[row.Number].Source : "（本资源末条文本）";
                EditorContextText.Text = $"上一条：{before}\n\n下一条：{after}" + (row.Unit.Context.Length > 0 ? "\n\n提取上下文：" + row.Unit.Context : "");
            }
            PreviousEditText.Visibility = row is { PreviousEdit: not null, Edit: null } ? Visibility.Visible : Visibility.Collapsed;
            PreviousEditText.Text = row?.PreviousEdit is { } old ? "原文、上下文或目标语言已变化，旧校对未自动采用：\n" + old.Text : "";
            EditorTermsPanel.Children.Clear();
            if (row is not null && selectedGame is { } game)
                foreach (var line in game.Project.Settings.Terminology.Split('\n', StringSplitOptions.TrimEntries | StringSplitOptions.RemoveEmptyEntries))
                {
                    var parts = line.Split('=', 2, StringSplitOptions.TrimEntries);
                    if (parts.Length != 2 || parts[0].Length == 0 || !new[] { row.Source, row.Unit.Context, row.Unit.Speaker ?? "" }.Any(text => text.Contains(parts[0], StringComparison.Ordinal))) continue;
                    var button = new Button { Content = parts[0] + " → " + parts[1], Tag = parts[1], HorizontalAlignment = HorizontalAlignment.Left };
                    button.Click += (_, _) => { if (TranslationEditorBox.IsEnabled) { TranslationEditorBox.SelectedText = (string)button.Tag; TranslationEditorBox.Focus(FocusState.Programmatic); } };
                    EditorTermsPanel.Children.Add(button);
                }
            EditorTermsEmptyText.Visibility = EditorTermsPanel.Children.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
            RenderEditorValidation();
        }
        finally { renderingEditor = false; }
    }

    private void RenderEditorValidation()
    {
        var row = editingRow;
        EditorSaveText.Text = selectedGame?.EditSaveError is { } saveError ? "未保存：" + saveError + "。请点击保存重试。"
            : selectedGame?.EditsDirty == true ? "等待保存…" : row?.Edit is { IsGenerated: true } ? "已保存 · 重新翻译结果，等待校对" : row?.Edit is not null
            ? (row.Reviewed ? "已校对 · 手工译文受保护" : "已保存 · 手工译文受保护") : row?.Text is null ? "尚未翻译" : "已保存 · 等待校对";
        EditorValidationInfo.IsOpen = row?.Text is not null || row?.Failure is not null;
        EditorValidationInfo.Severity = row?.Failure is not null || row?.Issues.Count > 0 ? InfoBarSeverity.Warning : InfoBarSeverity.Success;
        EditorValidationInfo.Title = row?.Failure is not null ? "翻译失败" : row?.Issues.Count > 0 ? "格式需要修正" : "文本格式检查通过";
        EditorValidationInfo.Message = row?.Failure ?? (row?.Issues.Count > 0 ? string.Join("；", row.Issues.Select(issue => issue.Message)) : "");
    }

    private void TranslationEditor_TextChanged(object sender, TextChangedEventArgs e)
    {
        if (renderingEditor || editingRow is not { } row || selectedGame is not { } game || !TranslationEditorBox.IsEnabled
            || TranslationEditorBox.Text == (row.Text ?? "")) return;
        SetEdit(game, row, TranslationEditorBox.Text, false);
        editSaveTimer?.Stop(); editSaveTimer?.Start();
    }

    private void SetEdit(GameWorkspace game, TranslationRow row, string text, bool reviewed, bool refresh = true)
    {
        if (game.Retranslating) return;
        var edit = new TranslationEdit(row.Signature, game.Project.Settings.TargetLanguage, text, reviewed, DateTimeOffset.UtcNow, row.Edit?.RetranslationId);
        game.Edits[row.Unit.Id] = edit; row.Edit = edit; row.Refresh();
        game.EditsDirty = true; game.EditRevision++; game.EditSaveError = null;
        InvalidateWorkbenchCache(game);
        if (refresh) { RenderEditorValidation(); UpdateWorkbenchSummary(); UpdatePatchNotice(); QueueTextFilter(); }
    }

    private async Task FlushEditsAsync(GameWorkspace game)
    {
        try
        {
            if (game.EditsDirty)
            {
                var revision = game.EditRevision;
                var snapshot = new TranslationEdits(1, new Dictionary<string, TranslationEdit>(game.Edits, StringComparer.Ordinal),
                    new Dictionary<string, TranslationEdit>(game.Candidates, StringComparer.Ordinal));
                await game.EnqueueWrite(() => Task.Run(() => snapshot.SaveAsync(game.Directory)));
                if (game.EditRevision == revision) { game.EditsDirty = false; game.EditSaveError = null; }
            }
            if (game.SelectionDirty)
            {
                var revision = game.SelectionRevision;
                var ids = game.SelectedTexts.ToArray();
                await game.EnqueueWrite(() => LocalFiles.WriteJsonAsync(Path.Combine(game.Directory, "trial-selection.json"), ids));
                if (game.SelectionRevision == revision) game.SelectionDirty = false;
            }
        }
        catch (Exception error) { game.EditSaveError = error.Message; throw; }
        if (selectedGame == game) RenderEditorValidation();
    }
    private async Task FlushAllEditsAsync()
    { foreach (var game in games.Where(game => game.EditsDirty || game.SelectionDirty).ToArray()) await FlushEditsAsync(game); }
    private async void SaveEdit_Click(object sender, RoutedEventArgs e)
    {
        try { await FlushAllEditsAsync(); }
        catch (Exception error) { EditorSaveText.Text = "未保存：" + error.Message + "。译文仍保留在编辑区，可重试。"; }
    }
    private async void ConfirmReview_Click(object sender, RoutedEventArgs e)
    {
        if (!ConfirmReviewButton.IsEnabled || editingRow is not { Text: { } text } row || selectedGame is not { } game) return;
        try
        {
            SetEdit(game, row, text, true); await FlushEditsAsync(game);
            if (selectedGame == game && editingRow == row) NavigateText(1);
        }
        catch (Exception error) { EditorSaveText.Text = "校对未保存：" + error.Message; }
    }
    private void NavigateText(int direction)
    {
        if (filteredTexts.Length == 0) return;
        var index = editingRow is null ? -1 : Array.IndexOf(filteredTexts, editingRow);
        if (index < 0 && editingRow is { } current)
            index = direction > 0 ? Array.FindIndex(filteredTexts, row => row.Number > current.Number) - 1 : Array.FindLastIndex(filteredTexts, row => row.Number < current.Number) + 1;
        var next = Math.Clamp(index + direction, 0, filteredTexts.Length - 1);
        ActivateText(filteredTexts[next]); textPage = next / TextPageSize; RenderTextPage();
        TextRowsList.ScrollIntoView(editingRow);
    }
    private void PreviousText_Click(object sender, RoutedEventArgs e) => NavigateText(-1);
    private void NextText_Click(object sender, RoutedEventArgs e) => NavigateText(1);

    private void WorkbenchLayout_SizeChanged(object sender, SizeChangedEventArgs e)
    {
        if (EditorPanel is null) return;
        var narrow = e.NewSize.Width < 680;
        EditorColumn.Width = new GridLength(narrow ? 0 : Math.Clamp(e.NewSize.Width * .4, 300, 460));
        EditorRow.Height = narrow ? new GridLength(1.15, GridUnitType.Star) : new GridLength(0);
        Grid.SetColumn(EditorPanel, narrow ? 0 : 1); Grid.SetRow(EditorPanel, narrow ? 1 : 0);
    }
}
