using System.Diagnostics;
using System.Text.Json;
using AvgTranslator.Core.Adapters;
using AvgTranslator.Core.Development;
using Xunit.Abstractions;

namespace AvgTranslator.Tests.Development;

public sealed class GameDiscoveryTests(ITestOutputHelper output) : IDisposable
{
    private readonly string root = Path.Combine(Path.GetTempPath(), "avg-discovery-" + Guid.NewGuid().ToString("N"));

    private async Task<string> ResourceAsync(string path, byte[] content)
    {
        var target = Path.Combine(root, "game", path);
        Directory.CreateDirectory(Path.GetDirectoryName(target)!);
        await File.WriteAllBytesAsync(target, content);
        return Path.Combine(root, "game");
    }

    [Fact]
    public async Task LayeredRulesUseSameFileAndKeepMissingHintsUnknownVersionsAndLegacyCandidates()
    {
        var game = await ResourceAsync("data/script.pak", [1, 2, 3, 4, 0xAA, 0xBB]);
        await ResourceAsync("other.pak", [8, 9]);
        var recognition = new AdapterRecognition(1,
            [new(["**/*.pak"], MinimumSize: 6, Signatures: [new(0, "0102"), new(0, "AABB", "end")])],
            [new(["optional.ini"])]);
        var session = GameDiscoverySession.Scan(game);
        var descriptor = new AdapterDiscoveryDescriptor([], ["**/*.pak"], recognition);
        Assert.Equal(RecognitionStatus.Unknown, (await session.EvaluateAsync(descriptor, false)).Status);
        Assert.Equal(0, session.RangeReads);
        Assert.Equal(RecognitionStatus.Match, (await session.EvaluateAsync(descriptor, true)).Status);
        Assert.Equal(2, session.RangeReads);
        var candidates = new[]
        {
            Candidate("engine.good", "one", descriptor),
            Candidate("engine.good", "two", descriptor),
            Candidate("engine.old", "one", descriptor with { Recognition = null }),
            Candidate("engine.future", "one", descriptor with { Recognition = recognition with { SchemaVersion = 2 } }),
            Candidate("engine.unknown", "one", null),
            Candidate("engine.wrong", "one", descriptor with { Recognition = recognition with
                { Required = [new(["**/*.pak"], MinimumSize: 6, Signatures: [new(0, "0809")])] } }),
            Candidate("engine.absent", "one", new([], ["**/*.ain"], null))
        };
        var selected = await AdapterDiscoveryPlanner.SelectAsync(session, candidates);
        Assert.Equal(5, selected.Count);
        Assert.Equal(new[] { "engine.good", "engine.good" }, selected.Take(2).Select(item => item.Id));
        Assert.Equal(2, session.RangeReads); // All equivalent signatures share one read per slice.
        Assert.DoesNotContain(selected, item => item.Id is "engine.wrong" or "engine.absent");
    }

    [Fact]
    public async Task ReadBudgetAndChangedFilesRemainUnknownRatherThanUnsupported()
    {
        var game = await ResourceAsync("scripts/scene.pak", [1]);
        for (int i = 0; i < 513; i++) await ResourceAsync($"scripts/{i}.pak", [1]);
        var session = GameDiscoverySession.Scan(game);
        for (int i = 0; i < 512; i++) await session.ReadRangeAsync($"scripts/{i}.pak", 0, 1, default);
        var descriptor = new AdapterDiscoveryDescriptor([], ["**/*.pak"],
            new(1, [new(["scripts/scene.pak"], Signatures: [new(0, "01")])], []));
        Assert.Equal(RecognitionStatus.Unknown, (await session.EvaluateAsync(descriptor, true)).Status);
        Assert.Equal(512, session.RangeReads);
        var fresh = GameDiscoverySession.Scan(game);
        await ResourceAsync("scripts/scene.pak", [1, 2]);
        Assert.Equal(RecognitionStatus.Unknown, (await fresh.EvaluateAsync(descriptor, true)).Status);
    }

    [Fact]
    public async Task ThousandEnginesAndFiveThousandVersionsUseOnlyIndexedMetadataAndSharedSignatureReads()
    {
        var game = await ResourceAsync("data/main.pak", [0x41, 0x42, 0x43, 0x44]);
        var store = new LocalAdapterPackageStore(Path.Combine(root, "extensions"));
        var families = new Dictionary<string, object>();
        for (int engine = 0; engine < 1000; engine++)
        {
            var family = new AdapterFamily("engine", "fixture" + engine, "Fixture " + engine);
            var recognition = new AdapterRecognition(1, [new(["**/*.pak"], Signatures: [new(0, engine == 777 ? "4142" : "0000")])], []);
            var descriptor = new AdapterDiscoveryDescriptor([], engine is 777 or 778 ? ["**/*.pak"] : [$"**/*.format{engine}"], recognition);
            families[family.AdapterId] = new
            {
                family, enabled = true,
                versions = Enumerable.Range(0, 5).ToDictionary(version => "v" + version, version => new
                {
                    version = "v" + version, source = EngineLibrarySource.External, exists = true,
                    publishedAt = DateTimeOffset.UnixEpoch.AddSeconds(version), isCandidate = true,
                    discovery = descriptor, discoveryLoaded = true
                }),
                games = Enumerable.Range(0, 10).Select(i => new { gameId = $"g{engine}-{i}", title = "Fixture", variant = "", adapterVersion = "v0" })
            };
        }
        Directory.CreateDirectory(store.RootDirectory);
        await File.WriteAllTextAsync(Path.Combine(store.RootDirectory, "library-index.json"), JsonSerializer.Serialize(new
        {
            schemaVersion = 1, discoveryInitialized = true, families, knowledge = new { }, diagnostics = Array.Empty<string>()
        }, new JsonSerializerOptions(JsonSerializerDefaults.Web)));
        var session = GameDiscoverySession.Scan(game);
        var watch = Stopwatch.StartNew();
        var indexed = await store.ReadDiscoveryCandidatesAsync();
        var selected = await AdapterDiscoveryPlanner.SelectAsync(session, indexed);
        watch.Stop();
        Assert.Equal(5000, indexed.Count);
        Assert.Equal(5, selected.Count);
        Assert.All(selected, candidate => Assert.Equal("engine.fixture777", candidate.Id));
        Assert.Equal(1, session.RangeReads);
        Assert.False(Directory.Exists(Path.Combine(store.RootDirectory, "packages")));
        output.WriteLine($"1,000 engines / 5,000 versions / 10,000 compatibility records: index + selection {watch.ElapsedMilliseconds} ms; signature reads {session.RangeReads}.");
        watch.Restart();
        var again = await new LocalAdapterPackageStore(store.RootDirectory).ReadDiscoveryCandidatesAsync();
        await AdapterDiscoveryPlanner.SelectAsync(GameDiscoverySession.Scan(game), again);
        output.WriteLine($"Warm repeated index + scan + selection: {watch.ElapsedMilliseconds} ms.");
    }

    private static AdapterDiscoveryCandidate Candidate(string id, string version, AdapterDiscoveryDescriptor? descriptor) =>
        new(id, version, null, descriptor, new(RecognitionStatus.Unknown, 0));

    public void Dispose() { if (Directory.Exists(root)) Directory.Delete(root, true); }
}
