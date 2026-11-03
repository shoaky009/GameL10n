using GameL10n.Core;
using GameL10n.Core.Development;
using GameL10n.Core.Providers;

namespace GameL10n.Tests.Providers;

public sealed class ProviderSchedulingTests
{
    [Fact]
    public async Task SeparateAnalysisAndTranslationClientsKeepTheSameAccountConcurrencyLimit()
    {
        var started = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var release = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var account = Guid.NewGuid().ToString("N");
        var translation = new ScheduledProvider(new Translation(started, release), account);
        var agentModel = new Agent();
        var analysis = new ScheduledAgentModel(agentModel, account);
        var translated = translation.TranslateAsync(new([], "简体中文", new Dictionary<string, string>(), ""));
        await started.Task.WaitAsync(TimeSpan.FromSeconds(5));
        using var cancellation = new CancellationTokenSource(TimeSpan.FromSeconds(5));
        var analyzed = analysis.NextAsync(new("Analyze", [], [], 100), cancellation.Token);
        Assert.False(agentModel.Called);
        release.SetResult();
        await translated.WaitAsync(TimeSpan.FromSeconds(5));
        await analyzed;
        Assert.True(agentModel.Called);
    }

    private sealed class Translation(TaskCompletionSource started, TaskCompletionSource release) : ITranslationProvider
    {
        public string Identity => "translation-test";
        public async Task<TranslationResponse> TranslateAsync(TranslationRequest request, CancellationToken cancellationToken = default)
        {
            started.SetResult();
            await release.Task.WaitAsync(cancellationToken);
            return new(new Dictionary<string, string>(), new(0, 0));
        }
    }

    private sealed class Agent : IAdaptationAgentModel
    {
        public string Identity => "analysis-test";
        public bool Called { get; private set; }
        public Task<AgentTurn> NextAsync(AgentRequest request, CancellationToken cancellationToken)
        {
            Called = true;
            return Task.FromResult(new AgentTurn(new("assistant", "done"), new(0, 0)));
        }
    }
}
