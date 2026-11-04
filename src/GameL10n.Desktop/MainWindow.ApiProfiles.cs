using System.Security.Cryptography;
using GameL10n.Core.Providers;
using GameL10n.Desktop.Settings;
using GameL10n.Core.Storage;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;

namespace GameL10n.Desktop;

public sealed partial class MainWindow
{
    private ApiProfileConfiguration apiConfiguration = new([]);
    private readonly Dictionary<string, string> sessionApiKeys = new(StringComparer.Ordinal);
    private bool refreshingApiProfiles;
    private bool savingApiProfiles;
    private int pendingApiSelections;

    private void RefreshApiProfiles(string? selectedId = null)
    {
        refreshingApiProfiles = true;
        try
        {
            selectedId ??= (ApiProfileList.SelectedItem as ApiProfile)?.Id;
            var shared = globalSettings.ReadApiConfiguration();
            apiConfiguration = selectedGame is { } game ? ResolveGameApi(game) : shared;
            var profiles = apiConfiguration.Profiles.ToArray();
            PopulateApiChoices(apiConfiguration, AnalysisApiBox, TranslationChainList, TranslationCandidateBox, TranslationChainEmptyText);
            PopulateApiChoices(shared, DefaultAnalysisApiBox, DefaultTranslationChainList, DefaultTranslationCandidateBox, DefaultTranslationChainEmptyText);
            UseGlobalAnalysisApiBox.IsChecked = selectedGame?.Project.Settings.AnalysisProfileId is null;
            UseGlobalTranslationApiBox.IsChecked = selectedGame?.Project.Settings.TranslationProfileIds is null;
            ApiProfileList.ItemsSource = profiles;
            ApiProfileList.SelectedItem = profiles.FirstOrDefault(profile => profile.Id == selectedId) ?? profiles.FirstOrDefault();
            ApiEmptyText.Visibility = profiles.Length == 0 ? Visibility.Visible : Visibility.Collapsed;
            ApiProfileList.Visibility = profiles.Length == 0 ? Visibility.Collapsed : Visibility.Visible;
            var chain = TranslationChainList.Items.OfType<ApiProfile>().ToArray();
            TaskApiSummaryText.Text = $"分析 API（{(UseGlobalAnalysisApiBox.IsChecked == true ? "全局默认" : "项目独立")}）：{(AnalysisApiBox.SelectedItem as ApiProfile)?.Name ?? "未配置"}　翻译 API 链（{(UseGlobalTranslationApiBox.IsChecked == true ? "全局默认" : "项目独立")}）：{(chain.Length == 0 ? "未配置" : string.Join(" → ", chain.Select(profile => profile.Name)))}";
        }
        finally { refreshingApiProfiles = false; UpdateApiControls(); }
    }

    private static void PopulateApiChoices(ApiProfileConfiguration configuration, ComboBox analysis,
        ListView chainList, ComboBox candidate, TextBlock emptyText)
    {
        var profiles = configuration.Profiles.ToArray();
        static ApiProfile MissingProfile(string id) => new(id, id.Length == 0 ? "尚未选择 API" : "已删除的 API（请重新选择）", "", "");
        analysis.ItemsSource = configuration.AnalysisProfileId is { } analysisId && !profiles.Any(profile => profile.Id == analysisId)
            ? [.. profiles, MissingProfile(analysisId)] : profiles;
        analysis.SelectedItem = analysis.Items.OfType<ApiProfile>().FirstOrDefault(profile => profile.Id == configuration.AnalysisProfileId);
        var selectedId = (chainList.SelectedItem as ApiProfile)?.Id;
        var candidateId = (candidate.SelectedItem as ApiProfile)?.Id;
        var ids = configuration.GetTranslationProfileIds();
        var chain = ids.Select(id => profiles.FirstOrDefault(profile => profile.Id == id) ?? MissingProfile(id)).ToArray();
        chainList.ItemsSource = chain;
        chainList.SelectedItem = chain.FirstOrDefault(profile => profile.Id == selectedId) ?? chain.FirstOrDefault();
        var candidates = profiles.Where(profile => !ids.Contains(profile.Id)).ToArray();
        candidate.ItemsSource = candidates;
        candidate.SelectedItem = candidates.FirstOrDefault(profile => profile.Id == candidateId) ?? candidates.FirstOrDefault();
        emptyText.Visibility = chain.Length == 0 ? Visibility.Visible : Visibility.Collapsed;
    }

