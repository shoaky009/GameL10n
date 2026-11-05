using GameL10n.Core.Development;
using System.Text.Json.Nodes;
using GameL10n.Core.Serialization;

namespace GameL10n.Core.Pipeline;

public sealed record GameAnalysisResult(GameSession? Session, AdapterDiscoveryResult Discovery);

/// <summary>Creates project snapshots and commits only a candidate accepted by engine discovery.</summary>
public sealed class GameAnalysisService(TranslationPipeline pipeline, AdapterDiscoveryService discovery)
{
    public Task<GameAnalysisResult> AnalyzeAsync(AdapterDiscoveryRequest request, CancellationToken ct = default,
        Action<string, string>? progress = null, Action<string>? log = null, Func<object, Task>? trace = null) =>
        new AdapterOperationTrace(trace, "analysis-run").MeasureAsync<GameAnalysisResult>("analysis", async () =>
        {
            GameSession? prepared = null;
            var result = await discovery.FindAsync(request with { Mode = AdapterDiscoveryMode.Analysis }, ct,
                async (adapter, probe, token) =>
                {
                    prepared = await pipeline.AnalyzeAsync(request.GameDirectory, adapter, token, request.GameId, probe,
                        progress, commitSnapshot: false, trace: trace);
                    return prepared.SourceDirectory;
                }, progress, log, trace);
            if (result.Adapter is null) return new(null, result);
            await new AdapterOperationTrace(trace, "analysis-phase", result.Adapter.Id, result.Adapter.Version)
                .MeasureAsync("commit", () => pipeline.CommitAnalysisAsync(prepared!, ct));
            return new(prepared, result);
        }, new JsonObject
        {
            ["gameId"] = request.GameId,
            ["coreBuild"] = typeof(GameAnalysisService).Module.ModuleVersionId.ToString("N")
        });
}
