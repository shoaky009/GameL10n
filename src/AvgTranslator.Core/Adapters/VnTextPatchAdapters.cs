using AvgTranslator.Core.Development;

namespace AvgTranslator.Core.Adapters;

/// <summary>Built-in loose-script adapters backed by the bundled VNTextPatch worker.</summary>
public static class VnTextPatchAdapters
{
    public static IReadOnlyList<IGameAdapter> CreateAll(IAdapterProcessRunner? runner = null,
        string? workerExecutablePath = null, string? workerAssemblyPath = null, GameDiscoverySession? discoverySession = null)
    {
        var client = new VnTextPatchClient(runner ?? new AdapterProcessRunner(), workerExecutablePath, workerAssemblyPath);
        var discovery = new VnTextPatchDiscovery(discoverySession);
        return VnTextPatchFormatCatalog.All.Select(format =>
            (IGameAdapter)new VnTextPatchAdapter(format, client, discovery)).ToArray();
    }

    /// <summary>Starts a fresh discovery pass after the user selects or updates a game directory.</summary>
    public static void ResetDiscovery(IEnumerable<IGameAdapter> adapters)
    {
        foreach (var adapter in adapters.OfType<VnTextPatchAdapter>()) adapter.ResetDiscovery();
    }
}
