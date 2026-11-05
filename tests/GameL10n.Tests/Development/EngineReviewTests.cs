using System.Text.Json;
using GameL10n.Core;
using GameL10n.Core.Adapters;
using GameL10n.Core.Development;
using GameL10n.Core.Storage;

namespace GameL10n.Tests.Development;

public sealed partial class EngineReviewTests : IDisposable
{
    private readonly string root = Path.Combine(Path.GetTempPath(), "gamel10n-engine-review-" + Guid.NewGuid().ToString("N"));
    private EngineKnowledgeStore Store => new(Path.Combine(root, "extensions", "engine-knowledge"));
    private static readonly JsonSerializerOptions Json = new(JsonSerializerDefaults.Web);

    [Fact]
    public async Task SummaryPublishesGlobalHelpersAndRepeatedClicksReuseIncludingNoImprovement()
    {
        var project = await ProjectAsync();
        var reads = 0;
        var model = new Model(_ => ++reads == 1
            ? Call("read_evidence", new { path = "adaptation/current/work/probe.py" })
            : Call("finish_review", new
            {
                summary = "已提炼同类 YBN 的探测方式。", mainProgramSuggestions = Array.Empty<string>(),
                improvement = new { key = "ybn-layout", family = new AdapterFamily("engine", "yuris", "YU-RIS"), aliases = new[] { "Yuris" },
                    conditions = "YBN v480；应核对版本和字段结构。", summary = "先验证文本布局", guidance = "先运行结构探测，再对当前样本做原文回填。",
                    evidence = new[] { "adaptation/current/work/probe.py" },
                    tools = new[] { new { name = "probe", description = "结构探测", usage = "使用 run_engine_python，以 arguments 传入当前样本。",
                        sourceFiles = new[] { "adaptation/current/work/probe.py" } } } }
            }));
        var service = new EngineReviewService(model, Store);
        var report = await service.ReviewAsync(project, "one", "First game");
        Assert.Equal("engine.yuris.ybn-layout", report.EntryId);
        Assert.Equal("created", report.ChangeKind);
        Assert.Equal(2, model.Calls);
        var global = Assert.Single(Store.ReadAll());
        Assert.Contains("work/probe.py", Assert.Single(global.Tools).Files.Keys);
        Assert.Single(Directory.GetFiles(Store.DirectoryPath));
        Assert.False(Directory.Exists(Path.Combine(project, "engine-knowledge")));
        Assert.True((await service.ReviewAsync(project, "one", "First game", limits: new(MaximumTokens: 20_000_000))).Reused);
        Assert.Equal(2, model.Calls);

        await File.AppendAllTextAsync(Path.Combine(project, "adaptation/current/work/probe.py"), "\n# new observed result");
        var noImprovement = new Model(_ => Call("finish_review", new
        { summary = "现有经验已涵盖这些结果，没有新增优化空间。", improvement = (object?)null, mainProgramSuggestions = new[] { "宿主性能需单独测量。" } }));
        var second = new EngineReviewService(noImprovement, Store);
        var declined = await second.ReviewAsync(project, "one", "First game");
        Assert.Null(declined.EntryId);
        Assert.Equal("none", declined.ChangeKind);
        Assert.Contains("没有新增", declined.Summary);
        Assert.True((await second.ReviewAsync(project, "one", "First game")).Reused);
        Assert.Equal(1, noImprovement.Calls);
        Assert.Equal(global.Revision, Assert.Single(Store.ReadAll()).Revision);
    }

    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public async Task SummaryUsesAdaptationLimitsBeyondOldTokenAndTurnCaps(bool useOverrides)
    {
        var project = await ProjectAsync();
        AdaptationLimits? limits = useOverrides ? new(18, 20_000_000, 24_000) : null;
        var calls = 0;
        var model = new Model(request =>
        {
            Assert.Equal(useOverrides ? 24_000 : 10_000, request.MaximumOutputTokens);
            var reply = ++calls < 18
                ? Call("read_evidence", new { path = "adaptation/current/work/probe.py" })
                : Call("finish_review", new { summary = "没有新增可靠结论。", improvement = (object?)null });
            return reply with { Usage = new(6000, 1000) };
        });

        var result = await new EngineReviewService(model, Store).ReviewAsync(project, "one", "First game", limits: limits);

        Assert.Equal(18, model.Calls);
        Assert.Equal(126_000, result.Usage.TotalTokens);
        Assert.False(result.Reused);
    }

    [Theory]
    [InlineData(2, 1_000_000, 2, "2 轮模型调用上限")]
    [InlineData(60, 20_000, 1, "Token 预算不足")]
    public async Task SummaryStopsAtConfiguredLimitsWithoutPublishingPartialKnowledge(
        int maximumTurns, long maximumTokens, int expectedCalls, string errorText)
    {
        var project = await ProjectAsync();
        var model = new Model(_ => Call("read_evidence", new { path = "adaptation/current/work/probe.py" })
            with { Usage = new(10_000, 10_000) });

        var error = await Assert.ThrowsAsync<InvalidDataException>(() => new EngineReviewService(model, Store)
            .ReviewAsync(project, "one", "First game", limits: new(maximumTurns, maximumTokens)));

        Assert.Contains(errorText, error.Message);
        Assert.Equal(expectedCalls, model.Calls);
        Assert.False(File.Exists(Path.Combine(project, "engine-review.json")));
        Assert.Empty(Store.ReadAll());
    }