    private void UpdateApiControls()
    {
        var editable = initialized && !closing && !changingDataDirectory && !savingApiProfiles && pendingApiSelections == 0;
        var projectEditable = editable && selectedGame is not null;
        UseGlobalAnalysisApiBox.IsEnabled = projectEditable;
        UseGlobalTranslationApiBox.IsEnabled = projectEditable;
        AnalysisApiBox.IsEnabled = projectEditable && UseGlobalAnalysisApiBox.IsChecked != true && apiConfiguration.Profiles.Count > 0;
        DefaultAnalysisApiBox.IsEnabled = editable && apiConfiguration.Profiles.Count > 0;
        AddApiButton.IsEnabled = ApiProfileList.IsEnabled = editable;
        EditApiButton.IsEnabled = DeleteApiButton.IsEnabled = editable && ApiProfileList.SelectedItem is ApiProfile;
        UpdateChainControls(projectEditable && UseGlobalTranslationApiBox.IsChecked != true,
            TranslationChainList, TranslationCandidateBox, AddTranslationApiButton, MoveTranslationUpButton, MoveTranslationDownButton, RemoveTranslationApiButton);
        UpdateChainControls(editable, DefaultTranslationChainList, DefaultTranslationCandidateBox,
            AddDefaultTranslationApiButton, MoveDefaultTranslationUpButton, MoveDefaultTranslationDownButton, RemoveDefaultTranslationApiButton);
        UpdateProjectAdaptationControls(projectEditable);
        if (initialized) UpdateControls();
    }

    private static void UpdateChainControls(bool editable, ListView chain, ComboBox candidate,
        Button add, Button up, Button down, Button remove)
    {
        chain.IsEnabled = editable;
        candidate.IsEnabled = editable && candidate.Items.Count > 0;
        add.IsEnabled = editable && candidate.SelectedItem is ApiProfile;
        up.IsEnabled = editable && chain.SelectedIndex > 0;
        down.IsEnabled = editable && chain.SelectedIndex >= 0 && chain.SelectedIndex < chain.Items.Count - 1;
        remove.IsEnabled = editable && chain.SelectedIndex >= 0;
    }

