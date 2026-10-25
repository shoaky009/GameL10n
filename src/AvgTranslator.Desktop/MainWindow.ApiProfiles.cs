using System.Security.Cryptography;
using AvgTranslator.Core.Providers;
using AvgTranslator.Desktop.Settings;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;

namespace AvgTranslator.Desktop;

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
            var profiles = apiConfiguration.Profiles.ToArray();
            AnalysisApiBox.ItemsSource = profiles;
            TranslationApiBox.ItemsSource = profiles;
            AnalysisApiBox.SelectedItem = profiles.FirstOrDefault(profile => profile.Id == apiConfiguration.AnalysisProfileId);
            TranslationApiBox.SelectedItem = profiles.FirstOrDefault(profile => profile.Id == apiConfiguration.TranslationProfileId);
            ApiProfileList.ItemsSource = profiles;
            ApiProfileList.SelectedItem = profiles.FirstOrDefault(profile => profile.Id == selectedId) ?? profiles.FirstOrDefault();
            ApiEmptyText.Visibility = profiles.Length == 0 ? Visibility.Visible : Visibility.Collapsed;
            ApiProfileList.Visibility = profiles.Length == 0 ? Visibility.Collapsed : Visibility.Visible;
            TaskApiSummaryText.Text = $"分析 API：{(AnalysisApiBox.SelectedItem as ApiProfile)?.Name ?? "未配置"}　翻译 API：{(TranslationApiBox.SelectedItem as ApiProfile)?.Name ?? "未配置"}";
        }
        finally { refreshingApiProfiles = false; UpdateApiControls(); }
    }

    private void UpdateApiControls()
    {
        AnalysisApiBox.IsEnabled = TranslationApiBox.IsEnabled = !savingApiProfiles && apiConfiguration.Profiles.Count > 0;
        AddApiButton.IsEnabled = ApiProfileList.IsEnabled = !savingApiProfiles;
        EditApiButton.IsEnabled = DeleteApiButton.IsEnabled = !savingApiProfiles && ApiProfileList.SelectedItem is ApiProfile;
    }

    private void ApiProfileList_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (initialized && !refreshingApiProfiles) UpdateApiControls();
    }

    private async void ApiPurpose_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (!initialized || refreshingApiProfiles || savingApiProfiles) return;
        var box = (ComboBox)sender;
        var purpose = box == AnalysisApiBox ? ApiPurpose.GameAnalysis : ApiPurpose.Translation;
        var id = (box.SelectedItem as ApiProfile)?.Id;
        if (id == (purpose == ApiPurpose.GameAnalysis ? apiConfiguration.AnalysisProfileId : apiConfiguration.TranslationProfileId)) return;
        pendingApiSelections++;
        try
        {
            await SaveSettingsAsync(current => current.Select(purpose, id));
            ShowApiStatus("API 用途选择已保存。运行中的任务继续使用启动时的配置。");
        }
        catch (Exception error) { ShowApiStatus(error.Message, isError: true); }
        finally { if (--pendingApiSelections == 0) RefreshApiProfiles(); }
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
        AutomationProperties.SetAutomationId(key, "ProfileApiKeyInput");
        AutomationProperties.SetAutomationId(rememberKey, "RememberApiKeyInput");
        var form = new StackPanel { Spacing = 12 };
        foreach (var element in new FrameworkElement[] { name, protocol, endpoint, model, key, rememberKey, errorBar }) form.Children.Add(element);
        var dialog = new ContentDialog { XamlRoot = RootGrid.XamlRoot, Title = existing is null ? "新增 API 配置" : "编辑 API 配置",
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
                var secret = key.Password.Trim();
                // Validate endpoint/model/protocol/auth locally; editing settings never sends a request.
                _ = new OpenAiCompatibleProvider(httpClient, new ProviderSettings(endpoint.Text.Trim(), secret, model.Text.Trim(), selectedProtocol));
                var remember = rememberKey.IsChecked == true;
                var keepKey = existing is not null && !keyChanged && remember == !existing.RequiresSessionKey;
                var encrypted = keepKey ? existing!.EncryptedApiKey : remember ? ApiKeyProtection.Protect(secret) : null;
                var requiresSessionKey = keepKey ? existing!.RequiresSessionKey : !remember && secret.Length > 0;
                var profile = new ApiProfile(existing?.Id ?? Guid.NewGuid().ToString("N"), name.Text.Trim(), endpoint.Text.Trim(),
                    model.Text.Trim(), selectedProtocol, encrypted, requiresSessionKey);
                await SaveSettingsAsync(current => current.Save(profile));
                if (!keepKey)
                {
                    if (requiresSessionKey) sessionApiKeys[profile.Id] = secret;
                    else sessionApiKeys.Remove(profile.Id);
                }
                RefreshApiProfiles(profile.Id);
                ShowApiStatus($"已保存“{profile.Name}”。");
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
        catch (Exception error) { ShowApiStatus(error.Message, isError: true); }
        finally { key.Password = ""; }
    }

    private async void DeleteApi_Click(object sender, RoutedEventArgs e)
    {
        if (savingApiProfiles || ApiProfileList.SelectedItem is not ApiProfile profile) return;
        savingApiProfiles = true;
        UpdateApiControls();
        try
        {
            var wasSelected = apiConfiguration.AnalysisProfileId == profile.Id || apiConfiguration.TranslationProfileId == profile.Id;
            await SaveSettingsAsync(current => current.Remove(profile.Id));
            sessionApiKeys.Remove(profile.Id);
            ShowApiStatus($"已删除“{profile.Name}”。" + (wasSelected ? "相关用途已清空，请重新选择 API。" : ""));
        }
        catch (Exception error) { ShowApiStatus(error.Message, isError: true); }
        finally { savingApiProfiles = false; RefreshApiProfiles(); }
    }

    private string ReadProfileKey(ApiProfile profile)
    {
        if (!profile.RequiresSessionKey) return ApiKeyProtection.Unprotect(profile.EncryptedApiKey);
        return sessionApiKeys.TryGetValue(profile.Id, out var key) ? key
            : throw new InvalidOperationException($"“{profile.Name}”的密钥仅保留在上次运行中，请编辑配置重新填写 API Key。");
    }

    private void ShowApiStatus(string message, bool isError = false)
    {
        ApiSettingsInfo.Severity = isError ? InfoBarSeverity.Error : InfoBarSeverity.Success;
        ApiSettingsInfo.Message = message;
        ApiSettingsInfo.IsOpen = true;
    }
}
