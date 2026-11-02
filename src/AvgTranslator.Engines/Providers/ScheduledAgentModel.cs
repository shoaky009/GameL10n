using System.Diagnostics;
using AvgTranslator.Core.Development;

namespace AvgTranslator.Core.Providers;

/// <summary>Shares the same account slot as translation requests without exposing translation operations.</summary>
public sealed class ScheduledAgentModel(IAdaptationAgentModel provider, string accountKey) : IAdaptationAgentModel
{
    private readonly SemaphoreSlim slot = ScheduledProvider.AccountSlot(accountKey);
    public string Identity => provider.Identity;
    public string? DisplayName => provider.DisplayName;
    public string? AnalysisReasoningEffort => provider.AnalysisReasoningEffort;
    public string? AnalysisModelName => provider.AnalysisModelName;

    public async Task<AgentTurn> NextAsync(AgentRequest request, CancellationToken cancellationToken)
    {
        var waiting = Stopwatch.StartNew();
        await slot.WaitAsync(cancellationToken);
        waiting.Stop();
        try
        {
            var turn = await provider.NextAsync(request, cancellationToken);
            return turn with { QueueMilliseconds = waiting.Elapsed.TotalMilliseconds };
        }
        finally { slot.Release(); }
    }
}