    private void ApiProfileList_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (initialized && !refreshingApiProfiles) UpdateApiControls();
    }

    private async void ApiPurpose_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (!initialized || refreshingApiProfiles || savingApiProfiles || pendingApiSelections > 0) return;
        var box = (ComboBox)sender;
        var isGlobal = box == DefaultAnalysisApiBox;
        if (!isGlobal && selectedGame is null) return;
        var id = (box.SelectedItem as ApiProfile)?.Id;
        if (id is null || id == (isGlobal ? globalSettings.ReadApiConfiguration() : apiConfiguration).AnalysisProfileId) return;
        pendingApiSelections++;
        UpdateApiControls();
        try
        {
            if (isGlobal) await SaveSharedApiProfilesAsync(current => current.Select(ApiPurpose.GameAnalysis, id));
            else await SaveSettingsAsync(current => current with { AnalysisProfileId = id });
            ShowApiScopeStatus(isGlobal, isGlobal ? "默认分析 API 已保存，跟随全局默认的项目将在下次任务使用。" : "项目分析 API 已保存，后续任务使用新设置。");
        }
        catch (Exception error) { ShowApiScopeStatus(isGlobal, error.Message, isError: true); }
        finally { if (--pendingApiSelections == 0) RefreshApiProfiles(); }
    }

    private void TranslationChain_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (initialized && !refreshingApiProfiles) UpdateApiControls();
    }

    private async Task ChangeTranslationChainAsync(bool isGlobal, Func<ApiProfileConfiguration, ApiProfileConfiguration> change)
    {
        if (savingApiProfiles || pendingApiSelections > 0) return;
        savingApiProfiles = true;
        UpdateApiControls();
        try
        {
            if (isGlobal) await SaveSharedApiProfilesAsync(change);
            else await SaveSettingsAsync(current => current with
            {
                TranslationProfileIds = change(current.ResolveApiConfiguration(globalSettings.ReadApiConfiguration())).GetTranslationProfileIds().ToArray()
            });
            ShowApiScopeStatus(isGlobal, isGlobal ? "默认翻译 API 链已保存，跟随全局默认的项目将在下次任务使用。" : "项目翻译 API 链已保存，后续任务使用新顺序。");
        }
        catch (Exception error) { ShowApiScopeStatus(isGlobal, error.Message, isError: true); }
        finally { savingApiProfiles = false; RefreshApiProfiles(); }
    }

    private async void AddTranslationApi_Click(object sender, RoutedEventArgs e)
    {
        var isGlobal = (Button)sender == AddDefaultTranslationApiButton;
        if ((isGlobal ? DefaultTranslationCandidateBox : TranslationCandidateBox).SelectedItem is ApiProfile profile)
            await ChangeTranslationChainAsync(isGlobal, current => current.SetTranslationChain([.. current.GetTranslationProfileIds(), profile.Id]));
    }

    private async void RemoveTranslationApi_Click(object sender, RoutedEventArgs e)
    {
        var isGlobal = (Button)sender == RemoveDefaultTranslationApiButton;
        if ((isGlobal ? DefaultTranslationChainList : TranslationChainList).SelectedItem is ApiProfile profile)
            await ChangeTranslationChainAsync(isGlobal, current => current with
            {
                TranslationProfileIds = current.GetTranslationProfileIds().Where(id => id != profile.Id).ToArray(), TranslationProfileId = null
            });
    }

    private async Task MoveTranslationApiAsync(int direction, bool isGlobal)
    {
        if ((isGlobal ? DefaultTranslationChainList : TranslationChainList).SelectedItem is not ApiProfile profile) return;
        await ChangeTranslationChainAsync(isGlobal, current =>
        {
            var ids = current.GetTranslationProfileIds().ToArray();
            var index = Array.IndexOf(ids, profile.Id);
            var destination = index + direction;
            if (index < 0 || destination < 0 || destination >= ids.Length) return current;
            (ids[index], ids[destination]) = (ids[destination], ids[index]);
            return current.SetTranslationChain(ids);
        });
    }

    private async void MoveTranslationUp_Click(object sender, RoutedEventArgs e) => await MoveTranslationApiAsync(-1, (Button)sender == MoveDefaultTranslationUpButton);
    private async void MoveTranslationDown_Click(object sender, RoutedEventArgs e) => await MoveTranslationApiAsync(1, (Button)sender == MoveDefaultTranslationDownButton);

    private async void ApiInheritance_Click(object sender, RoutedEventArgs e)
    {
        if (selectedGame is null || savingApiProfiles || pendingApiSelections > 0) return;
        var analysis = (CheckBox)sender == UseGlobalAnalysisApiBox;
        var inherit = ((CheckBox)sender).IsChecked == true;
        var effective = apiConfiguration;
        pendingApiSelections++;
        UpdateApiControls();
        try
        {
            await SaveSettingsAsync(current => analysis
                ? current with { AnalysisProfileId = inherit ? null : effective.AnalysisProfileId ?? "" }
                : current with { TranslationProfileIds = inherit ? null : effective.GetTranslationProfileIds().ToArray() });
            ShowApiStatus(inherit ? "已跟随全局默认，后续任务使用最新默认配置。" : "已设为项目独立配置，可在下方调整。");
        }
        catch (Exception error) { ShowApiStatus(error.Message, true); }
        finally { pendingApiSelections--; RefreshApiProfiles(); }
    }

    private async void AddApi_Click(object sender, RoutedEventArgs e) => await EditApiProfileAsync(null);
    private async void EditApi_Click(object sender, RoutedEventArgs e)
    {
        if (ApiProfileList.SelectedItem is ApiProfile profile) await EditApiProfileAsync(profile);
    }

    private async Task EditApiProfileAsync(ApiProfile? existing)
    {
        if (savingApiProfiles) return;
        var name = new TextBox { Header = "配置名称", PlaceholderText = "例如：通用模型、剧情翻译", Text = existing?.Name ?? "", MaxLength = 80 };
        var protocol = new ComboBox { Header = "API 协议", HorizontalAlignment = HorizontalAlignment.Stretch,
            ItemsSource = new[] { "Chat Completions", "OpenAI Responses" }, SelectedIndex = existing?.Protocol == ProviderApiProtocol.Responses ? 1 : 0 };
        var endpoint = new TextBox { Header = "API 基础地址或完整接口地址", PlaceholderText = "https://api.openai.com/v1", Text = existing?.Endpoint ?? "" };
        var model = new TextBox { Header = "模型名称", PlaceholderText = "填写服务商提供的模型名称", Text = existing?.Model ?? "", MaxLength = 256 };
        var translationFormat = new ComboBox { Header = "翻译输入格式", HorizontalAlignment = HorizontalAlignment.Stretch,
            ItemsSource = new[] { "批量 JSON（通用模型）", "逐条纯文本（GalTransl 等专用翻译模型）" },
            SelectedIndex = existing?.TranslationFormat == TranslationInputFormat.PlainText ? 1 : 0 };
        var translationHelp = new HelpButton { VerticalAlignment = VerticalAlignment.Bottom, Margin = new Thickness(0, 0, 0, 2),
            Text = "逐条纯文本只发送一条原文，风格与相关术语放在 system 中。成功一条保存一条；翻译链含此模式时整条链按单条处理。此选项不改变智能识别请求。" };
        AutomationProperties.SetName(translationHelp, "翻译输入格式说明");
        Grid.SetColumn(translationHelp, 1);
        var translationField = new Grid { ColumnSpacing = 6,
            ColumnDefinitions = { new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) }, new ColumnDefinition { Width = GridLength.Auto } },
            Children = { translationFormat, translationHelp } };
        string?[] effortValues = [null, "low", "medium", "high", "xhigh"];
        var effort = new ComboBox { Header = "分析思考等级", HorizontalAlignment = HorizontalAlignment.Stretch,
            ItemsSource = new[] { "接口默认（不指定）", "low", "medium", "high", "xhigh" },
            SelectedIndex = Math.Max(0, Array.IndexOf(effortValues, existing?.AnalysisReasoningEffort)) };
        var effortHelp = new HelpButton { VerticalAlignment = VerticalAlignment.Bottom, Margin = new Thickness(0, 0, 0, 2),
            Text = "仅用于智能识别与适配。支持的等级取决于模型；更高等级可能更慢。不确定时使用接口默认。" };
        AutomationProperties.SetName(effortHelp, "分析思考等级说明");
        Grid.SetColumn(effortHelp, 1);
        var effortField = new Grid { ColumnSpacing = 6,
            ColumnDefinitions = { new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) }, new ColumnDefinition { Width = GridLength.Auto } },
            Children = { effort, effortHelp } };
        var key = new PasswordBox { Header = "API Key", MaxLength = 8192 };
        var rememberKey = new CheckBox { Content = "记住 API Key（在本机加密保存）", IsChecked = existing?.RequiresSessionKey != true };
        var errorBar = new InfoBar { IsOpen = false, IsClosable = false, Severity = InfoBarSeverity.Error };
        try { key.Password = existing is null ? "" : ReadProfileKey(existing); }
        catch (InvalidOperationException error) { errorBar.Message = error.Message; errorBar.IsOpen = true; }
        var keyChanged = false;
        key.PasswordChanged += (_, _) => keyChanged = true;
        AutomationProperties.SetAutomationId(name, "ProfileNameInput");
        AutomationProperties.SetAutomationId(protocol, "ProfileProtocolInput");
        AutomationProperties.SetAutomationId(endpoint, "ProfileEndpointInput");
        AutomationProperties.SetAutomationId(model, "ProfileModelInput");
        AutomationProperties.SetAutomationId(translationFormat, "ProfileTranslationFormatInput");
        AutomationProperties.SetAutomationId(effort, "ProfileAnalysisReasoningEffortInput");
        AutomationProperties.SetAutomationId(key, "ProfileApiKeyInput");
        AutomationProperties.SetAutomationId(rememberKey, "RememberApiKeyInput");
        var form = new StackPanel { Spacing = 12 };
        foreach (var element in new FrameworkElement[] { name, protocol, endpoint, model, translationField, effortField, key, rememberKey, errorBar }) form.Children.Add(element);
        var dialog = new ContentDialog { XamlRoot = RootGrid.XamlRoot, Title = existing is null ? "新增全局 API 配置" : "编辑全局 API 配置",
            Content = new ScrollViewer { Content = form, MaxHeight = 480, VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
                HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled },
            PrimaryButtonText = "保存", CloseButtonText = "取消", DefaultButton = ContentDialogButton.Primary };
        dialog.PrimaryButtonClick += async (dialogSender, args) =>
        {
            var deferral = args.GetDeferral();
            savingApiProfiles = true;
            UpdateApiControls();
            try
            {
                var selectedProtocol = protocol.SelectedIndex == 1 ? ProviderApiProtocol.Responses : ProviderApiProtocol.ChatCompletions;
                var selectedEffort = effortValues[effort.SelectedIndex];
                var selectedTranslationFormat = translationFormat.SelectedIndex == 1 ? TranslationInputFormat.PlainText : TranslationInputFormat.JsonBatch;
                var secret = key.Password.Trim();
                // Validate endpoint/model/protocol/auth locally; editing settings never sends a request.
                _ = new OpenAiCompatibleProvider(httpClient, new ProviderSettings(endpoint.Text.Trim(), secret, model.Text.Trim(), selectedProtocol, selectedEffort, selectedTranslationFormat));
                var remember = rememberKey.IsChecked == true;
                var keepKey = existing is not null && !keyChanged && remember == !existing.RequiresSessionKey;
                var encrypted = keepKey ? existing!.EncryptedApiKey : remember ? ApiKeyProtection.Protect(secret) : null;
                var requiresSessionKey = keepKey ? existing!.RequiresSessionKey : !remember && secret.Length > 0;
                var profile = new ApiProfile(existing?.Id ?? Guid.NewGuid().ToString("N"), name.Text.Trim(), endpoint.Text.Trim(),
                    model.Text.Trim(), selectedProtocol, encrypted, requiresSessionKey, selectedEffort, selectedTranslationFormat);
                await SaveSharedApiProfilesAsync(current => current.Save(profile));
                if (!keepKey)
                {
                    if (requiresSessionKey) sessionApiKeys[profile.Id] = secret;
                    else sessionApiKeys.Remove(profile.Id);
                }
                RefreshApiProfiles(profile.Id);
                ShowSharedApiStatus($"已保存全局 API“{profile.Name}”。可设为全局默认，也可在项目中单独选择。");
            }
            catch (Exception error)
            {
                args.Cancel = true;
                errorBar.Message = error is CryptographicException ? "Windows 无法加密密钥。可取消“记住 API Key”，仅在本次运行中使用。" : error.Message;
                errorBar.IsOpen = true;
            }
            finally { savingApiProfiles = false; UpdateApiControls(); deferral.Complete(); }
        };
        try { await dialog.ShowAsync(); }
        catch (Exception error) { ShowSharedApiStatus(error.Message, isError: true); }
        finally { key.Password = ""; }
    }

    private async void DeleteApi_Click(object sender, RoutedEventArgs e)
    {
        if (savingApiProfiles || ApiProfileList.SelectedItem is not ApiProfile profile) return;
        savingApiProfiles = true;
        UpdateApiControls();
        try
        {
            var used = games.Count(game => ResolveGameApi(game).AnalysisProfileId == profile.Id
                || ResolveGameApi(game).GetTranslationProfileIds().Contains(profile.Id));
            await SaveSharedApiProfilesAsync(current => current.Remove(profile.Id));
            sessionApiKeys.Remove(profile.Id);
            ShowSharedApiStatus($"已删除全局 API“{profile.Name}”。" + (used > 0 ? $"{used} 个项目使用了此配置，请检查全局默认或项目独立配置。" : ""));
        }
        catch (Exception error) { ShowSharedApiStatus(error.Message, isError: true); }
        finally { savingApiProfiles = false; RefreshApiProfiles(); }
    }

    private string ReadProfileKey(ApiProfile profile)
    {
        if (!profile.RequiresSessionKey) return ApiKeyProtection.Unprotect(profile.EncryptedApiKey);
        return sessionApiKeys.TryGetValue(profile.Id, out var key) ? key
            : throw new InvalidOperationException($"“{profile.Name}”的密钥仅保留在上次运行中，请编辑配置重新填写 API Key。");
    }

    private async Task SaveSharedApiProfilesAsync(Func<ApiProfileConfiguration, ApiProfileConfiguration> change)
    {
        await settingsWriteLock.WaitAsync();
        try
        {
            var shared = change(globalSettings.ReadApiConfiguration());
            var settings = globalSettings with { ApiConfiguration = shared };
            await LocalFiles.WriteJsonAsync(Path.Combine(dataDirectory, "settings.json"), settings);
            globalSettings = settings;
            apiConfiguration = selectedGame is { } game ? ResolveGameApi(game) : shared;
        }
        finally { settingsWriteLock.Release(); }
    }

    private void ShowApiScopeStatus(bool isGlobal, string message, bool isError = false)
    {
        if (isGlobal) ShowSharedApiStatus(message, isError);
        else ShowApiStatus(message, isError);
    }

    private void ShowSharedApiStatus(string message, bool isError = false)
    {
        SharedApiInfo.Severity = isError ? InfoBarSeverity.Error : InfoBarSeverity.Success;
        SharedApiInfo.Message = message;
        SharedApiInfo.IsOpen = true;
    }

    private void ShowApiStatus(string message, bool isError = false)
    {
        ApiSettingsInfo.Severity = isError ? InfoBarSeverity.Error : InfoBarSeverity.Success;
        ApiSettingsInfo.Message = message;
        ApiSettingsInfo.IsOpen = true;
    }
}
