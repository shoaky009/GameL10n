using System.Diagnostics;
using AvgTranslator.Core.Adapters;
using AvgTranslator.Core.Development;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace AvgTranslator.Desktop;

public sealed partial class MainWindow
{
    private IReadOnlyList<EngineKnowledgeSummary> engineKnowledgeSummaries = [];
    private IReadOnlyList<EngineFamilyRow> engineFamilyRows = [];
    private EngineFamilyRow? selectedEngineFamily;
    private Task? engineLibraryLoad;
    private bool engineLibraryDirty = true;
    private bool engineLibraryUpdating;
    private bool engineLibrarySaving;
    private bool engineKnowledgeSaving;
    private bool SavingEngineLibrary => engineLibrarySaving || engineKnowledgeSaving;
    private bool engineLibraryNarrowDetail;
    private bool engineLibraryLoadFailed;
    private bool engineLibraryReading;
    private string? engineLibraryDirectory;
    private int engineLibraryVersionReadVersion;
    private bool engineLibraryVersionUpdating;
    private bool engineLibraryVersionInspecting;
    private EngineVersionRow? inspectedEngineVersionRow;
    private EngineLibraryVersion? inspectedEngineVersion;
    private string? engineLibraryVersionInspectionError;

    private EngineLibraryVersion? SelectedInspectedLibraryVersion =>
        ReferenceEquals(CatalogVersionsList.SelectedItem, inspectedEngineVersionRow) ? inspectedEngineVersion : null;

    private LocalAdapterPackageStore LibraryStore() => new(Path.Combine(dataDirectory, "extensions"));

    // Invalidation does not read disk. Off-page updates are loaded when the library is opened.
    private void InvalidateEngineLibrary() => engineLibraryDirty = true;

    private void RefreshCatalog()
    {
        InvalidateEngineLibrary();
        if (engineLibraryDirectory is not null && engineLibraryDirectory != dataDirectory)
        {
            engineFamilyRows = [];
            engineKnowledgeSummaries = [];
            CatalogFamiliesList.ItemsSource = null;
            ClearEngineFamily();
        }
        if (globalPage == "catalog") _ = EnsureEngineLibraryAsync();
    }

    private async Task EnsureEngineLibraryAsync()
    {
        if (engineLibraryLoad is { } pending) { await pending; return; }
        if (!engineLibraryDirty && engineLibraryDirectory == dataDirectory) return;
        engineLibraryLoad = LoadEngineLibraryAsync();
        try { await engineLibraryLoad; }
        finally { engineLibraryLoad = null; }
        if (engineLibraryDirty && !engineLibraryLoadFailed && globalPage == "catalog") await EnsureEngineLibraryAsync();
    }

    private async Task LoadEngineLibraryAsync()
    {
        var directory = dataDirectory;
        var store = LibraryStore();
        var builtIns = adapters.Select(adapter => adapter.CatalogEntry).OfType<AdapterCatalogEntry>()
            .Append(new(SiglusAssets.Family, SiglusAssets.Version, [])).ToArray();
        engineLibraryDirty = false;
        engineLibraryLoadFailed = false;
        SetLibraryLoading(true);
        try
        {
            var loaded = await store.ReadEngineLibraryAsync(builtIns, directory);
            if (directory != dataDirectory) { engineLibraryDirty = true; return; }
            engineLibraryDirectory = directory;
            engineKnowledgeSummaries = loaded.Knowledge;
            var families = loaded.Families.ToList();
            var familyIds = families.Select(family => family.Family.AdapterId).ToHashSet(StringComparer.Ordinal);
            foreach (var summary in loaded.Knowledge)
                if (familyIds.Add(summary.Family.AdapterId))
                    families.Add(new(summary.Family, true, [], [], []));
            engineFamilyRows = families.Select(family => new EngineFamilyRow(family)).ToArray();
            ApplyCatalogFilters(preservePosition: true);
            var diagnostics = loaded.Diagnostics;
            if (diagnostics.Count > 0)
                ShowLibraryStatus($"{diagnostics.Count:N0} 项记录未能完整读取。\n" + string.Join("\n", diagnostics.Take(4)), InfoBarSeverity.Warning);
            else CatalogInfo.IsOpen = false;
        }
        catch (Exception error)
        {
            if (directory != dataDirectory) { engineLibraryDirty = true; return; }
            // Keep the current in-memory view available when a refresh fails.
            engineLibraryDirty = true;
            engineLibraryLoadFailed = true;
            ShowLibraryStatus("读取引擎库失败：" + error.Message + "。可点击刷新重试。", InfoBarSeverity.Error);
            CatalogEmptyText.Text = "引擎库未能加载。请点击刷新重试。";
        }
        finally { SetLibraryLoading(false); }
    }