    [Fact]
    public async Task SharedKnowledgeIsConditionalAndHelpersAreCopiedWithoutExecution()
    {
        var entry = await Store.SaveAsync(Entry(), null);
        var project = await ProjectAsync();
        var packages = new LocalAdapterPackageStore(Path.Combine(root, "extensions"));
        var toolchain = new PinnedDotNetToolchain(Path.Combine(root, "unused"), "10.0.402", "10.0.13");
        var runner = new NeverRun();
        var progress = new List<(string Stage, string Message)>();
        AdaptationTools Tools(string format, string workspace) => new(new("another-game", project, []), new AdaptationSession
        {
            BuiltinDiscovery = new(BuiltinAdapterDiscovery.Revision, 1, false,
                [new(format, "builtin." + format, format, "header", [], [], [])], [])
        }, workspace, new(toolchain, runner), packages, reportProgress: (stage, message) => progress.Add((stage, message)));
        var related = Tools("Yuris", Path.Combine(root, "other-workspace"));
        var unrelated = Tools("Unity", Path.Combine(root, "unity-workspace"));
        using var matches = JsonDocument.Parse(await related.ExecuteAsync(new("lookup", "inspect_engine_knowledge", "{}"), default));
        Assert.Equal(entry.Id, matches.RootElement.GetProperty("result").GetProperty("entries")[0].GetProperty("id").GetString());
        using var none = JsonDocument.Parse(await unrelated.ExecuteAsync(new("lookup", "inspect_engine_knowledge", "{}"), default));
        Assert.Empty(none.RootElement.GetProperty("result").GetProperty("entries").EnumerateArray());
        Assert.Empty(progress);
        var readArgs = JsonSerializer.Serialize(new { id = entry.Id });
        var detail = await related.ExecuteAsync(new("read", "inspect_engine_knowledge", readArgs), default);
        await related.ExecuteAsync(new("reread", "inspect_engine_knowledge", readArgs), default);
        var readNotice = Assert.Single(progress);
        Assert.Equal("参考引擎经验", readNotice.Stage);
        Assert.Contains(entry.Family.Name, readNotice.Message);
        Assert.Contains(entry.Summary, readNotice.Message);
        var readTrace = JsonSerializer.SerializeToElement(AdaptationTrace.Result(detail), Json);
        Assert.Equal(entry.Family.Name, readTrace.GetProperty("knowledgeName").GetString());
        Assert.Equal(entry.Revision, readTrace.GetProperty("revision").GetString());
        var args = JsonSerializer.Serialize(new { id = entry.Id, tool = "probe" });
        var copyResult = await related.ExecuteAsync(new("copy", "copy_engine_knowledge_tools", args), default);
        using var copied = JsonDocument.Parse(copyResult);
        await related.ExecuteAsync(new("recopy", "copy_engine_knowledge_tools", args), default);
        Assert.Equal(2, progress.Count);
        Assert.Equal("复用经验工具", progress[1].Stage);
        Assert.Contains(entry.Family.Name, progress[1].Message);
        Assert.Contains("probe", progress[1].Message);
        var copyTrace = JsonSerializer.SerializeToElement(AdaptationTrace.Result(copyResult), Json);
        Assert.Equal(entry.Family.Name, copyTrace.GetProperty("knowledgeName").GetString());
        Assert.Equal("probe", copyTrace.GetProperty("knowledgeTool").GetString());
        var relative = copied.RootElement.GetProperty("result").GetProperty("files")[0].GetString()!;
        Assert.Equal("import sys; print(sys.argv[1])", await File.ReadAllTextAsync(Path.Combine(root, "other-workspace", relative)));
        await Store.SetEnabledAsync(entry.Id, false);
        Assert.Empty(EngineKnowledgeStore.Match(Store.ReadAll(), ["Yuris"]));
        // An ongoing adaptation keeps its captured references; the next task observes the disable.
        using var pinned = JsonDocument.Parse(await related.ExecuteAsync(new("lookup", "inspect_engine_knowledge", "{}"), default));
        Assert.Single(pinned.RootElement.GetProperty("result").GetProperty("entries").EnumerateArray());
    }

