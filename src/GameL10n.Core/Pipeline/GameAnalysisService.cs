using GameL10n.Core.Development;

namespace GameL10n.Core.Pipeline;

public sealed record GameAnalysisResult(GameSession? Session, AdapterDiscoveryResult Discovery);

/// <summary>Creates project snapshots and commits only a candidate accepted by engine discovery.</summary>
public sealed class GameAnalysisService(TranslationPipeline pipeline, AdapterDiscoveryService discovery)
{
    public async Task<GameAnalysisResult> AnalyzeAsync(AdapterDiscoveryRequest request, CancellationToken ct = default,
        Action<string, string>? progress = null, Action<string>? log = null, Func<object, Task>? trace = null)
    {
        GameSession? prepared = null;
        var result = await discovery.FindAsync(request with { Mode = AdapterDiscoveryMode.Analysis }, ct,
            async (adapter, probe, token) =>
            {
                prepared = await pipeline.AnalyzeAsync(request.GameDirectory, adapter, token, request.GameId, probe,
                    progress, commitSnapshot: false);
                return prepared.SourceDirectory;
            }, progress, log, trace);
        if (result.Adapter is null) return new(null, result);
        await pipeline.CommitAnalysisAsync(prepared!, ct);
        return new(prepared, result);
    }
}
