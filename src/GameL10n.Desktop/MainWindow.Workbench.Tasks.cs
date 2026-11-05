using GameL10n.Core;
using GameL10n.Core.Pipeline;
using GameL10n.Core.Storage;
using GameL10n.Core.Translation;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Markup;

namespace GameL10n.Desktop;

public sealed partial class MainWindow
{
    internal sealed record TranslationScope(IReadOnlyList<string>? UnitIds, bool BuildPatchAfter, bool Completed, string? RetranslationId = null);
    internal sealed record RuntimeCheck(string Status, string Note, DateTimeOffset CheckedAt);

    private Progress<PipelineProgress> TranslationProgress(GameWorkspace game)
    {
        var runId = game.RunId;
        long tokens = 0;
        var cacheHits = 0;
        return new(update =>
        {
            if (!game.AcceptProgress || !game.Busy || game.RunId != runId) return;
            tokens = Math.Max(tokens, update.TranslationTokens);
            cacheHits = Math.Max(cacheHits, update.CacheHits);
            update = update with { TranslationTokens = tokens, CacheHits = cacheHits };
            game.Progress = update;
            SetStage(game, update.Stage, update.Message);
            if (update.Stage != "失败") Log(game, update.Message);
        });
    }

    private IProgress<IReadOnlyDictionary<string, string>> TranslationUpdates(GameWorkspace game, string? retranslationId = null)
    {
        var captured = game.Session;
        var policyKey = WorkbenchPolicy(game);
        var runId = game.RunId;
        return new Progress<IReadOnlyDictionary<string, string>>(updates =>
        {
            if (!ReferenceEquals(game.RowsSession, captured) || game.RowsPolicy != policyKey) return;
            if (game.RunId != runId || (retranslationId is not null && (!game.AcceptProgress || !game.Retranslating))) return;
            foreach (var (id, text) in updates)
                if (game.RowsById.TryGetValue(id, out var row))
                {
                    if (retranslationId is not null)
                    {
                        // Resuming a task must retain edits and review state made after its previous run.
                        if (row.Edit?.RetranslationId != retranslationId || row.Edit.Text != text)
                            row.Edit = new(row.Signature, game.Project.Settings.TargetLanguage, text, false, DateTimeOffset.UtcNow, retranslationId, IsGenerated: true);
                        game.Edits[id] = row.Edit; game.Candidates.Remove(id);
                    }
                    else row.ModelText = text;
                    row.Refresh();
                    if (selectedGame == game && row == editingRow && (row.Edit is null || retranslationId is not null)) RenderEditor();
                }
            InvalidateWorkbenchCache(game);
            if (selectedGame == game) { QueueTextFilter(); UpdatePatchNotice(); }
        });
    }

    private IProgress<IReadOnlyDictionary<string, string?>> TranslationFailureUpdates(GameWorkspace game)
    {
        var captured = game.Session;
        var policyKey = WorkbenchPolicy(game);
        var runId = game.RunId;
        return new Progress<IReadOnlyDictionary<string, string?>>(updates =>
        {
            if (game.RunId != runId || !ReferenceEquals(game.RowsSession, captured) || game.RowsPolicy != policyKey) return;
            foreach (var (id, reason) in updates)
                if (game.RowsById.TryGetValue(id, out var row)) { row.TranslationFailure = reason; row.Refresh(); }
            InvalidateWorkbenchCache(game);
            if (selectedGame == game) { RenderEditorValidation(); QueueTextFilter(); }
        });
    }

    private async Task RefreshSavedRetranslationsAsync(GameWorkspace game)
    {
        // Discard late updates before applying the authoritative snapshot. Keep the same rows
        // so the current filter, selection and editor position survive completion or a pause.
        TranslationEdits saved;
        try { saved = await TranslationEdits.LoadAsync(game.Directory); }
        catch
        {
            game.Rows = null; game.RowsById.Clear(); game.RowsSession = null;
            throw;
        }
        game.AcceptProgress = false;
        game.Edits = new(saved.Entries, StringComparer.Ordinal);
        game.Candidates = new(saved.Candidates ?? new Dictionary<string, TranslationEdit>(), StringComparer.Ordinal);
        foreach (var row in game.Rows ?? [])
        {
            var edit = saved.Find(row.Unit, game.Project.Settings.TargetLanguage);
            if (row.Edit == edit) continue;
            row.Edit = edit; row.Refresh();
        }
        InvalidateWorkbenchCache(game);
        if (selectedGame == game)
        {
            RenderEditor(); UpdateWorkbenchSummary(); UpdatePatchNotice(); QueueTextFilter();
        }
    }

