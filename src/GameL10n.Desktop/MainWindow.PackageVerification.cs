using GameL10n.Core.Development;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace GameL10n.Desktop;

public sealed partial class MainWindow
{
    private async void CatalogVerifyVersion_Click(object sender, RoutedEventArgs e)
    {
        if (SavingEngineLibrary || changingDataDirectory || changingAdapterLibrary || closing
            || SelectedInspectedLibraryVersion is not { Source: EngineLibrarySource.External, Exists: true } version) return;
        var directory = dataDirectory;
        var choices = games.Where(item => !item.Busy && !item.IsBeingRemoved
            && (item.Project.Analysis?.AdapterId ?? item.Project.AdapterId) == version.AdapterId)
            .OrderBy(item => item.Name, StringComparer.CurrentCultureIgnoreCase).ToArray();
        if (choices.Length == 0)
        {
            ShowLibraryStatus("没有可用的同引擎项目。需要一个已完成提取、保留原文快照且当前未执行任务的项目作为验证样本。", InfoBarSeverity.Warning);
            return;
        }
        var picker = new ComboBox { ItemsSource = choices, DisplayMemberPath = "Name", SelectedIndex = 0,
            HorizontalAlignment = HorizontalAlignment.Stretch, IsTextSearchEnabled = true };
        var content = new StackPanel { Spacing = 12 };
        content.Children.Add(new TextBlock { Text = $"{version.AdapterId} · {version.Version}\n选择用于比较已有文本的游戏项目：", TextWrapping = TextWrapping.Wrap });
        content.Children.Add(picker);
        content.Children.Add(new TextBlock { Text = "使用项目已保存的原文快照和提取记录，从临时位置加载发布包，比较文本并检查原文、中文回填及局部试译。缺少这些记录时请先完成提取。\n不调用模型，不运行游戏，不安装补丁，不切换项目版本。需要临时复制资源，请预留磁盘空间；完成或取消后删除本次临时副本。", TextWrapping = TextWrapping.Wrap });
        var choose = new ContentDialog { XamlRoot = RootGrid.XamlRoot, Title = "验证发布包", Content = content,
            PrimaryButtonText = "开始验证", CloseButtonText = "取消", DefaultButton = ContentDialogButton.Primary };
        if (await choose.ShowAsync() != ContentDialogResult.Primary || picker.SelectedItem is not GameWorkspace game) return;
        if (directory != dataDirectory || changingDataDirectory || changingAdapterLibrary || closing || game.Busy || game.IsBeingRemoved) return;

        using var cancellation = new CancellationTokenSource();
        changingAdapterLibrary = true;
        engineLibrarySaving = true;
        UpdateControls();
        UpdateLibraryActions();
        var status = new TextBlock { Text = "正在读取发布包和已有提取记录…", TextWrapping = TextWrapping.Wrap, IsTextSelectionEnabled = true };
        var ring = new ProgressBar { IsIndeterminate = true };
        var details = new StackPanel { Spacing = 12 };
        details.Children.Add(ring);
        details.Children.Add(status);
        var dialog = new ContentDialog { XamlRoot = RootGrid.XamlRoot, Title = "验证发布包 · " + game.Name,
            Content = new ScrollViewer { Content = details, MaxHeight = 420 }, CloseButtonText = "取消验证" };
        var finished = false;
        var dialogOpen = true;
        var progress = new Progress<(string Stage, string Message)>(value =>
        {
            if (dialogOpen && !finished) status.Text = value.Stage + "\n" + value.Message;
        });
        Task? verificationTask = null;
        try
        {
            var store = LibraryStore();
            verificationTask = RunAsync();
            await dialog.ShowAsync();
            dialogOpen = false;
            if (!finished)
            {
                cancellation.Cancel();
                ShowLibraryStatus("正在取消验证并移除本次临时文件…", InfoBarSeverity.Informational);
            }
            await verificationTask;

            async Task RunAsync()
            {
                try
                {
                    var result = await Task.Run(async () =>
                    {
                        var package = store.LoadVersion(version.AdapterId, version.Version);
                        var verifier = new PublishedPackageVerifier(FindToolchain(), new AdapterProcessRunner(),
                            new EngineReferenceToolkit(Path.Combine(store.RootDirectory, "engine-tools"),
                                reportProgress: (stage, message) => ((IProgress<(string, string)>)progress).Report((stage, message))));
                        return await verifier.VerifyAsync(package, game.Directory,
                            Path.Combine(store.RootDirectory, "verification"), cancellation.Token,
                            (stage, message) => ((IProgress<(string, string)>)progress).Report((stage, message)));
                    });
                    var summary = result.Status == "cancelled" ? "验证已取消。"
                        : result.Passed ? "发布包资源验收通过，提取结果与已有记录一致。" : "发布包验证未通过。";
                    if (result.DevelopmentPathReferences.Count > 0)
                        summary += "\n发现旧目录路径引用，需要核对是否仍依赖开发文件：" + string.Join("、", result.DevelopmentPathReferences);
                    status.Text = summary + (result.Passed ? "" : "\n" + string.Join("\n", result.Diagnostics))
                        + "\n\n本次没有运行游戏，启动与显示效果尚未验证。更换目录验收不等同于文件访问隔离。"
                        + "\n结果已保存到项目的 package-verification.json，仅适用于本次包和资源。发布包、资源或工具链变化后需要重新验证。";
                    ShowLibraryStatus(game.Name + "：" + summary, result.Passed && result.DevelopmentPathReferences.Count == 0
                        ? InfoBarSeverity.Success : InfoBarSeverity.Warning);
                }
                catch (Exception error)
                {
                    status.Text = "验证未完成：" + error.Message;
                    ShowLibraryStatus(status.Text, InfoBarSeverity.Error);
                }
                finally
                {
                    finished = true;
                    ring.IsIndeterminate = false;
                    ring.Visibility = Visibility.Collapsed;
                    dialog.CloseButtonText = "关闭";
                }
            }
        }
        catch (Exception error) { cancellation.Cancel(); ShowLibraryStatus("验证未完成：" + error.Message, InfoBarSeverity.Error); }
        finally
        {
            dialogOpen = false;
            if (!finished) cancellation.Cancel();
            if (verificationTask is not null) await verificationTask;
            changingAdapterLibrary = false;
            engineLibrarySaving = false;
            UpdateControls();
            UpdateLibraryActions();
        }
    }
}