    private void SetLibraryLoading(bool loading)
    {
        engineLibraryReading = loading;
        CatalogLoadingPanel.Visibility = loading && engineFamilyRows.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
        CatalogLoadingRing.IsActive = loading;
        CatalogRefreshButton.IsEnabled = !loading && !SavingEngineLibrary;
        CatalogRefreshButton.Content = loading ? "读取中…" : "刷新";
        CatalogEmptyText.Visibility = !loading && CatalogFamiliesList.Items.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
        UpdateLibraryActions();
    }

    private async void RefreshCatalog_Click(object sender, RoutedEventArgs e)
    {
        InvalidateEngineLibrary();
        await EnsureEngineLibraryAsync();
    }

    private void CatalogSearch_TextChanged(object sender, TextChangedEventArgs e)
    {
        if (CatalogFamiliesList is not null) ApplyCatalogFilters();
    }

    private void CatalogFilter_Changed(object sender, SelectionChangedEventArgs e)
    {
        if (CatalogFamiliesList is not null) ApplyCatalogFilters();
    }

    private static string FilterValue(ComboBox box) => (box.SelectedItem as ComboBoxItem)?.Tag as string ?? "all";

    private void ApplyCatalogFilters(bool preservePosition = false)
    {
        var selectedIds = CatalogFamiliesList.SelectedItems.OfType<EngineFamilyRow>().Select(row => row.Id).ToHashSet(StringComparer.Ordinal);
        var detailId = selectedEngineFamily?.Id;
        var scroll = FindLibraryScrollViewer(CatalogFamiliesList);
        var offset = preservePosition ? scroll?.VerticalOffset ?? 0 : 0;
        var query = CatalogSearchBox.Text.Trim();
        var source = FilterValue(CatalogSourceFilter);
        var enabled = FilterValue(CatalogEnabledFilter);
        var rows = engineFamilyRows.Where(row => (query.Length == 0 || row.SearchText.Contains(query, StringComparison.OrdinalIgnoreCase))
            && (source == "all" || row.MatchesSource(source))
            && (enabled == "all" || row.CanSetEnabled && row.Data.Enabled == (enabled == "enabled")));
        rows = FilterValue(CatalogSortBox) switch
        {
            "games" => rows.OrderByDescending(row => row.Data.Games.Count).ThenBy(row => row.Name, StringComparer.CurrentCultureIgnoreCase),
            "recent" => rows.OrderByDescending(row => row.LatestPublication).ThenBy(row => row.Name, StringComparer.CurrentCultureIgnoreCase),
            _ => rows.OrderBy(row => row.Name, StringComparer.CurrentCultureIgnoreCase).ThenBy(row => row.Id, StringComparer.Ordinal)
        };
        var visible = rows.ToArray();
        engineLibraryUpdating = true;
        try
        {
            CatalogFamiliesList.ItemsSource = visible;
            foreach (var row in visible.Where(row => selectedIds.Contains(row.Id))) CatalogFamiliesList.SelectedItems.Add(row);
        }
        finally { engineLibraryUpdating = false; }
        CatalogEmptyText.Text = engineFamilyRows.Count == 0 ? "暂无引擎记录。完成适配或总结后会在这里显示。" : "没有匹配的引擎、模板或兼容游戏。请调整搜索或筛选。";
        CatalogEmptyText.Visibility = visible.Length == 0 && engineLibraryLoad is null ? Visibility.Visible : Visibility.Collapsed;
        CatalogCountText.Text = $"{visible.Length:N0} / {engineFamilyRows.Count:N0} 个引擎与模板 · Ctrl / Shift 多选";
        if (detailId is not null && engineFamilyRows.FirstOrDefault(row => row.Id == detailId) is { } detail)
        {
            if (!ReferenceEquals(detail, selectedEngineFamily)) ShowEngineFamily(detail, false);
        }
        else if (selectedEngineFamily is not null) ClearEngineFamily();
        UpdateLibraryActions();
        CatalogFamiliesList.DispatcherQueue.TryEnqueue(() => FindLibraryScrollViewer(CatalogFamiliesList)?.ChangeView(null, offset, null, true));
    }

