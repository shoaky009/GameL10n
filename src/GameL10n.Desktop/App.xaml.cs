using Microsoft.UI.Xaml;

namespace GameL10n.Desktop;

public partial class App : Application
{
    private Window? window;
    public App()
    {
        UnhandledException += (_, args) => Program.WriteStartupError(args.Exception);
        DebugSettings.IsXamlResourceReferenceTracingEnabled = true;
        DebugSettings.XamlResourceReferenceFailed += (_, args) => Program.WriteStartupError(new InvalidOperationException(args.Message));
        InitializeComponent();
    }
    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        window = new MainWindow();
        window.Activate();
    }
}
