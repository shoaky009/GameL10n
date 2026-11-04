using System.Text.Json;
using GameL10n.Core.Development;
using GameL10n.Core.Storage;

namespace GameL10n.Tests.Development;

public sealed partial class AutomaticAdaptationTests
{
    [Fact]
    public async Task ContinuousSourceEditsStillReceiveVerificationReview()
    {
        var request = Request();
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var turn = 0;
        var model = new FakeModel(context =>
        {
            if (turn++ == 24)
            {
                Assert.Single(context.Messages, message => message.Content.Contains("验证进展检查：", StringComparison.Ordinal));
                return Blocked("needs_input", "需要显示反馈。", "提供画面。");
            }
            Assert.DoesNotContain(context.Messages, message => message.Content.Contains("验证进展检查：", StringComparison.Ordinal));
            return new(new("assistant", ToolCalls: [new("edit" + turn, "write_file", JsonSerializer.Serialize(new
            {
                path = "src/Parser.cs", content = "// implementation " + turn
            }))]), new(100, 20));
        });
        var result = await new AutomaticAdaptationService(model, Compiler(new ScriptedRunner()), store).AdaptAsync(request);
        Assert.Equal(AdaptationStatus.WaitingForUser, result.Status);
        var saved = await LocalFiles.ReadJsonAsync<AdaptationSession>(Path.Combine(store.WorkDirectory, "current/session.json"));
        Assert.Equal(24, saved!.LastImplementationTurn);
        Assert.Equal(0, saved.LastVerificationProgressTurn);
        Assert.Equal(24, saved.LastVerificationReviewTurn);
    }

    [Fact]
    public async Task VerificationProgressRequiresNewPassedStagesAndIgnoresCachedResults()
    {
        var (request, state, workspace, store) = await VerificationCacheFixtureAsync(new ScriptedRunner());
        var tools = new AdaptationTools(request, state, workspace,
            Compiler(new ScriptedRunner { CompilationError = "fixture compilation failure" }), store);
        state.TotalTurns = 3;
        await Assert.ThrowsAsync<GeneratedAdapterException>(() => tools.ExecuteAsync(new("bad", "test_adapter", CacheTestArguments), default));
        Assert.Equal(AdapterVerificationMilestone.None, state.HighestVerificationMilestone);
        Assert.Null(state.LastVerificationProgressTurn);

        async Task ChangeSource(string content) => await tools.ExecuteAsync(new("edit", "write_file",
            JsonSerializer.Serialize(new { path = "src/Program.cs", content })), default);
        await ChangeSource("// implementation 1");
        tools = new(request, state, workspace, Compiler(new ScriptedRunner { DropTranslatedUnit = true }), store);
        state.TotalTurns = 5;
        using var partial = JsonDocument.Parse(await tools.ExecuteAsync(new("partial", "test_adapter", CacheTestArguments), default));
        Assert.False(partial.RootElement.GetProperty("result").GetProperty("passed").GetBoolean());
        Assert.Equal(AdapterVerificationMilestone.OriginalRoundtrip, state.HighestVerificationMilestone);
        Assert.Equal(5, state.LastVerificationProgressTurn);
        state.TotalTurns = 9;
        using var cachedFailure = JsonDocument.Parse(await tools.ExecuteAsync(new("cached", "test_adapter", CacheTestArguments), default));
        Assert.True(cachedFailure.RootElement.GetProperty("result").GetProperty("reused").GetBoolean());
        await ChangeSource("// implementation 2");
        await tools.ExecuteAsync(new("same-stage", "test_adapter", CacheTestArguments), default);
        Assert.Equal(5, state.LastVerificationProgressTurn);

        tools = new(request, state, workspace, Compiler(new ScriptedRunner()), store);
        state.TotalTurns = 12;
        await ChangeSource("// implementation 3");
        await tools.ExecuteAsync(new("complete", "test_adapter", CacheTestArguments), default);
        Assert.Equal(AdapterVerificationMilestone.Complete, state.HighestVerificationMilestone);
        Assert.Equal(12, state.LastVerificationProgressTurn);
        state.TotalTurns = 20;
        using var cachedSuccess = JsonDocument.Parse(await tools.ExecuteAsync(new("cached-success", "test_adapter", CacheTestArguments), default));
        Assert.True(cachedSuccess.RootElement.GetProperty("result").GetProperty("reused").GetBoolean());
        Assert.Equal(12, state.LastVerificationProgressTurn);
    }

    [Fact]
    public async Task LegacyVerificationReviewIsNotRepeatedOnEveryResume()
    {
        var request = Request();
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var path = Path.Combine(store.WorkDirectory, "current/session.json");
        await LocalFiles.WriteJsonAsync(path, new AdaptationSession
        {
            GameId = request.GameId, GameDirectory = request.GameDirectory, ModelIdentity = "fake",
            TotalTurns = 80, LastImplementationTurn = 79, Messages = [new("user", "Continue existing implementation.")]
        });
        var model = new FakeModel(context =>
        {
            Assert.Single(context.Messages, message => message.Content.Contains("验证进展检查：", StringComparison.Ordinal));
            return Blocked("needs_input", "需要显示反馈。", "提供画面。");
        });
        var service = new AutomaticAdaptationService(model, Compiler(new ScriptedRunner()), store);
        await service.AdaptAsync(request);
        await service.AdaptAsync(request);
        var saved = await LocalFiles.ReadJsonAsync<AdaptationSession>(path);
        Assert.Equal(80, saved!.LastVerificationReviewTurn);
        Assert.Equal(80, saved.LastVerificationProgressTurn);
        Assert.Equal(2, model.Calls);
    }
}
