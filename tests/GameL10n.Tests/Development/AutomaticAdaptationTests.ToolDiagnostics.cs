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

    [Fact]
    public async Task InlineProbeFailureKeepsRawOutputForTargetedReadBack()
    {
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        using var fixture = new EngineArchiveFixture();
        await fixture.PrepareSharedToolsAsync(store.RootDirectory);
        const string source = "print('fixture probe')";
        const string middle = "PRIVATE_MIDDLE_FINDING";
        const string traceback = "Traceback (most recent call last):\n  File probe.py, line 42\nValueError: decisive failure";
        var stdout = "stdout head\n" + new string('x', 420_000) + middle + new string('y', 420_000) + "\nstdout tail";
        var stderr = "stderr head\n" + new string('z', 40_000) + "\n" + traceback;
        var arguments = JsonSerializer.Serialize(new { script = "work/probe.py", content = source }, Json);
        var runs = 0;
        var runner = new ScriptedRunner { ObserveRequest = request =>
        {
            if (Path.GetFileName(request.ExecutablePath) != "python.exe") return;
            runs++;
            Assert.Equal(source, File.ReadAllText(Path.Combine(request.WorkspacePath, "work/probe.py")));
            throw new AdapterProcessLimitException("fixture timeout", timedOut: true, standardOutput: stdout, standardError: stderr);
        } };
        var turn = 0;
        var model = new FakeModel(context =>
        {
            switch (turn++)
            {
                case 0:
                    return new(new("assistant", ToolCalls: [new("probe", "run_engine_python", arguments)]), new(100, 20));
                case 1:
                    Assert.DoesNotContain(context.Messages, message => message.Role == "user"
                        && message.Content.Contains("当前会话较长", StringComparison.Ordinal));
                    var message = Assert.Single(context.Messages, message => message.ToolCallId == "probe");
                    using (var preview = JsonDocument.Parse(message.Content))
                    {
                        Assert.False(preview.RootElement.GetProperty("success").GetBoolean());
                        Assert.EndsWith(traceback, preview.RootElement.GetProperty("stderr").GetString());
                        Assert.Equal("probe", preview.RootElement.GetProperty("processOutputPreview").GetProperty("toolCallId").GetString());
                    }
                    Assert.True(message.Content.Length < 10_000);
                    Assert.DoesNotContain(middle, message.Content);
                    return new(new("assistant", ToolCalls: [new("recover", "read_tool_result",
                        JsonSerializer.Serialize(new { toolCallId = "probe", query = middle, length = 512 }, Json))]), new(100, 20));
                default:
                    using (var recovered = JsonDocument.Parse(Assert.Single(context.Messages, message => message.ToolCallId == "recover").Content))
                    {
                        var result = recovered.RootElement.GetProperty("result");
                        Assert.Contains(middle, result.GetProperty("content").GetString());
                        Assert.True(result.GetProperty("totalCharacters").GetInt32() > 850_000);
                    }
                    return Blocked("needs_input", "等待样本。", "提供样本后继续。");
            }
        });

        var result = await new AutomaticAdaptationService(model, Runtime(runner), store).AdaptAsync(Request());

        Assert.True(result.Status == AdaptationStatus.WaitingForUser, result.Summary + "\n" + string.Join("\n", result.Diagnostics));
        Assert.Equal(3, model.Calls);
        Assert.Equal(1, runs);
        var saved = await GameL10n.Core.Storage.LocalFiles.ReadJsonAsync<AdaptationSession>(
            Path.Combine(store.WorkDirectory, "current/session.json"), FixtureJson.TypeInfo<global::GameL10n.Core.Development.AdaptationSession>(FixtureJson.Storage));
        using var original = JsonDocument.Parse(Assert.Single(saved!.Messages, message => message.ToolCallId == "probe").Content);
        Assert.Equal(stdout, original.RootElement.GetProperty("stdout").GetString());
        Assert.Equal(stderr, original.RootElement.GetProperty("stderr").GetString());
        Assert.False(original.RootElement.TryGetProperty("processOutputPreview", out _));
        Assert.Equal(arguments, saved.Messages.SelectMany(message => message.ToolCalls ?? []).Single(call => call.Id == "probe").Arguments);
    }

    [Fact]
    public async Task SuccessfulInlineProbeStoresReusableInvocationWithoutItsSource()
    {
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        using var fixture = new EngineArchiveFixture();
        await fixture.PrepareSharedToolsAsync(store.RootDirectory);
        const string source = "print('confirmed layout')";
        var arguments = JsonSerializer.Serialize(new { script = "work/probe.py", content = source, arguments = new[] { "--known-boundary" } }, Json);
        var runs = 0;
        var runner = new ScriptedRunner { LocalProbe = () => runs++ };
        var model = new FakeModel(context => context.Messages.Any(message => message.ToolCallId == "probe")
            ? Blocked("needs_input", "需要显示反馈。", "提供画面后继续。")
            : new(new("assistant", ToolCalls: [new("probe", "run_engine_python", arguments)]), new(100, 20)));

        var result = await new AutomaticAdaptationService(model, Runtime(runner), store).AdaptAsync(Request());

        Assert.Equal(AdaptationStatus.WaitingForUser, result.Status);
        Assert.Equal(1, runs);
        Assert.Equal(source, await File.ReadAllTextAsync(Path.Combine(store.WorkDirectory, "current/work/probe.py")));
        var saved = await GameL10n.Core.Storage.LocalFiles.ReadJsonAsync<AdaptationSession>(
            Path.Combine(store.WorkDirectory, "current/session.json"), FixtureJson.TypeInfo<global::GameL10n.Core.Development.AdaptationSession>(FixtureJson.Storage));
        var probe = Assert.Single(saved!.SuccessfulProbes);
        Assert.Equal("run_engine_python", probe.Tool);
        using var invocation = JsonDocument.Parse(probe.Arguments);
        Assert.Equal("work/probe.py", invocation.RootElement.GetProperty("script").GetString());
        Assert.Equal("--known-boundary", Assert.Single(invocation.RootElement.GetProperty("arguments").EnumerateArray()).GetString());
        Assert.False(invocation.RootElement.TryGetProperty("content", out _));
        Assert.Equal(arguments, saved.Messages.SelectMany(message => message.ToolCalls ?? []).Single(call => call.Id == "probe").Arguments);
    }
}
