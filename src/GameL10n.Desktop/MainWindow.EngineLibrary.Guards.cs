using GameL10n.Core.Development;

namespace GameL10n.Desktop;

public sealed partial class MainWindow
{
    // Set on the UI thread during deletion; prevents a new task from taking a package after the reference check.
    private bool changingAdapterLibrary;

    private IReadOnlyList<EngineLibraryReference> GetRunningEngineReferences() => games
        .Where(game => game.Session is not null)
        .Select(game => new EngineLibraryReference(game.Session!.Adapter.Id, game.Session.Adapter.Version,
            game.Id, game.Name, EngineLibraryReferenceKind.Running, game.Directory))
        .ToArray();
}
