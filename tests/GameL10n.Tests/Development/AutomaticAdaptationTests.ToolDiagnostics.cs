using System.Text.Json;
using GameL10n.Core.Development;

namespace GameL10n.Tests.Development;

public sealed partial class AutomaticAdaptationTests
{
    [Fact]
    public async Task LocalToolTimeoutReachesModelWithoutCopyingOutputIntoTimingLog()
    {
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        using var fixture = new EngineArchiveFixture();
        await fixture.PrepareSharedToolsAsync(store.RootDirectory);
        var turn = 0;
        var model = new FakeModel(context => turn++ switch
        {
            0 => new(new("assistant", ToolCalls: [new("write", "write_file",
                """{"path":"work/probe.py","content":"print(1)"}""")]), new(100, 20)),
            1 => new(new("assistant", ToolCalls: [new("probe", "run_engine_python",
                """{"script":"work/probe.py"}""")]), new(100, 20)),
            _ => InspectTimeout(context)
        });
        var runner = new ScriptedRunner { ObserveRequest = request =>
        {
            if (Path.GetFileName(request.ExecutablePath) == "python.exe")
                throw new AdapterProcessLimitException("fixture timeout", timedOut: true,
                    standardOutput: "private-timeout-output", standardError: "private-timeout-error");
        } };
        var result = await new AutomaticAdaptationService(model, Runtime(runner), store).AdaptAsync(Request());
        Assert.Equal(AdaptationStatus.WaitingForUser, result.Status);
        var log = Assert.Single(Directory.GetFiles(Path.Combine(store.RootDirectory, "logs"), "adaptation-*.jsonl"));
        var logText = await File.ReadAllTextAsync(log);
        Assert.DoesNotContain("private-timeout-output", logText);
        Assert.DoesNotContain("private-timeout-error", logText);
        var failure = File.ReadLines(log).Select(line => JsonSerializer.Deserialize<JsonElement>(line).GetProperty("entry"))
            .Single(entry => entry.TryGetProperty("call", out var call) && call.GetProperty("id").GetString() == "probe");
        Assert.Equal("failed", failure.GetProperty("status").GetString());
        Assert.Equal("run", failure.GetProperty("result").GetProperty("phase").GetString());
        Assert.True(failure.GetProperty("result").GetProperty("timedOut").GetBoolean());

        static AgentTurn InspectTimeout(AgentRequest context)
        {
            using var response = JsonDocument.Parse(context.Messages.Single(message => message.ToolCallId == "probe").Content);
            Assert.False(response.RootElement.GetProperty("success").GetBoolean());
            Assert.Equal("run", response.RootElement.GetProperty("phase").GetString());
            Assert.True(response.RootElement.GetProperty("timedOut").GetBoolean());
            Assert.Equal("private-timeout-output", response.RootElement.GetProperty("stdout").GetString());
            Assert.Equal("private-timeout-error", response.RootElement.GetProperty("stderr").GetString());
            return Blocked("needs_input", "等待样本。", "提供样本后继续。");
        }
    }
}
