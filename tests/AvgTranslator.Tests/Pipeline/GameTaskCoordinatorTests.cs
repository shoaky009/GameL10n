using AvgTranslator.Core.Pipeline;

namespace AvgTranslator.Tests.Pipeline;

public sealed class GameTaskCoordinatorTests
{
    [Fact]
    public async Task OpeningSavedProjectDoesNotWaitForAnotherGamesAnalysis()
    {
        var coordinator = new GameTaskCoordinator();
        var finish = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var analysis = coordinator.RunAsync("first", GameTaskKind.Analysis, async token =>
        { await finish.Task.WaitAsync(token); return 1; });
        try
        {
            Assert.Equal(2, await coordinator.RunAsync("second", GameTaskKind.Loading, _ => Task.FromResult(2))
                .WaitAsync(TimeSpan.FromSeconds(3)));
            await Assert.ThrowsAsync<InvalidOperationException>(() => coordinator.RunAsync("first", GameTaskKind.Loading, _ => Task.FromResult(0)));
        }
        finally { finish.TrySetResult(); }
        await analysis;
    }

    [Theory]
    [InlineData(GameTaskKind.Translation)]
    [InlineData(GameTaskKind.Adaptation)]
    [InlineData(GameTaskKind.Summary)]
    public async Task BackgroundTaskDoesNotBlockAnotherGameAnalysisAndFailureReleasesGame(GameTaskKind kind)
    {
        var coordinator = new GameTaskCoordinator(1);
        var started = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var finish = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var translating = coordinator.RunAsync("first", kind, async token =>
        { started.SetResult(); await finish.Task.WaitAsync(token); return 1; });
        await started.Task.WaitAsync(TimeSpan.FromSeconds(3));
        try
        {
            Assert.Equal(2, await coordinator.RunAsync("second", GameTaskKind.Analysis, _ => Task.FromResult(2)).WaitAsync(TimeSpan.FromSeconds(3)));
            if (kind is GameTaskKind.Adaptation or GameTaskKind.Summary)
                Assert.Equal(4, await coordinator.RunAsync("third", GameTaskKind.Translation, _ => Task.FromResult(4)).WaitAsync(TimeSpan.FromSeconds(3)));
            await Assert.ThrowsAsync<InvalidOperationException>(() => coordinator.RunAsync("first", GameTaskKind.Analysis, _ => Task.FromResult(0)));
            await Assert.ThrowsAsync<IOException>(() => coordinator.RunAsync<int>("second", GameTaskKind.Analysis, _ => throw new IOException("failed")));
            Assert.Equal(3, await coordinator.RunAsync("second", GameTaskKind.Analysis, _ => Task.FromResult(3)));
        }
        finally { finish.TrySetResult(); }
        Assert.Equal(1, await translating);
    }

    [Fact]
    public async Task CancelQueuedGameDoesNotCancelRunningGameOrLeakSlot()
    {
        var coordinator = new GameTaskCoordinator(1);
        var started = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var finish = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var first = coordinator.RunAsync("first", GameTaskKind.Translation, async token =>
        { started.SetResult(); await finish.Task.WaitAsync(token); return 1; });
        await started.Task.WaitAsync(TimeSpan.FromSeconds(3));
        using var cancellation = new CancellationTokenSource();
        var invoked = false;
        var queued = coordinator.RunAsync("second", GameTaskKind.Translation, _ => { invoked = true; return Task.FromResult(2); }, cancellation.Token);
        cancellation.Cancel();
        try { await Assert.ThrowsAnyAsync<OperationCanceledException>(() => queued); Assert.False(invoked); Assert.False(first.IsCompleted); }
        finally { finish.TrySetResult(); }
        await first;
        Assert.Equal(3, await coordinator.RunAsync("second", GameTaskKind.Translation, _ => Task.FromResult(3)).WaitAsync(TimeSpan.FromSeconds(3)));
    }
}
