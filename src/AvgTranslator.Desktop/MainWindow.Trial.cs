using AvgTranslator.Core;
using AvgTranslator.Core.Storage;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Markup;

namespace AvgTranslator.Desktop;

public sealed partial class MainWindow
{
    private async void Trial_Click(object sender, RoutedEventArgs e)
    {
        if (session is null || busy) return;
        var captured = session;
        try
        {
            var units = captured.Extraction.Units;
            var knownIds = units.Select(u => u.Id).ToHashSet(StringComparer.Ordinal);
            var previous = await LocalFiles.ReadJsonAsync<string[]>(Path.Combine(captured.ProjectDirectory, "trial-selection.json"));
            var selectedIds = (previous ?? []).Where(knownIds.Contains).ToHashSet(StringComparer.Ordinal);
            var rows = units.Select((unit, index) => new TrialRow(unit,
                $"{index + 1}. {unit.ResourcePath} · {unit.Locator} · {TrialKind(unit.Kind)}\n{unit.SourceText}")).ToArray();
            var byLabel = rows.ToDictionary(row => row.Label, StringComparer.Ordinal);

            var resources = new ComboBox { Header = "脚本 / 资源", HorizontalAlignment = HorizontalAlignment.Stretch };
            resources.Items.Add(new ComboBoxItem { Content = "全部脚本" });
            foreach (var group in units.GroupBy(u => u.ResourcePath))
                resources.Items.Add(new ComboBoxItem { Content = $"{group.Key}（{group.Count():N0} 条）", Tag = group.Key });
            resources.SelectedIndex = selectedIds.Count == 0 ? 1 : 0;
            var search = new TextBox { Header = "查找原文", PlaceholderText = "输入游戏画面中的文字、角色名或位置" };
            var count = new NumberBox { Header = "批量勾选条数", Value = 20, Minimum = 1, Maximum = 200,
                SpinButtonPlacementMode = NumberBoxSpinButtonPlacementMode.Inline, Width = 150 };
            var selectFirst = new Button { Content = "加选列表前 N 条", VerticalAlignment = VerticalAlignment.Bottom };
            var clear = new Button { Content = "清空全部选择", VerticalAlignment = VerticalAlignment.Bottom };
            var selectedOnly = new CheckBox { Content = "仅显示已选文本" };
            var summary = new TextBlock { TextWrapping = TextWrapping.Wrap };
            var list = new ListView
            {
                SelectionMode = ListViewSelectionMode.Multiple, IsMultiSelectCheckBoxEnabled = true,
                Height = 260, HorizontalAlignment = HorizontalAlignment.Stretch,
                ItemTemplate = (DataTemplate)XamlReader.Load("""
                    <DataTemplate xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation">
                      <TextBlock Text="{Binding}" TextWrapping="Wrap" Margin="0,6" />
                    </DataTemplate>
                    """)
            };
            ScrollViewer.SetHorizontalScrollBarVisibility(list, ScrollBarVisibility.Disabled);
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(list, "可试译的原文，勾选要在游戏中检查的文本");
            var controls = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 12 };
            controls.Children.Add(count);
            controls.Children.Add(selectFirst);
            controls.Children.Add(clear);
            var body = new StackPanel { Spacing = 12 };
            body.Children.Add(new TextBlock { Text = "勾选开场或能通过存档到达的文本。可跨脚本选择，其余文本保留原文；试译结果可用于后续全量翻译。",
                TextWrapping = TextWrapping.Wrap });
            body.Children.Add(resources);
            body.Children.Add(search);
            body.Children.Add(controls);
            body.Children.Add(selectedOnly);
            body.Children.Add(list);
            body.Children.Add(summary);
            var dialog = new ContentDialog
            {
                XamlRoot = RootGrid.XamlRoot, Title = "选择文本试译", Content = new ScrollViewer { Content = body,
                    VerticalScrollBarVisibility = ScrollBarVisibility.Auto, HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled },
                PrimaryButtonText = "试译并生成补丁", CloseButtonText = "取消", DefaultButton = ContentDialogButton.Primary
            };
            dialog.Resources["ContentDialogMaxWidth"] = 820d;
            TrialRow[] visible = [];
            var updating = false;
            void UpdateSummary()
            {
                summary.Text = $"已选 {selectedIds.Count:N0} 条 · 当前列表 {visible.Length:N0} 条。切换脚本或搜索会保留选择。";
                dialog.IsPrimaryButtonEnabled = selectedIds.Count > 0;
                selectFirst.IsEnabled = visible.Length > 0;
                clear.IsEnabled = selectedIds.Count > 0;
            }
            void RefreshList()
            {
                var path = (resources.SelectedItem as ComboBoxItem)?.Tag as string;
                var query = search.Text.Trim();
                visible = rows.Where(row => (path is null || row.Unit.ResourcePath == path)
                    && (query.Length == 0 || row.Label.Contains(query, StringComparison.OrdinalIgnoreCase)
                        || row.Unit.Speaker?.Contains(query, StringComparison.OrdinalIgnoreCase) == true)
                    && (selectedOnly.IsChecked != true || selectedIds.Contains(row.Unit.Id))).ToArray();
                updating = true;
                try
                {
                    list.ItemsSource = visible.Select(row => row.Label).ToArray();
                    foreach (var row in visible.Where(row => selectedIds.Contains(row.Unit.Id)))
                        list.SelectedItems.Add(row.Label);
                }
                finally { updating = false; }
                UpdateSummary();
            }
            list.SelectionChanged += (_, args) =>
            {
                if (updating) return;
                foreach (string label in args.RemovedItems) selectedIds.Remove(byLabel[label].Unit.Id);
                foreach (string label in args.AddedItems) selectedIds.Add(byLabel[label].Unit.Id);
                UpdateSummary();
            };
            resources.SelectionChanged += (_, _) => RefreshList();
            search.TextChanged += (_, _) => RefreshList();
            selectedOnly.Checked += (_, _) => RefreshList();
            selectedOnly.Unchecked += (_, _) => RefreshList();
            selectFirst.Click += (_, _) =>
            {
                var take = double.IsFinite(count.Value) ? (int)Math.Clamp(count.Value, 1, 200) : 20;
                foreach (var row in visible.Take(take)) selectedIds.Add(row.Unit.Id);
                RefreshList();
            };
            clear.Click += (_, _) => { selectedIds.Clear(); RefreshList(); };
            RefreshList();
            if (await dialog.ShowAsync() != ContentDialogResult.Primary || selectedIds.Count == 0) return;
            if (!ReferenceEquals(session, captured) || busy)
                throw new InvalidOperationException("当前游戏已变化，请重新选择试译文本。");
            await TranslateAsync(units.Where(u => selectedIds.Contains(u.Id)).Select(u => u.Id).ToArray());
        }
        catch (Exception error) { ShowError(error); }
    }

    private static string TrialKind(string kind) => kind switch
    {
        "name" => "角色名", "choice" => "选项", "dialogue" => "对白 / 旁白", _ => kind
    };

    private sealed record TrialRow(TranslationUnit Unit, string Label);
}