    private async void Trial_Click(object sender, RoutedEventArgs e)
    {
        if (selectedGame is not { } game || game.SelectedTexts.Count == 0) return;
        await TranslateAsync(game.SelectedTexts.ToArray());
    }
    private async void TrialPatch_Click(object sender, RoutedEventArgs e)
    {
        if (selectedGame is not { } game || game.SelectedTexts.Count == 0) return;
        await TranslateAsync(game.SelectedTexts.ToArray(), buildPatchAfter: true);
    }

    private async void RetranslateSelected_Click(object sender, RoutedEventArgs e)
    {
        if (selectedGame is not { Session: { } session, Busy: false } game || game.SelectedTexts.Count == 0) return;
        try
        {
            var ids = game.SelectedTexts.ToArray();
            var choice = await new ContentDialog
            {
                XamlRoot = RootGrid.XamlRoot, Title = $"重新翻译所选 {ids.Length:N0} 条文本",
                Content = "将使用当前翻译 API 重新生成译文，包括已翻译、已编辑和已校对的所选文本。新译文通过格式检查后替换当前译文，并标为待校对；失败文本保留原译文。\n重译期间暂停本项目的编辑与校对；完成后需要重新生成补丁，已有补丁保留原来的内容。",
                PrimaryButtonText = "重新翻译并替换", CloseButtonText = "取消"
            }.ShowAsync();
            if (choice != ContentDialogResult.Primary || selectedGame != game || game.Busy || !ReferenceEquals(game.Session, session)) return;
            await TranslateAsync(ids, retranslationId: Guid.NewGuid().ToString("N"));
        }
        catch (Exception error) { ShowError(error); }
    }

    private async void GeneratePatch_Click(object sender, RoutedEventArgs e)
    {
        if (selectedGame is not { Session: { } captured, Busy: false } game || closing || changingDataDirectory) return;
        try
        {
            await FlushEditsAsync(game); await SaveSettingsAsync(); await EnsureWorkbenchAsync(game);
            if (selectedGame != game || game.Busy || !ReferenceEquals(game.Session, captured)) return;
            var scope = new ComboBox { Header = "生成范围", HorizontalAlignment = HorizontalAlignment.Stretch };
            scope.Items.Add(new ComboBoxItem { Content = $"全部已保存译文（{game.Rows?.Count(row => row.Text is not null):N0} 条）", Tag = "all" });
            scope.Items.Add(new ComboBoxItem { Content = $"仅所选文本（{game.SelectedTexts.Count:N0} 条）", Tag = "selected", IsEnabled = game.SelectedTexts.Count > 0 });
            scope.SelectedIndex = game.SelectedTexts.Count > 0 ? 1 : 0;
            var reviewed = new CheckBox { Content = "只包含已确认校对的译文" };
            var body = new StackPanel { Spacing = 16 };
            body.Children.Add(scope); body.Children.Add(reviewed);
            body.Children.Add(new TextBlock { Text = "使用生成时已保存的译文。未翻译和未选入的文本保留原文；有格式问题的译文需要修正后再生成。后续编辑不会改变这份补丁。", TextWrapping = TextWrapping.Wrap });
            var dialog = new ContentDialog { XamlRoot = RootGrid.XamlRoot, Title = "生成汉化补丁", Content = body,
                PrimaryButtonText = "生成补丁", CloseButtonText = "取消", DefaultButton = ContentDialogButton.Primary };
            if (await dialog.ShowAsync() != ContentDialogResult.Primary) return;
            if (selectedGame != game || game.Busy || !ReferenceEquals(game.Session, captured)) return;
            var ids = (scope.SelectedItem as ComboBoxItem)?.Tag as string == "selected" ? game.SelectedTexts.ToArray() : null;
            var reviewedOnly = reviewed.IsChecked == true;
            await FlushEditsAsync(game);
            var policy = ReadPolicy(game.Project.Settings);
            await StartWorkAsync(game, GameTaskKind.Patching, "生成补丁", async token =>
            {
                var progress = TranslationProgress(game);
                game.Result = await Task.Run(() => pipeline.BuildFromSavedAsync(captured, policy, ids, reviewedOnly, progress, token));
                await LoadPatchIndexAsync(game);
                SetStage(game, "补丁已生成");
                Log(game, "补丁已生成，可以安装并进入所选片段验证。后续编辑不会修改已有补丁。");
            });
            if (selectedGame == game && game.Error is null) ProjectTabs.SelectedItem = PatchTab;
        }
        catch (Exception error) { ShowError(error); }
    }