    [Fact]
    public async Task UpdatingExperiencePreservesHelpersAndOriginsAndRejectsStaleWriters()
    {
        var first = await Store.SaveAsync(Entry(), null);
        var update = first with { Guidance = "新增格式证据；原来的回填检查仍然必要。", Tools = [], Origins = [new("two", "Second game", "hash-2", ["overview"])] };
        var saved = await Store.SaveAsync(update, first.Revision);
        Assert.Single(saved.Tools);
        Assert.Equal(2, saved.Origins.Count);
        Assert.NotEqual(first.Revision, saved.Revision);
        await Assert.ThrowsAsync<InvalidDataException>(() => Store.SaveAsync(first, first.Revision));
        Assert.Equal(saved.Revision, Assert.Single(Store.ReadAll()).Revision);
    }

    [Fact]
    public async Task LibraryMetadataDoesNotMakeChangedKnowledgeTrusted()
    {
        var saved = await Store.SaveAsync(Entry(), null);
        var summary = Assert.Single(Store.ReadSummaries());
        Assert.Equal(saved.Id, summary.Id);
        Assert.Equal(1, summary.ToolCount);
        var path = Path.Combine(Store.DirectoryPath, saved.Id + ".json");
        var altered = saved with { Guidance = "未经过版本校验的修改" };
        await LocalFiles.WriteJsonAsync(path, altered, FixtureJson.TypeInfo<global::GameL10n.Core.Development.EngineKnowledgeEntry>(FixtureJson.Storage));
        Assert.Single(Store.ReadSummaries());
        Assert.Throws<InvalidDataException>(() => Store.ReadEntry(saved.Id));
        Assert.Empty(Store.ReadAll());
    }

    [Fact]
    public async Task LibraryKnowledgeSummaryIsUpdatedWithoutReopeningItsSource()
    {
        var entry = await Store.SaveAsync(Entry(), null);
        var packages = new LocalAdapterPackageStore(Path.Combine(root, "extensions"));
        await Store.SetEnabledAsync(entry.Id, false);
        File.Delete(Path.Combine(Store.DirectoryPath, entry.Id + ".json"));
        var snapshot = await packages.ReadEngineLibraryAsync([], root);
        var summary = Assert.Single(snapshot.Knowledge);
        Assert.Equal(entry.Id, summary.Id);
        Assert.False(summary.Enabled);
        Assert.Equal(1, summary.ToolCount);
        Assert.Null(Store.ReadEntry(entry.Id));
        var index = await File.ReadAllTextAsync(Path.Combine(packages.RootDirectory, "library-index.json"));
        Assert.DoesNotContain("Console.WriteLine", index);
        Assert.DoesNotContain(entry.Guidance, index);
    }

    [Fact]
    public async Task ChangedOrOutOfProjectEvidenceCannotBePublished()
    {
        var project = await ProjectAsync();
        var evidence = await EngineReviewEvidence.LoadAsync(project, default);
        await Assert.ThrowsAsync<InvalidDataException>(() => evidence.ReadAsync("../secret.txt", 0, 100, default));
        await Assert.ThrowsAsync<InvalidDataException>(() => evidence.ToolSourceAsync("adaptation/current/work/probe.py", default));
        await evidence.ReadAsync("adaptation/current/work/probe.py", 0, 100, default);
        await File.WriteAllTextAsync(Path.Combine(project, "adaptation/current/work/probe.py"), "changed");
        await Assert.ThrowsAsync<IOException>(() => evidence.EnsureUnchangedAsync(default));
        await Assert.ThrowsAsync<IOException>(() => evidence.ToolSourceAsync("adaptation/current/work/probe.py", default));
    }

    private async Task<string> ProjectAsync()
    {
        var project = Path.Combine(root, "project");
        Directory.CreateDirectory(Path.Combine(project, "adaptation/current/work"));
        await File.WriteAllTextAsync(Path.Combine(project, "adaptation/current/work/probe.py"), "import sys; print(sys.argv[1])");
        return project;
    }

    private static EngineKnowledgeEntry Entry() => new("ybn-layout", new("engine", "yuris", "YU-RIS"), ["Yuris"],
        "YBN v480，先核对结构。", "检查文本布局", "检查字段边界和原文字节回填。",
        [new("probe", "结构检查", "run_engine_python with a sample path", new Dictionary<string, string> { ["work/probe.py"] = "import sys; print(sys.argv[1])" })],
        [new("one", "First game", "hash-1", ["overview"])]);
    private static AgentTurn Call(string name, object args) => new(new("assistant", ToolCalls: [new(Guid.NewGuid().ToString("N"), name, JsonSerializer.Serialize(args, Json))]), new(20, 10));
    private sealed class Model(Func<AgentRequest, AgentTurn> reply) : IAdaptationAgentModel
    {
        public string Identity => "review-fixture";
        public int Calls { get; private set; }
        public Task<AgentTurn> NextAsync(AgentRequest request, CancellationToken cancellationToken) { Calls++; return Task.FromResult(reply(request)); }
    }
    private sealed class NeverRun : IAdapterProcessRunner
    {
        public Task<AdapterProcessResult> RunAsync(AdapterProcessRequest request, CancellationToken cancellationToken) => throw new Exception("Copying reference tools must not execute them.");
    }
    public void Dispose() { if (Directory.Exists(root)) Directory.Delete(root, true); }
}
