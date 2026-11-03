using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace GameL10n.Desktop;

public sealed partial class MainWindow
{
    private string? globalPage;

    private void ShowPage(string? page)
    {
        globalPage = page;
        ProjectView.Visibility = page is null ? Visibility.Visible : Visibility.Collapsed;
        ApiProfilesView.Visibility = page == "apis" ? Visibility.Visible : Visibility.Collapsed;
        CatalogView.Visibility = page == "catalog" ? Visibility.Visible : Visibility.Collapsed;
        ApplicationSettingsView.Visibility = page == "application" ? Visibility.Visible : Visibility.Collapsed;
        if (page is null) GlobalNavigation.SelectedItem = null;
        else if (GameList.SelectionMode == ListViewSelectionMode.Single) GameList.SelectedItem = null;
        else GameList.SelectedItems.Clear();
    }

    private async void GlobalNavigation_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (!initialized || switchingGames || changingDataDirectory || removingGames || GlobalNavigation.SelectedItem is not ListViewItem { Tag: string page }) return;
        switchingGames = true;
        SetWindowActivity("正在保存当前项目设置…");
        UpdateControls();
        try
        {
            if (globalPage is null && selectedGame is not null) await SaveSettingsAsync(persistNavigation: false);
            ShowPage(page);
            if (page == "catalog") _ = EnsureEngineLibraryAsync();
        }
        catch (Exception error)
        {
            GlobalNavigation.SelectedItem = null;
            GameList.SelectedItem = selectedGame;
            ShowError(error);
        }
        finally { switchingGames = false; SetWindowActivity(null); UpdateControls(); }
    }

    private void ManageApis_Click(object sender, RoutedEventArgs e) => GlobalNavigation.SelectedItem = ApiNavigationItem;
}