    private async Task LoadPatchIndexAsync(GameWorkspace game)
    {
        var patch = game.Result?.PatchDirectory;
        if (patch == game.PatchIndexDirectory) return;
        game.PatchTextHashes = null; game.RuntimeCheck = null; game.PatchIndexDirectory = patch;
        try
        {
            var snapshot = patch is null ? null : await LocalFiles.ReadJsonAsync<PatchTranslationSnapshot>(Path.Combine(patch, "translation-snapshot.json"), DesktopJson.TypeInfo<PatchTranslationSnapshot>(DesktopJson.Storage));
            game.PatchTextHashes = snapshot?.Texts is null ? null : await Task.Run(() => snapshot.Texts.ToDictionary(pair => pair.Key, pair => LocalFiles.HashText(pair.Value), StringComparer.Ordinal));
            var runtime = patch is null ? null : await LocalFiles.ReadJsonAsync<RuntimeCheck>(Path.Combine(patch, "runtime-check.json"), DesktopJson.TypeInfo<RuntimeCheck>(DesktopJson.Storage));
            game.RuntimeCheck = runtime is null ? null : $"用户记录 · {runtime.CheckedAt.ToLocalTime():MM-dd HH:mm} · {(runtime.Status == "passed" ? "显示正常" : "存在问题")}"
                + (!string.IsNullOrEmpty(runtime.Note) ? "\n" + runtime.Note : "");
        }
        catch (Exception error) when (error is IOException or System.Text.Json.JsonException or UnauthorizedAccessException)
        {
            Program.WriteStartupError(error);
            Log(game, "部分补丁记录无法读取，可以继续校对并重新生成补丁。");
        }
    }

    private void UpdatePatchNotice()
    {
        var game = selectedGame;
        var changed = game?.PatchTextHashes is { } hashes && game.Rows is not null
            && hashes.Any(pair => !game.RowsById.TryGetValue(pair.Key, out var row) || row.Text is null || row.TextHash != pair.Value);
        PatchChangedInfo.IsOpen = changed;
        RuntimeCheckText.Text = game?.RuntimeCheck ?? "尚未记录游戏内验证结果。结构校验通过不代表中文显示已验证。";
    }

    private sealed record PatchTextPreview(string Label);

    private async void ShowPatchTexts_Click(object sender, RoutedEventArgs e)
    {
        if (selectedGame is not { Result: { } result } game) return;
        try
        {
            var snapshot = await LocalFiles.ReadJsonAsync<PatchTranslationSnapshot>(Path.Combine(result.PatchDirectory, "translation-snapshot.json"), DesktopJson.TypeInfo<PatchTranslationSnapshot>(DesktopJson.Storage));
            if (snapshot?.Texts is null) return;
            var rows = game.RowsById.ToDictionary(pair => pair.Key, pair => pair.Value.Unit, StringComparer.Ordinal);
            var preview = await Task.Run(() => snapshot.Texts.Select(pair => new PatchTextPreview(rows.TryGetValue(pair.Key, out var unit)
                ? $"{unit.ResourcePath} · {unit.Locator}\n原文：{unit.SourceText}\n补丁译文：{pair.Value}"
                : pair.Key + "\n补丁译文：" + pair.Value)).ToArray());
            if (selectedGame != game) return;
            var list = new ListView { ItemsSource = preview, Height = 420, HorizontalContentAlignment = HorizontalAlignment.Stretch,
                SelectionMode = ListViewSelectionMode.None,
                ItemTemplate = (DataTemplate)XamlReader.Load("""
                    <DataTemplate xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation">
                      <TextBlock Text="{Binding Label}" TextWrapping="Wrap" IsTextSelectionEnabled="True" Margin="0,8" />
                    </DataTemplate>
                    """) };
            var dialog = new ContentDialog { XamlRoot = RootGrid.XamlRoot, Title = $"补丁使用的译文 · {preview.Length:N0} 条", Content = list, CloseButtonText = "关闭" };
            dialog.Resources["ContentDialogMaxWidth"] = 820d;
            await dialog.ShowAsync();
        }
        catch (Exception error) { ShowError(error); }
    }

