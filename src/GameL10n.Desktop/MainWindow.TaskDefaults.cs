using GameL10n.Core.Development;
using GameL10n.Core.Storage;
using Microsoft.UI.Xaml;

namespace GameL10n.Desktop;

public sealed partial class MainWindow
{
    private AdaptationLimits ResolveGameAdaptationLimits(GameWorkspace game) =>
        game.Project.Settings.ResolveAdaptationLimits(new AdaptationLimits(
            globalSettings.AdaptationMaximumTurns, globalSettings.AdaptationTokenBudget));

    private static void ValidateAdaptationValues(double budget, double turns)
    {
        if (!double.IsFinite(budget) || budget is < 1000 or > 20_000_000 || budget != Math.Truncate(budget)
            || !double.IsFinite(turns) || turns is < 1 or > 400 || turns != Math.Truncate(turns))
            throw new InvalidDataException("Token 预算须为 1,000 至 20,000,000 的整数，模型轮数须为 1 至 400 的整数。");
    }

    private void RefreshProjectAdaptationSettings()
    {
        if (selectedGame is not { } game) return;
        var limits = ResolveGameAdaptationLimits(game);
        UseGlobalAdaptationLimitsBox.IsChecked = game.Project.Settings.AdaptationTokenBudget is null
            && game.Project.Settings.AdaptationMaximumTurns is null;
        ProjectTokenBudgetBox.Value = limits.MaximumTokens;
        ProjectMaximumTurnsBox.Value = limits.MaximumTurns;
        UpdateProjectAdaptationControls(initialized && !closing && !changingDataDirectory);
    }

    private void UpdateProjectAdaptationControls(bool editable)
    {
        UseGlobalAdaptationLimitsBox.IsEnabled = editable;
        ProjectTokenBudgetBox.IsEnabled = ProjectMaximumTurnsBox.IsEnabled =
            editable && UseGlobalAdaptationLimitsBox.IsChecked != true;
    }

    private async void AdaptationInheritance_Click(object sender, RoutedEventArgs e)
    {
        if (selectedGame is null || savingApiProfiles || pendingApiSelections > 0) return;
        pendingApiSelections++;
        UpdateApiControls();
        try
        {
            await SaveSettingsAsync();
            ShowApiStatus(UseGlobalAdaptationLimitsBox.IsChecked == true
                ? "适配预算与轮数已跟随全局默认。" : "适配预算与轮数已设为项目独立配置，修改后请保存项目设置。");
        }
        catch (Exception error) { ShowApiStatus(error.Message, true); }
        finally { pendingApiSelections--; RefreshProjectAdaptationSettings(); UpdateApiControls(); }
    }

    private async void SaveGlobalLimits_Click(object sender, RoutedEventArgs e)
    {
        if (!initialized || closing || changingDataDirectory || savingApiProfiles || pendingApiSelections > 0) return;
        pendingApiSelections++;
        UpdateApiControls();
        try
        {
            ValidateAdaptationValues(DefaultTokenBudgetBox.Value, DefaultMaximumTurnsBox.Value);
            var budget = (long)DefaultTokenBudgetBox.Value;
            var turns = (int)DefaultMaximumTurnsBox.Value;
            await settingsWriteLock.WaitAsync();
            try
            {
                var settings = globalSettings with { AdaptationTokenBudget = budget, AdaptationMaximumTurns = turns };
                await LocalFiles.WriteJsonAsync(Path.Combine(dataDirectory, "settings.json"), settings, DesktopJson.TypeInfo<Settings.DesktopSettings>(DesktopJson.Storage));
                globalSettings = settings;
            }
            finally { settingsWriteLock.Release(); }
            // Project drafts have already been saved when navigating to the global page.
            RefreshProjectAdaptationSettings();
            ShowSharedApiStatus("默认适配预算与轮数已保存。跟随全局默认的项目下次任务立即使用；运行中的任务保持原预算。");
        }
        catch (Exception error) { ShowSharedApiStatus(error.Message, true); }
        finally { pendingApiSelections--; UpdateApiControls(); }
    }
}
