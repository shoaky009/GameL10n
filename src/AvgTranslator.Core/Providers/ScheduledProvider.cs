using System.Collections.Concurrent;
using System.Diagnostics;
using AvgTranslator.Core.Development;

namespace AvgTranslator.Core.Providers;

/// <summary>Shares an API request slot across games and releases it after each model turn.</summary>
public sealed class ScheduledProvider(OpenAiCompatibleProvider provider, string accountKey)
    : ITranslationProvider, IAdaptationAgentModel
{
    private static readonly ConcurrentDictionary<string, SemaphoreSlim> Slots = new(StringComparer.Ordinal);
    private readonly SemaphoreSlim slot = Slots.GetOrAdd(accountKey, _ => new(1, 1));
    public string Identity => provider.Identity;
    public string? DisplayName => provider.DisplayName;
    public int MaximumBatchSize => provider.MaximumBatchSize;
    public string? AnalysisReasoningEffort => provider.AnalysisReasoningEffort;
    public string? AnalysisModelName => provider.AnalysisModelName;

    public async Task<TranslationResponse> TranslateAsync(TranslationRequest request, CancellationToken cancellationToken = default)
    {
        await slot.WaitAsync(cancellationToken);
        try { return await provider.TranslateAsync(request, cancellationToken); }
        finally { slot.Release(); }
    }

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
