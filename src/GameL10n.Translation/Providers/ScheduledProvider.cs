using System.Collections.Concurrent;

namespace GameL10n.Core.Providers;

/// <summary>Shares an account request slot across translation and analysis tasks.</summary>
public sealed class ScheduledProvider(ITranslationProvider provider, string accountKey)
    : ITranslationProvider
{
    private static readonly ConcurrentDictionary<string, SemaphoreSlim> Slots = new(StringComparer.Ordinal);
    internal static SemaphoreSlim AccountSlot(string accountKey) => Slots.GetOrAdd(accountKey, _ => new(1, 1));
    private readonly SemaphoreSlim slot = AccountSlot(accountKey);
    public string Identity => provider.Identity;
    public string? DisplayName => provider.DisplayName;
    public int MaximumBatchSize => provider.MaximumBatchSize;

    public async Task<TranslationResponse> TranslateAsync(TranslationRequest request, CancellationToken cancellationToken = default)
    {
        await slot.WaitAsync(cancellationToken);
        try { return await provider.TranslateAsync(request, cancellationToken); }
        finally { slot.Release(); }
    }
}
