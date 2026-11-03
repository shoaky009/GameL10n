using GameL10n.Core.Development;
using GameL10n.Core.Pipeline;

namespace GameL10n.Desktop;

public sealed partial class MainWindow
{
    // Set on the UI thread during deletion; prevents a new task from taking a package during removal.
    private bool changingAdapterLibrary;

    private IReadOnlyList<EngineLibraryReference> GetRunningEngineReferences() => games
        .Where(game => game.Busy && (game.Kind is GameTaskKind.Translation or GameTaskKind.Patching) && game.Session is not null)
        .Select(game => new EngineLibraryReference(game.Session!.Adapter.Id, game.Session.Adapter.Version,
            game.Id, game.Name, EngineLibraryReferenceKind.Running, game.Directory))
        .ToArray();

    private void DetachDeletedEngineVersion(string adapterId, string version)
    {
        foreach (var game in games)
        {
            if (game.Session is not { Adapter: GeneratedGameAdapter } previous
                || previous.Adapter.Id != adapterId || previous.Adapter.Version != version) continue;
            game.Session = previous with { Adapter = new UnavailableGameAdapter(adapterId, version) };
            if (ReferenceEquals(game.RowsSession, previous)) game.RowsSession = game.Session;
        }
    }
}