    private void CatalogFamilies_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (engineLibraryUpdating) return;
        if (e.AddedItems.OfType<EngineFamilyRow>().LastOrDefault() is { } row) ShowEngineFamily(row, false);
        UpdateLibraryActions();
    }

    private void CatalogFamily_ItemClick(object sender, ItemClickEventArgs e)
    {
        if (e.ClickedItem is EngineFamilyRow row) ShowEngineFamily(row, true);
    }

    private void CatalogMultiSelect_Changed(object sender, RoutedEventArgs e)
    {
        if (CatalogFamiliesList is null) return;
        var selected = CatalogFamiliesList.SelectedItems.OfType<EngineFamilyRow>().ToArray();
        engineLibraryUpdating = true;
        try
        {
            var multiple = CatalogMultiSelectButton.IsChecked == true;
            CatalogFamiliesList.SelectionMode = multiple ? ListViewSelectionMode.Multiple : ListViewSelectionMode.Extended;
            CatalogFamiliesList.IsItemClickEnabled = !multiple;
            foreach (var row in selected)
                if (!CatalogFamiliesList.SelectedItems.Contains(row)) CatalogFamiliesList.SelectedItems.Add(row);
        }
        finally { engineLibraryUpdating = false; }
        UpdateLibraryActions();
    }

    private void ShowEngineFamily(EngineFamilyRow row, bool openNarrow)
    {
        if (ReferenceEquals(selectedEngineFamily, row))
        {
            if (openNarrow) engineLibraryNarrowDetail = true;
            UpdateLibraryLayout();
            return;
        }
        var changed = selectedEngineFamily?.Id != row.Id;
        selectedEngineFamily = row;
        CatalogDetailTitle.Text = row.Name;
        CatalogDetailSubtitle.Text = row.Id + " · " + row.EnabledLabel;
        CatalogDetailTabs.Visibility = Visibility.Visible;
        CatalogOverviewText.Text = $"分类：{(row.Data.Family.Kind == "engine" ? "引擎" : "资源模板")}\n来源：{(row.SourceLabel.Length == 0 ? "经验记录" : row.SourceLabel)}\n版本：{row.Data.Versions.Count:N0} 个\n兼容游戏记录：{row.Data.Games.Count:N0} 条\n项目引用：删除外置版本时检查\n\n新识别候选：{(row.CanSetEnabled ? row.EnabledLabel : "暂无适配版本，经验启停在“相关经验”中设置")}";
        var selectedVersion = (CatalogVersionsList.SelectedItem as EngineVersionRow)?.Data;
        ResetLibraryVersionInspection();
        engineLibraryVersionUpdating = true;
        try
        {
            CatalogVersionsList.ItemsSource = row.Data.Versions.Select(version => new EngineVersionRow(version)).ToArray();
            CatalogVersionsList.SelectedItem = changed ? null : CatalogVersionsList.Items.OfType<EngineVersionRow>().FirstOrDefault(version =>
                selectedVersion is not null && version.Data.AdapterId == selectedVersion.AdapterId && version.Data.Version == selectedVersion.Version && version.Data.Source == selectedVersion.Source);
        }
        finally { engineLibraryVersionUpdating = false; }
        UpdateLibraryVersionActions();
        if (changed) CatalogGameSearchBox.Text = "";
        ApplyCatalogGameFilter();
        RenderLibraryKnowledge(row.Data.Family, changed);
        if (openNarrow) engineLibraryNarrowDetail = true;
        UpdateLibraryLayout();
    }

    private void ClearEngineFamily()
    {
        selectedEngineFamily = null;
        ResetLibraryVersionInspection();
        engineLibraryVersionUpdating = true;
        try { CatalogVersionsList.ItemsSource = null; }
        finally { engineLibraryVersionUpdating = false; }
        ClearLibraryKnowledgeSelection();
        UpdateLibraryVersionActions();
        CatalogDetailTitle.Text = "选择引擎或资源模板";
        CatalogDetailSubtitle.Text = "查看版本、兼容游戏与相关经验。";
        CatalogDetailTabs.Visibility = Visibility.Collapsed;
        engineLibraryNarrowDetail = false;
        UpdateLibraryLayout();
    }

    private void CatalogGameSearch_TextChanged(object sender, TextChangedEventArgs e) => ApplyCatalogGameFilter();

    private void ApplyCatalogGameFilter()
    {
        if (CatalogGamesList is null) return;
        var query = CatalogGameSearchBox.Text.Trim();
        var all = selectedEngineFamily?.Data.Games ?? [];
        var filtered = all.Where(game => query.Length == 0 || (game.Title + "\n" + game.Variant + "\n" + game.AdapterVersion)
            .Contains(query, StringComparison.OrdinalIgnoreCase)).OrderBy(game => game.Title, StringComparer.CurrentCultureIgnoreCase)
            .ThenBy(game => game.AdapterVersion, StringComparer.Ordinal).Select(game => new EngineGameRow(game)).ToArray();
        CatalogGamesList.ItemsSource = filtered;
        CatalogGameCountText.Text = all.Count == 0 ? "暂无兼容游戏记录。" : $"{filtered.Length:N0} / {all.Count:N0} 条记录 · 样本与结构校验不代表游戏运行验证";
    }

    private async void CatalogVersion_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (!engineLibraryVersionUpdating) await InspectSelectedLibraryVersionAsync();
    }

    private async void CatalogVersion_ItemClick(object sender, ItemClickEventArgs e)
    {
        if (!engineLibraryVersionInspecting && ReferenceEquals(e.ClickedItem, CatalogVersionsList.SelectedItem))
            await InspectSelectedLibraryVersionAsync();
    }

    private void ResetLibraryVersionInspection()
    {
        engineLibraryVersionReadVersion++;
        engineLibraryVersionInspecting = false;
        inspectedEngineVersionRow = null;
        inspectedEngineVersion = null;
        engineLibraryVersionInspectionError = null;
    }

    private async Task InspectSelectedLibraryVersionAsync()
    {
        ResetLibraryVersionInspection();
        if (CatalogVersionsList.SelectedItem is not EngineVersionRow row) { UpdateLibraryVersionActions(); return; }
        inspectedEngineVersionRow = row;
        if (row.Data.Source != EngineLibrarySource.External)
        {
            inspectedEngineVersion = row.Data;
            UpdateLibraryVersionActions();
            return;
        }
        var request = engineLibraryVersionReadVersion;
        var directory = dataDirectory;
        var store = LibraryStore();
        bool IsCurrent() => request == engineLibraryVersionReadVersion && directory == dataDirectory
            && ReferenceEquals(CatalogVersionsList.SelectedItem, row);
        engineLibraryVersionInspecting = true;
        UpdateLibraryVersionActions();
        try
        {
            var inspected = await store.InspectLibraryVersionAsync(row.Data);
            if (IsCurrent()) inspectedEngineVersion = inspected;
        }
        catch (Exception error)
        {
            if (IsCurrent()) engineLibraryVersionInspectionError = error.Message;
        }
        finally
        {
            if (IsCurrent())
            {
                engineLibraryVersionInspecting = false;
                UpdateLibraryVersionActions();
            }
        }
    }

    private void UpdateLibraryActions()
    {
        var selected = CatalogFamiliesList.SelectedItems.OfType<EngineFamilyRow>().Where(row => row.CanSetEnabled).ToArray();
        CatalogRefreshButton.IsEnabled = !engineLibraryReading && !SavingEngineLibrary;
        CatalogKnowledgeEnabledBox.IsEnabled = selectedKnowledgeEntry is not null && !SavingEngineLibrary;
        CatalogEnableButton.IsEnabled = !SavingEngineLibrary && selected.Any(row => !row.Data.Enabled);
        CatalogDisableButton.IsEnabled = !SavingEngineLibrary && selected.Any(row => row.Data.Enabled);
        CatalogEnableButton.Content = selected.Length > 1 ? $"启用所选 ({selected.Length})" : "启用所选";
        CatalogDisableButton.Content = selected.Length > 1 ? $"停用所选 ({selected.Length})" : "停用所选";
        UpdateLibraryVersionActions();
    }

    private void UpdateLibraryVersionActions()
    {
        var selected = (CatalogVersionsList.SelectedItem as EngineVersionRow)?.Data;
        var version = SelectedInspectedLibraryVersion;
        CatalogOpenVersionButton.IsEnabled = !SavingEngineLibrary && version is { Source: EngineLibrarySource.External, Exists: true, DirectoryPath: not null };
        CatalogVerifyVersionButton.IsEnabled = !SavingEngineLibrary && version is { Source: EngineLibrarySource.External, Exists: true };
        CatalogDeleteVersionButton.IsEnabled = !SavingEngineLibrary && version is { Source: EngineLibrarySource.External, Exists: true }
            && (!version.ReferencesKnown || version.References.Count == 0);
        if (selected is null)
            CatalogVersionReferenceText.Text = "选择版本以检查文件和查看可用操作。";
        else if (selected.Source == EngineLibrarySource.BuiltIn)
            CatalogVersionReferenceText.Text = "内置版本随程序提供，不能删除。";
        else if (selected.Source == EngineLibrarySource.History)
            CatalogVersionReferenceText.Text = "仅保留兼容历史记录，没有可删除的扩展文件。";
        else if (engineLibraryVersionInspecting)
            CatalogVersionReferenceText.Text = "正在检查此版本的文件…\n项目引用将在删除时检查。";
        else if (engineLibraryVersionInspectionError is { } error)
            CatalogVersionReferenceText.Text = "检查版本文件失败：" + error + "\n可重新选择此版本重试。";
        else if (version is null)
            CatalogVersionReferenceText.Text = "文件尚未检查。选择此版本检查文件是否存在；项目引用将在删除时检查。";
        else if (!version.Exists)
            CatalogVersionReferenceText.Text = "文件丢失：此版本的文件夹或必要文件已不存在。\n索引与兼容历史记录仍保留。";
        else if (!version.ReferencesKnown)
            CatalogVersionReferenceText.Text = "版本文件存在。删除时将检查项目引用，无引用时才会删除外置文件。兼容历史记录会保留。";
        else if (version.References.Count == 0)
            CatalogVersionReferenceText.Text = "版本文件存在，没有项目引用此版本。删除时会再次检查，兼容历史记录会保留。";
        else CatalogVersionReferenceText.Text = "当前不能删除：\n" + FormatLibraryReferences(version.References);
    }

    private async void CatalogEnable_Click(object sender, RoutedEventArgs e) => await SetLibraryFamiliesEnabledAsync(true);
    private async void CatalogDisable_Click(object sender, RoutedEventArgs e) => await SetLibraryFamiliesEnabledAsync(false);

    private async Task SetLibraryFamiliesEnabledAsync(bool enabled)
    {
        if (SavingEngineLibrary || changingDataDirectory || closing) return;
        var selected = CatalogFamiliesList.SelectedItems.OfType<EngineFamilyRow>().Where(row => row.CanSetEnabled && row.Data.Enabled != enabled).Select(row => row.Id).ToArray();
        if (selected.Length == 0) return;
        var directory = dataDirectory;
        engineLibrarySaving = true;
        UpdateControls();
        UpdateLibraryActions();
        try
        {
            var store = LibraryStore();
            await Task.Run(() => store.SetFamiliesEnabledAsync(selected, enabled));
            if (directory != dataDirectory) return;
            InvalidateEngineLibrary();
            await EnsureEngineLibraryAsync();
            ShowLibraryStatus($"已{(enabled ? "启用" : "停用")} {selected.Length:N0} 个引擎与模板，对下一次新识别生效。已有项目固定版本仍可使用。", InfoBarSeverity.Success);
        }
        catch (Exception error)
        {
            if (directory != dataDirectory) return;
            InvalidateEngineLibrary();
            await EnsureEngineLibraryAsync();
            ShowLibraryStatus("保存启用状态失败：" + error.Message, InfoBarSeverity.Error);
        }
        finally { engineLibrarySaving = false; UpdateControls(); UpdateLibraryActions(); }
    }

    private void CatalogOpenVersion_Click(object sender, RoutedEventArgs e)
    {
        if (SelectedInspectedLibraryVersion is not { Source: EngineLibrarySource.External, Exists: true, DirectoryPath: { } directory }) return;
        try
        {
            if (!Directory.Exists(directory)) throw new DirectoryNotFoundException("此版本文件夹已不存在，索引记录仍保留。可重新选择版本检查。");
            Process.Start(new ProcessStartInfo(directory) { UseShellExecute = true });
        }
        catch (Exception error) { ShowLibraryStatus("无法打开文件夹：" + error.Message, InfoBarSeverity.Error); }
    }

    private async void CatalogDeleteVersion_Click(object sender, RoutedEventArgs e)
    {
        if (SavingEngineLibrary || changingDataDirectory || closing || SelectedInspectedLibraryVersion is not { Source: EngineLibrarySource.External, Exists: true } version) return;
        var directory = dataDirectory;
        var references = version.References;
        var blocked = version.ReferencesKnown && references.Count > 0;
        var dialog = new ContentDialog
        {
            XamlRoot = RootGrid.XamlRoot, Title = blocked ? "此版本正在被引用" : "删除外置引擎版本？",
            Content = new TextBlock { Text = $"{selectedEngineFamily?.Name}\n{version.AdapterId}\n版本：{version.Version}\n位置：{version.DirectoryPath}\n\n"
                + (blocked ? "无法删除：\n" + FormatLibraryReferences(references) : "将先检查当前项目引用，仅在没有引用时删除此版本的外置文件。兼容历史记录会保留。此操作无法撤销。"), TextWrapping = TextWrapping.Wrap },
            PrimaryButtonText = blocked ? "" : "删除此版本", CloseButtonText = blocked ? "关闭" : "取消", DefaultButton = ContentDialogButton.Close
        };
        if (await dialog.ShowAsync() != ContentDialogResult.Primary) return;
        if (games.Any(game => game.Busy))
        { ShowLibraryStatus("请先等待当前任务完成或暂停，再删除扩展版本。", InfoBarSeverity.Warning); return; }
        if (directory != dataDirectory || changingDataDirectory || changingAdapterLibrary) return;
        changingAdapterLibrary = true;
        engineLibrarySaving = true;
        UpdateControls();
        UpdateLibraryActions();
        try
        {
            var store = LibraryStore();
            var runtimeReferences = GetRunningEngineReferences();
            var result = await Task.Run(() => store.DeleteVersionAsync(version.AdapterId, version.Version, directory, runtimeReferences));
            InvalidateEngineLibrary();
            await EnsureEngineLibraryAsync();
            ShowLibraryStatus(result.Deleted ? $"已删除外置版本 {version.AdapterId} · {version.Version}。" : "未删除此版本：\n"
                + string.Join("\n", result.Diagnostics) + (result.References.Count > 0 ? "\n" + FormatLibraryReferences(result.References) : ""),
                result.Deleted ? InfoBarSeverity.Success : InfoBarSeverity.Warning);
        }
        catch (Exception error) { ShowLibraryStatus("删除版本失败：" + error.Message, InfoBarSeverity.Error); }
        finally { changingAdapterLibrary = false; engineLibrarySaving = false; UpdateControls(); UpdateLibraryActions(); }
    }

    private static string FormatLibraryReferences(IReadOnlyList<EngineLibraryReference> references) => string.Join("\n", references.Take(8).Select(reference =>
        $"{reference.GameTitle ?? reference.GameId ?? "项目记录"} · {ReferenceLabel(reference.Kind)}")) + (references.Count > 8 ? $"\n另有 {references.Count - 8:N0} 条引用。" : "");

    private static string ReferenceLabel(EngineLibraryReferenceKind kind) => kind switch
    {
        EngineLibraryReferenceKind.Active => "当前固定版本", EngineLibraryReferenceKind.Previous => "回滚版本",
        EngineLibraryReferenceKind.Running => "已加载会话", EngineLibraryReferenceKind.Source => "原始文本快照",
        EngineLibraryReferenceKind.Extraction => "提取记录", EngineLibraryReferenceKind.Job => "翻译任务", _ => "项目记录"
    };

    private void ShowLibraryStatus(string message, InfoBarSeverity severity)
    {
        CatalogInfo.Title = severity == InfoBarSeverity.Error ? "操作未完成" : "引擎库";
        CatalogInfo.Message = message;
        CatalogInfo.Severity = severity;
        CatalogInfo.IsOpen = true;
    }

    private void CatalogLayout_SizeChanged(object sender, SizeChangedEventArgs e) => UpdateLibraryLayout();

    private void UpdateLibraryLayout()
    {
        var narrow = CatalogLayout.ActualWidth < 780;
        var detail = narrow && engineLibraryNarrowDetail && selectedEngineFamily is not null;
        CatalogListPane.Visibility = detail ? Visibility.Collapsed : Visibility.Visible;
        CatalogDetailPane.Visibility = !narrow || detail ? Visibility.Visible : Visibility.Collapsed;
        CatalogListColumn.Width = detail ? new GridLength(0) : new GridLength(1, GridUnitType.Star);
        CatalogDetailColumn.Width = !narrow ? new GridLength(1.2, GridUnitType.Star) : detail ? new GridLength(1, GridUnitType.Star) : new GridLength(0);
        CatalogDetailPane.BorderThickness = narrow ? new Thickness(0) : new Thickness(1, 0, 0, 0);
        CatalogDetailPane.Padding = narrow ? new Thickness(0) : new Thickness(16, 0, 0, 0);
        CatalogBackButton.Visibility = detail ? Visibility.Visible : Visibility.Collapsed;
        var compactFilters = (narrow ? CatalogLayout.ActualWidth : CatalogLayout.ActualWidth / 2.2) < 430;
        CatalogFiltersGrid.ColumnDefinitions[2].Width = compactFilters ? new GridLength(0) : new GridLength(1, GridUnitType.Star);
        Grid.SetColumn(CatalogSortBox, compactFilters ? 0 : 2);
        Grid.SetColumnSpan(CatalogSortBox, compactFilters ? 2 : 1);
        Grid.SetRow(CatalogSortBox, compactFilters ? 1 : 0);
        CatalogSortBox.Margin = compactFilters ? new Thickness(0, 8, 0, 0) : new Thickness(0);
    }

    private void CatalogBack_Click(object sender, RoutedEventArgs e)
    {
        engineLibraryNarrowDetail = false;
        UpdateLibraryLayout();
        CatalogFamiliesList.Focus(FocusState.Programmatic);
    }

    private static ScrollViewer? FindLibraryScrollViewer(DependencyObject element)
    {
        if (element is ScrollViewer viewer) return viewer;
        for (var index = 0; index < VisualTreeHelper.GetChildrenCount(element); index++)
            if (FindLibraryScrollViewer(VisualTreeHelper.GetChild(element, index)) is { } found) return found;
        return null;
    }

    private sealed class EngineFamilyRow(EngineLibraryFamily data)
    {
        public EngineLibraryFamily Data { get; } = data;
        public string Id => Data.Family.AdapterId;
        public string Name => Data.Family.Name;
        public bool CanSetEnabled => Data.Versions.Count > 0;
        public string EnabledLabel => CanSetEnabled ? Data.Enabled ? "已启用" : "已停用" : "仅经验";
        public string SourceLabel => string.Join(" / ", Data.Versions.Select(version => SourceName(version.Source)).Distinct());
        public string Description => $"{(SourceLabel.Length == 0 ? "经验记录" : SourceLabel)} · {Data.Versions.Count:N0} 版本 · {Data.Games.Count:N0} 游戏记录";
        public DateTimeOffset LatestPublication { get; } = data.Versions.Select(version => version.PublishedAt ?? DateTimeOffset.MinValue).DefaultIfEmpty().Max();
        public string SearchText { get; } = string.Join('\n', new[] { data.Family.AdapterId, data.Family.Name }.Concat(data.Games.SelectMany(game => new[] { game.Title, game.Variant })));
        public bool MatchesSource(string source) => source switch
        {
            "builtin" => Data.Versions.Any(version => version.Source == EngineLibrarySource.BuiltIn),
            "external" => Data.Versions.Any(version => version.Source == EngineLibrarySource.External),
            "history" => Data.Versions.Count > 0 && Data.Versions.All(version => version.Source == EngineLibrarySource.History), _ => true
        };
    }

    private sealed class EngineVersionRow(EngineLibraryVersion data)
    {
        public EngineLibraryVersion Data { get; } = data;
        public string Title => Data.Version + " · " + SourceName(Data.Source);
        public string Description => (Data.PublishedAt is { } published ? $"{published.ToLocalTime():yyyy-MM-dd HH:mm} · " : "")
            + $"{Data.Games.Count:N0} 条游戏记录 · " + (Data.ReferencesKnown ? $"{Data.References.Count:N0} 条引用" : "删除时检查项目引用")
            + (Data.IsCandidate ? " · 可供新识别" : Data.Source == EngineLibrarySource.External ? " · 当前非新识别候选" : "");
    }

    private sealed class EngineGameRow(GameCompatibility game)
    {
        public string Title => game.Title;
        public string Description => (string.IsNullOrWhiteSpace(game.Variant) ? "" : game.Variant + " · ") + "适配版本 " + game.AdapterVersion;
        public string Validation => StructureLabel(game.StructureStatus) + "；游戏运行：" + (game.RuntimeStatus == "passed" ? "已验证" : game.RuntimeStatus == "failed" ? "验证失败" : "未验证");
    }

    private static string SourceName(EngineLibrarySource source) => source switch { EngineLibrarySource.BuiltIn => "内置", EngineLibrarySource.External => "外置扩展", _ => "仅历史记录" };

    private static string StructureLabel(string status) => status switch
    {
        "sample-roundtrip-passed" => "样本回填通过", "resource-roundtrip-passed" => "资源回填通过",
        "patch-validated" => "汉化产物结构校验通过", _ => "结构验证：" + status
    };
}
