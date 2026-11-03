using System.Collections.Concurrent;

namespace GameL10n.Core.Pipeline;

public enum GameTaskKind { Analysis, Translation, Installation, Adaptation, Loading, Patching, Summary }

public sealed class GameTaskCoordinator(int maximumTranslations = 2)
{
    private readonly SemaphoreSlim translationSlots = new(Math.Clamp(maximumTranslations, 1, 8));
    private readonly SemaphoreSlim analysisSlot = new(1, 1);
    private readonly SemaphoreSlim adaptationSlot = new(1, 1);
    private readonly ConcurrentDictionary<string, byte> active = new(StringComparer.Ordinal);

    public async Task<T> RunAsync<T>(string gameId, GameTaskKind kind, Func<CancellationToken, Task<T>> work,
        CancellationToken ct = default, Action? onStarted = null)
    {
        if (!active.TryAdd(gameId, 0)) throw new InvalidOperationException("这个游戏已有任务正在运行。");
        SemaphoreSlim? slot = kind switch
        { GameTaskKind.Translation => translationSlots, GameTaskKind.Analysis => analysisSlot, GameTaskKind.Adaptation or GameTaskKind.Summary => adaptationSlot, _ => null };
        var acquired = false;
        try
        {
            if (slot is not null) { await slot.WaitAsync(ct); acquired = true; }
            ct.ThrowIfCancellationRequested();
            onStarted?.Invoke();
            return await work(ct);
        }
        finally
        {
            if (acquired) slot!.Release();
            active.TryRemove(gameId, out _);
        }
    }
}
