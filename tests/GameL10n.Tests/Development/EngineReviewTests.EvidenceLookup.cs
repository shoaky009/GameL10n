using System.Text.Json;
using GameL10n.Core.Adapters;
using GameL10n.Core.Development;

namespace GameL10n.Tests.Development;

public sealed partial class EngineReviewTests
{
    [Fact]
    public async Task HistoricalToolContentCanBePagedAndCitedButCachedContentCannotOutliveItsSnapshot()
    {
        var project = await ProjectAsync();
        var session = Path.Combine(project, EngineReviewEvidence.SessionEvidencePath);
        var prefix = new string('x', 13000);
        var content = prefix + "DECISIVE:yes; more evidence";
        await File.WriteAllTextAsync(session, JsonSerializer.Serialize(new
        {
            messages = new object[]
            {
                new { role = "assistant", content = "PRIVATE_REASONING", providerData = new { secret = "PRIVATE_PROVIDER" } },
                new { role = "tool", toolCallId = "old-sample", content, providerData = new { secret = "PRIVATE_TOOL_PROVIDER" } }
            }
        }, Json));
        var turn = 0;
        var model = new Model(request =>
        {
            if (turn++ == 0)
                return Call("read_tool_result", new { toolCallId = "old-sample", query = "DECISIVE", length = 12 });
            using var response = JsonDocument.Parse(request.Messages.Last(message => message.Role == "tool").Content);
            Assert.True(response.RootElement.GetProperty("success").GetBoolean());
            var page = response.RootElement.GetProperty("result");
            Assert.Equal("adaptation", page.GetProperty("source").GetString());
            Assert.Equal(EngineReviewEvidence.SessionEvidencePath, page.GetProperty("evidencePath").GetString());
            Assert.Equal("DECISIVE:yes", page.GetProperty("content").GetString());
            Assert.Equal(prefix.Length + 12, page.GetProperty("nextOffset").GetInt32());
            Assert.True(page.GetProperty("more").GetBoolean());
            Assert.DoesNotContain("PRIVATE_", response.RootElement.ToString());
            return FinishWithEvidence([EngineReviewEvidence.SessionEvidencePath]);
        });

        var published = await new EngineReviewService(model, Store).ReviewAsync(project, "one", "First game");
        Assert.Equal("created", published.ChangeKind);
        Assert.Equal(EngineReviewEvidence.SessionEvidencePath, Assert.Single(Assert.Single(Store.ReadAll()).Origins).Evidence.Single());

        var snapshot = await EngineReviewEvidence.LoadAsync(project, default);
        Assert.False(snapshot.IsEvidence(EngineReviewEvidence.SessionEvidencePath));
        await Assert.ThrowsAsync<InvalidDataException>(() => snapshot.ReadAsync(EngineReviewEvidence.SessionEvidencePath, 0, 100, default));
        Assert.Equal(content, await snapshot.ReadAdaptationResultAsync("old-sample", default));
        Assert.True(snapshot.IsEvidence(EngineReviewEvidence.SessionEvidencePath));
        await File.WriteAllTextAsync(session, "{\"messages\":[]}");
        await Assert.ThrowsAsync<IOException>(() => snapshot.ReadAdaptationResultAsync("old-sample", default));
        await Assert.ThrowsAsync<IOException>(() => snapshot.EnsureUnchangedAsync(default));
    }

