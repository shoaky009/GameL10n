using System.Text.Json;
using GameL10n.Core.Adapters;
using GameL10n.Core.Development;
using GameL10n.Core.Storage;

namespace GameL10n.Tests.Development;

public sealed partial class EngineReviewTests
{
    [Fact]
    public async Task UiStateAndUnrelatedKnowledgeDoNotRestartReviewButSourceAndRelatedKnowledgeDo()
    {
        var project = await ProjectAsync();
        var path = Path.Combine(project, "game.json");
        string Manifest(string stage, string source = "source-1") => JsonSerializer.Serialize(new
        {
            id = "one", gameDirectory = Path.Combine(root, "original"), adapterId = "engine.yuris480-ypf",
            adapterVersion = "one", analysis = new { sourceFingerprint = source },
            taskSummary = new { stage }, settings = new { batchSize = stage.Length }, updatedAt = DateTimeOffset.UtcNow
        }, Json);
        await File.WriteAllTextAsync(path, Manifest("running"));
        var model = new Model(_ =>
        {
            // Stage/settings writes during the task are not a change to extraction evidence.
            File.WriteAllText(path, Manifest("completed"));
            return Call("finish_review", new { summary = "没有新的可靠优化。", improvement = (object?)null });
        });
        var service = new EngineReviewService(model, Store);
        await service.ReviewAsync(project, "one", "First game");
        await File.WriteAllTextAsync(path, Manifest("selected-again"));
        Assert.True((await service.ReviewAsync(project, "one", "First game")).Reused);
        await Store.SaveAsync(Entry() with { Family = new("engine", "unity-text", "Unity / TextAsset"), Aliases = ["Unity"] }, null);
        Assert.True((await service.ReviewAsync(project, "one", "First game")).Reused);
        Assert.Equal(1, model.Calls);
        await Store.SaveAsync(Entry(), null);
        Assert.False((await service.ReviewAsync(project, "one", "First game")).Reused);
        Assert.Equal(2, model.Calls);

        await File.WriteAllTextAsync(path, Manifest("updated-resource", "source-2"));
        var changed = new Model(_ => Call("finish_review", new { summary = "新资源仍无需新增经验。", improvement = (object?)null }));
        Assert.False((await new EngineReviewService(changed, Store).ReviewAsync(project, "one", "First game")).Reused);
        Assert.Equal(1, changed.Calls);
    }

    [Fact]
    public void FamilyCandidatesFindVariantsWithoutEquatingThemAndExactMatchesRankFirst()
    {
        var variant = Entry() with
        {
            Key = "adler-filter", Family = new("engine", "kirikiri-xp3-adler-filter", "KiriKiri XP3 / Adler filter"), Aliases = []
        };
        var exact = variant with { Family = new("engine", "vntextpatch-kirikiriks", "Kirikiri / KS") };
        var unrelated = variant with { Family = new("engine", "unity-text", "Unity / TextAsset") };
        var entries = EngineKnowledgeStore.Match(new[] { variant, unrelated, exact }, ["KirikiriKs"]);
        Assert.Equal(new[] { exact.Id, variant.Id }, entries.Select(entry => entry.Id));
        Assert.Empty(EngineKnowledgeStore.Match(new[] { variant }, ["Unity"]));
        Assert.Empty(EngineKnowledgeStore.Match(new[] { variant with { Enabled = false } }, ["KirikiriKs"]));
        Assert.Single(EngineKnowledgeStore.Match(new[] { Entry() with { Family = new("engine", "yuris480-ypf", "YU-RIS 480 / YPF") } }, ["Yuris"]));
    }

    [Fact]
    public async Task LargeReviewEvidenceIsBoundedAndOriginalResultsRemainRetrievableWithoutPayloadLogs()
    {
        var project = await ProjectAsync();
        for (var index = 0; index < 10; index++)
            await File.WriteAllTextAsync(Path.Combine(project, $"adaptation/current/work/evidence{index}.txt"),
                $"PRIVATE_EVIDENCE_{index}:" + new string((char)('汉' + index), 10000));
        var turn = 0;
        string? originalId = null, retrievalId = null;
        var model = new Model(request =>
        {
            if (turn < 10)
            {
                var reply = Call("read_evidence", new { path = $"adaptation/current/work/evidence{turn++}.txt", length = 12000 });
                originalId ??= reply.Message.ToolCalls![0].Id;
                return reply;
            }
            if (turn++ == 10)
            {
                Assert.Contains(request.Messages, message => message.Content.Contains("previousResultOmitted", StringComparison.Ordinal));
                Assert.True(AutomaticAdaptationService.VisibleHistoryBytes(request.Messages) < 130 * 1024);
                var reply = Call("read_tool_result", new { toolCallId = originalId, length = 500 });
                retrievalId = reply.Message.ToolCalls![0].Id;
                return reply;
            }
            Assert.Contains("PRIVATE_EVIDENCE_0", Assert.Single(request.Messages, message => message.ToolCallId == retrievalId).Content);
            return Call("finish_review", new { summary = "没有可发布的新结论。", improvement = (object?)null });
        });
        var result = await new EngineReviewService(model, Store).ReviewAsync(project, "one", "First game");
        Assert.Equal(12, model.Calls);
        Assert.Equal("none", result.ChangeKind);
        var trace = Assert.Single(Directory.GetFiles(Path.Combine(project, "logs"), "engine-review-*.jsonl"));
        Assert.DoesNotContain("PRIVATE_EVIDENCE_", await File.ReadAllTextAsync(trace));
        var modelRows = 0;
        foreach (var line in await File.ReadAllLinesAsync(trace))
        {
            using var row = JsonDocument.Parse(line);
            if (row.RootElement.GetProperty("entry").GetProperty("operation").GetString() == "model") modelRows++;
        }
        Assert.Equal(model.Calls, modelRows);
        Assert.True((await new EngineReviewService(model, Store).ReviewAsync(project, "one", "First game")).Reused);
        Assert.Equal(12, model.Calls);
    }

    [Fact]
    public async Task ReviewEvidenceSupportsTargetedReadsAndOverviewWithoutWholeLogPaging()
    {
        var project = await ProjectAsync();
        await File.WriteAllTextAsync(Path.Combine(project, "game.json"), "{}");
        var path = Path.Combine(project, "adaptation/current/work/status.txt");
        await File.WriteAllTextAsync(path, "old investigation\nDECISIVE_CALL: verified change\nother detail");
        var evidence = await EngineReviewEvidence.LoadAsync(project, default);
        Assert.Empty(evidence.EngineKeys);
        using var found = JsonDocument.Parse(JsonSerializer.Serialize(await evidence.ReadAsync(
            "adaptation/current/work/status.txt", 0, 20, default, "DECISIVE_CALL"), Json));
        Assert.True(found.RootElement.GetProperty("queryFound").GetBoolean());
        Assert.StartsWith("DECISIVE_CALL:", found.RootElement.GetProperty("content").GetString());
        using var overview = JsonDocument.Parse(JsonSerializer.Serialize(await evidence.ReadAsync("overview", 0, 12000, default), Json));
        Assert.Contains("recentFailures", overview.RootElement.GetProperty("content").GetString());
        Assert.True(evidence.IsEvidence("overview"));
    }
}
