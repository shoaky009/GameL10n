using System.IO.Compression;
using System.Text.Json;
using GameL10n.Core.Development;

namespace GameL10n.Tests.Development;

public sealed partial class AutomaticAdaptationTests
{
    private const string SearchReferenceRevision = "0123456789012345678901234567890123456789";

    [Theory]
    [InlineData("XP3 archive kirikiri")]
    [InlineData("XP3/archives:KiriKiri")]
    [InlineData("python/archives/xp3.py kirikiri")]
    public async Task ReferenceSearchRanksMixedQueriesAgainstPinnedLocalCatalog(string query)
    {
        using var client = ReferenceSearchOfflineClient();
        var workspace = Path.Combine(root, "workspace");
        var tools = CreateReferenceSearchTools(client, workspace);
        using var response = JsonDocument.Parse(await tools.ExecuteAsync(new("search", "search_engine_references",
            JsonSerializer.Serialize(new { query })), default));
        var result = response.RootElement.GetProperty("result");
        var first = result.GetProperty("matches")[0];
        Assert.Equal("kirikiri", first.GetProperty("id").GetString());
        Assert.Equal("XP3 container only", first.GetProperty("scope").GetString());
        Assert.Equal("Synthetic vectors; game support unverified", first.GetProperty("evidence").GetString());
        Assert.Contains("/" + SearchReferenceRevision + "/", first.GetProperty("documentUrl").GetString());
        Assert.EndsWith("/python/archives/xp3.py", first.GetProperty("sourceUrls")[0].GetString());
        Assert.True(result.GetProperty("reused").GetBoolean());
        Assert.Contains("Kirikiri 脚本编码", result.GetProperty("guidance").ToString());
        Assert.False(Directory.Exists(workspace));
    }

    [Fact]
    public async Task ReferenceSearchPrioritizesExactIdsThenAliasesAndKeepsResultsBoundedAndStable()
    {
        using var client = ReferenceSearchOfflineClient();
        var tools = CreateReferenceSearchTools(client, Path.Combine(root, "workspace"));
        using var exact = JsonDocument.Parse(await tools.ExecuteAsync(new("exact", "search_engine_references",
            """{"query":"kirikiri"}"""), default));
        Assert.Equal(["kirikiri", "z-alias", "a-related"], exact.RootElement.GetProperty("result").GetProperty("matches")
            .EnumerateArray().Select(item => item.GetProperty("id").GetString()!).ToArray());
        using var alias = JsonDocument.Parse(await tools.ExecuteAsync(new("alias", "search_engine_references",
            """{"query":"KRKR2"}"""), default));
        Assert.Equal("kirikiri", Assert.Single(alias.RootElement.GetProperty("result").GetProperty("matches")
            .EnumerateArray()).GetProperty("id").GetString());
        foreach (var callId in new[] { "bounded", "repeat" })
        {
            using var bounded = JsonDocument.Parse(await tools.ExecuteAsync(new(callId, "search_engine_references",
                """{"query":"XP3"}"""), default));
            var result = bounded.RootElement.GetProperty("result");
            Assert.Equal(11, result.GetProperty("totalMatches").GetInt32());
            Assert.True(result.GetProperty("more").GetBoolean());
            Assert.Equal(["xp3", "a-related", "kirikiri", "reader-a", "reader-b", "reader-c"],
                result.GetProperty("matches").EnumerateArray().Select(item => item.GetProperty("id").GetString()!).ToArray());
        }
    }

    [Theory]
    [InlineData("archive engine")]
    [InlineData("python/archives/engine.py")]
    [InlineData("unrelated mystery")]
    [InlineData("rio archive")]
    [InlineData("Kaguya")]
    public async Task ReferenceSearchDoesNotRecommendFromGenericTermsOrArbitrarySubstrings(string query)
    {
        using var client = ReferenceSearchOfflineClient();
        var tools = CreateReferenceSearchTools(client, Path.Combine(root, "workspace"));
        using var response = JsonDocument.Parse(await tools.ExecuteAsync(new("search", "search_engine_references",
            JsonSerializer.Serialize(new { query })), default));
        var result = response.RootElement.GetProperty("result");
        Assert.Equal("queried", result.GetProperty("status").GetString());
        Assert.Equal(0, result.GetProperty("totalMatches").GetInt32());
        Assert.Empty(result.GetProperty("matches").EnumerateArray());
        Assert.Contains("未命中不代表不支持", result.GetProperty("hint").GetString());
        Assert.DoesNotContain("Kirikiri 脚本编码", result.GetProperty("guidance").ToString());
    }

    [Theory]
    [InlineData("Hxv4")]
    [InlineData("PSB SCN")]
    [InlineData("KAGEX")]
    public async Task ReferenceSearchFindsSemanticSupportWhenTheCatalogListsOnlyTheContainer(string query)
    {
        using var client = ReferenceSearchOfflineClient();
        var tools = CreateReferenceSearchTools(client, Path.Combine(root, "workspace"));
        using var response = JsonDocument.Parse(await tools.ExecuteAsync(new("search", "search_engine_references",
            JsonSerializer.Serialize(new { query })), default));
        var match = Assert.Single(response.RootElement.GetProperty("result").GetProperty("matches").EnumerateArray());
        Assert.Equal("kirikiri", match.GetProperty("id").GetString());
        Assert.Equal("kirikiri-scn-text", match.GetProperty("readySupport").GetProperty("component").GetString());
        Assert.Contains("过滤", match.GetProperty("readySupport").GetProperty("condition").GetString());
    }

    private AdaptationTools CreateReferenceSearchTools(HttpClient client, string workspace)
    {
        var engines = new List<object>
        {
            new { id = "a-related", title = "KiriKiri Toolkit", aliases = new[] { "XP3" },
                document = "engines/related.md", python_references = new[] { "python/engines/related.py" } },
            new { id = "z-alias", title = "Other archive", aliases = new[] { "KiriKiri" },
                document = "engines/other.md", python_references = new[] { "python/archives/other.py" } },
            new { id = "generic", title = "Archive engine", document = "engines/generic.md",
                python_references = new[] { "python/engines/scenario.py" } },
            new { id = "xp3", title = "XP3 archive", document = "engines/xp3.md",
                python_references = new[] { "python/archives/xp3.py" } },
            new { id = "kirikiri", title = "KiriKiri / TJS2", aliases = new[] { "KRKR2" },
                document = "engines/kirikiri.md", python_references = new[] { "python/archives/xp3.py" },
                scope = "XP3 container only", evidence = "Synthetic vectors; game support unverified" }
        };
        for (var letter = 'h'; letter >= 'a'; letter--)
            engines.Add(new { id = "reader-" + letter, title = "Archive reader", document = "engines/reader.md",
                python_references = new[] { "python/archives/xp3.py" } });
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var library = Path.Combine(store.RootDirectory, "engine-tools", "references", SearchReferenceRevision, "library.zip");
        Directory.CreateDirectory(Path.GetDirectoryName(library)!);
        using (var zip = ZipFile.Open(library, ZipArchiveMode.Create))
        using (var writer = new StreamWriter(zip.CreateEntry("catalog/engines.json").Open()))
            writer.Write(JsonSerializer.Serialize(new { engines }));
        var pin = new EngineReferencePin(SearchReferenceRevision, Hash(library), Hash(library));
        return new AdaptationTools(Request(), new() { EngineReference = pin }, workspace,
            Runtime(new ScriptedRunner()), store, client);
    }

    private static HttpClient ReferenceSearchOfflineClient() => new(new ResearchHandler(_ =>
        throw new InvalidOperationException("Pinned reference searches must not make HTTP requests.")));
}