    [Fact]
    public async Task CurrentReviewResultsTakePriorityAndUnknownIdsExplainBothDomainsAndAvailableEvidence()
    {
        var project = await ProjectAsync();
        await File.WriteAllTextAsync(Path.Combine(project, EngineReviewEvidence.SessionEvidencePath),
            """{"messages":[{"role":"tool","toolCallId":"collision","content":"OLD_RESULT: historical"}]}""");
        var logs = Directory.CreateDirectory(Path.Combine(project, "logs")).FullName;
        await File.WriteAllTextAsync(Path.Combine(logs, "adaptation-fixture.jsonl"),
            """{"runId":"one","entry":{"operation":"tool","tool":"test_adapter","call":{"id":"collision"},"result":{"passed":true},"status":"completed"}}""");
        var turn = 0;
        var model = new Model(request =>
        {
            switch (turn++)
            {
                case 0:
                    Assert.Contains("collision", request.Messages[0].Content);
                    return new(new("assistant", ToolCalls: [new("collision", "read_evidence",
                        """{"path":"adaptation/current/work/probe.py"}""")]), new(20, 10));
                case 1:
                    return Call("read_tool_result", new { toolCallId = "collision", query = "import sys", length = 30 });
                case 2:
                    using (var response = JsonDocument.Parse(request.Messages.Last(message => message.Role == "tool").Content))
                    {
                        var page = response.RootElement.GetProperty("result");
                        Assert.Equal("review", page.GetProperty("source").GetString());
                        Assert.Equal("adaptation/current/work/probe.py", page.GetProperty("evidencePath").GetString());
                        Assert.Contains("import sys", page.GetProperty("content").GetString());
                        Assert.DoesNotContain("OLD_RESULT", page.GetProperty("content").GetString());
                    }
                    return Call("read_tool_result", new { toolCallId = "collision", source = "adaptation", query = "OLD_RESULT", length = 10 });
                case 3:
                    using (var response = JsonDocument.Parse(request.Messages.Last(message => message.Role == "tool").Content))
                    {
                        var page = response.RootElement.GetProperty("result");
                        Assert.Equal("adaptation", page.GetProperty("source").GetString());
                        Assert.Equal("OLD_RESULT", page.GetProperty("content").GetString());
                    }
                    return Call("read_tool_result", new { toolCallId = "unknown-call", source = "auto" });
                default:
                    using (var response = JsonDocument.Parse(request.Messages.Last(message => message.Role == "tool").Content))
                    {
                        Assert.False(response.RootElement.GetProperty("success").GetBoolean());
                        var error = response.RootElement.GetProperty("error").GetString();
                        Assert.Contains("unknown-call", error);
                        Assert.Contains("source='adaptation'", error);
                        Assert.Contains("entry.call.id", error);
                        Assert.Contains("read_evidence", error);
                        Assert.Contains("adaptation/current/work/probe.py", error);
                    }
                    return Call("finish_review", new { summary = "现有证据无需新增经验。", improvement = (object?)null });
            }
        });

        Assert.Equal("none", (await new EngineReviewService(model, Store).ReviewAsync(project, "one", "First game")).ChangeKind);
        Assert.Equal(5, model.Calls);
    }

    [Fact]
    public async Task EvidenceLimitsAreAdvertisedAndFailuresIdentifyOnlyTheInvalidEvidenceField()
    {
        var project = await ProjectAsync();
        var turn = 0;
        const string invalid = "adaptation/current/work/not-read.py";
        var model = new Model(request =>
        {
            if (turn++ == 0)
            {
                var schema = Assert.Single(request.Tools, tool => tool.Name == "finish_review").Parameters;
                var evidence = schema.GetProperty("properties").GetProperty("improvement").GetProperty("anyOf")[1]
                    .GetProperty("properties").GetProperty("evidence");
                Assert.Equal(1, evidence.GetProperty("minItems").GetInt32());
                Assert.Equal(8, evidence.GetProperty("maxItems").GetInt32());
                return FinishWithEvidence(Enumerable.Repeat("overview", 9).ToArray());
            }
            using var response = JsonDocument.Parse(request.Messages.Last(message => message.Role == "tool").Content);
            Assert.False(response.RootElement.GetProperty("success").GetBoolean());
            var error = response.RootElement.GetProperty("error").GetString();
            Assert.Contains("improvement.evidence", error);
            if (turn == 2)
            {
                Assert.Contains("9", error);
                Assert.Contains("1–8", error);
                Assert.Contains("仅调整 evidence", error);
                return FinishWithEvidence([invalid]);
            }
            Assert.Contains(invalid, error);
            Assert.Contains("overview", error);
            Assert.Contains("不必重写其他结论", error);
            return FinishWithEvidence(["overview"]);
        });

        Assert.Equal("created", (await new EngineReviewService(model, Store).ReviewAsync(project, "one", "First game")).ChangeKind);
        Assert.Equal(3, model.Calls);
        Assert.Equal("overview", Assert.Single(Assert.Single(Store.ReadAll()).Origins).Evidence.Single());
    }

    private static AgentTurn FinishWithEvidence(string[] evidence) => Call("finish_review", new
    {
        summary = "已有工具证据支持相同变体的最小核对路线。", mainProgramSuggestions = Array.Empty<string>(),
        improvement = new
        {
            key = "tool-evidence", family = new AdapterFamily("engine", "tblstr", "TBLSTR"), aliases = Array.Empty<string>(),
            conditions = "仅在表头、记录边界及主记录索引一致时使用。", summary = "先核对表结构。",
            guidance = "结构匹配时复用既有解析器并验证当前独立样本；不符时转查其他变体。", evidence, tools = Array.Empty<object>()
        }
    });
}
