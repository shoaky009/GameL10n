using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;

namespace GameL10n.Desktop;

internal static class Program
{
    [STAThread]
    private static int Main()
    {
        try
        {
            WinRT.ComWrappersSupport.InitializeComWrappers();
            Application.Start(parameters =>
            {
                SynchronizationContext.SetSynchronizationContext(new DispatcherQueueSynchronizationContext(DispatcherQueue.GetForCurrentThread()));
                _ = new App();
            });
            return 0;
        }
        catch (Exception error) { WriteStartupError(error); return 1; }
    }

    internal static void WriteStartupError(Exception error)
    {
        string directory;
        try { directory = Path.Combine(Core.Storage.DataLocation.Resolve(), "logs"); }
        catch { directory = Path.Combine(Core.Storage.DataLocation.DefaultDirectory, "logs"); }
        try
        {
            Directory.CreateDirectory(directory);
            var details = string.Join("\n", error.Data.Keys.Cast<object>().Select(key => key + ": " + error.Data[key]));
            File.AppendAllText(Path.Combine(directory, "startup-error.log"), DateTimeOffset.UtcNow + "\n" + error + "\n" + details + "\n");
        }
        catch (Exception loggingError) when (loggingError is IOException or UnauthorizedAccessException) { }
    }
}