    private async void RecordRuntime_Click(object sender, RoutedEventArgs e)
    {
        if (selectedGame is not { Result: { } result } game || sender is not Button { Tag: string status }) return;
        try
        {
            var note = new TextBox { Header = "验证说明（可选）", PlaceholderText = "例如：开场对白正常；选择项仍有乱码", AcceptsReturn = true, TextWrapping = TextWrapping.Wrap, MinHeight = 100 };
            var dialog = new ContentDialog { XamlRoot = RootGrid.XamlRoot, Title = status == "passed" ? "记录游戏内显示正常" : "记录游戏内显示问题",
                Content = note, PrimaryButtonText = "保存记录", CloseButtonText = "取消" };
            if (await dialog.ShowAsync() != ContentDialogResult.Primary || game.Result != result) return;
            await LocalFiles.WriteJsonAsync(Path.Combine(result.PatchDirectory, "runtime-check.json"), new RuntimeCheck(status, note.Text.Trim(), DateTimeOffset.UtcNow), DesktopJson.TypeInfo<RuntimeCheck>(DesktopJson.Storage));
            game.PatchIndexDirectory = null; await LoadPatchIndexAsync(game); if (selectedGame == game) UpdatePatchNotice();
        }
        catch (Exception error) { ShowError(error); }
    }

    private async void Candidate_Click(object sender, RoutedEventArgs e)
    {
        if (selectedGame is not { Session: { } captured, Busy: false } game || editingRow is not { } row) return;
        try
        {
            await FlushEditsAsync(game); await SaveSettingsAsync();
            var savedCandidate = game.Candidates.GetValueOrDefault(row.Unit.Id);
            var candidate = savedCandidate?.SourceSignature == row.Signature && savedCandidate.Language == game.Project.Settings.TargetLanguage
                ? savedCandidate.Text : null;
            if (candidate is null)
            {
                var providers = ResolveGameApi(game).ResolveTranslationChain().Select(profile => (ITranslationProvider)CreateProvider(game, profile)).ToArray();
                var policy = ReadPolicy(game.Project.Settings);
                await StartWorkAsync(game, GameTaskKind.Translation, "生成候选译文", async token =>
                {
                    var progress = TranslationProgress(game);
                    candidate = await Task.Run(() => pipeline.CreateCandidateAsync(captured, row.Unit, providers, policy, progress, token));
                    game.Candidates[row.Unit.Id] = new(row.Signature, policy.TargetLanguage, candidate, false, DateTimeOffset.UtcNow);
                    game.EditsDirty = true; game.EditRevision++;
                    await FlushEditsAsync(game);
                    Log(game, "候选译文已就绪，点击该文本的“重新翻译并对比”即可查看。当前译文未改变。");
                    SetStage(game, "候选译文待对比");
                });
            }
            if (candidate is null || selectedGame != game || !ReferenceEquals(game.Session, captured) || closing) return;
            var body = new StackPanel { Spacing = 12 };
            body.Children.Add(new TextBlock { Text = "原文", FontWeight = Microsoft.UI.Text.FontWeights.SemiBold });
            body.Children.Add(new TextBlock { Text = row.Source, TextWrapping = TextWrapping.Wrap, IsTextSelectionEnabled = true });
            body.Children.Add(new TextBlock { Text = "当前译文", FontWeight = Microsoft.UI.Text.FontWeights.SemiBold });
            body.Children.Add(new TextBlock { Text = row.Text ?? "尚未翻译", TextWrapping = TextWrapping.Wrap, IsTextSelectionEnabled = true });
            body.Children.Add(new TextBlock { Text = "候选译文", FontWeight = Microsoft.UI.Text.FontWeights.SemiBold });
            body.Children.Add(new TextBlock { Text = candidate, TextWrapping = TextWrapping.Wrap, IsTextSelectionEnabled = true });
            var dialog = new ContentDialog { XamlRoot = RootGrid.XamlRoot, Title = "对比候选译文",
                Content = new ScrollViewer { Content = body, MaxHeight = 500 }, PrimaryButtonText = "采用候选", SecondaryButtonText = "丢弃候选", CloseButtonText = "稍后处理" };
            var answer = await dialog.ShowAsync();
            if (answer == ContentDialogResult.Primary && selectedGame == game && ReferenceEquals(game.Session, captured))
            { SetEdit(game, row, candidate, false); if (editingRow == row) RenderEditor(); }
            if (answer is ContentDialogResult.Primary or ContentDialogResult.Secondary)
            {
                game.Candidates.Remove(row.Unit.Id); game.EditsDirty = true; game.EditRevision++;
                await FlushEditsAsync(game);
            }
            if (selectedGame == game) UpdateWorkbenchControls();
        }
        catch (Exception error) { ShowError(error); }
    }
}
