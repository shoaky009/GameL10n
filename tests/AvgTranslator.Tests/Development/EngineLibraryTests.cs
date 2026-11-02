using AvgTranslator.Core.Adapters;
using AvgTranslator.Core.Development;
using AvgTranslator.Core.Storage;

namespace AvgTranslator.Tests.Development;

public sealed class EngineLibraryTests : IDisposable
{
    private readonly string root = Path.Combine(Path.GetTempPath(), "avg-engine-library-" + Guid.NewGuid().ToString("N"));
    private const string GameId = "0123456789abcdef01234567";
    private static readonly AdapterFamily Family = new("engine", "library-fixture", "Library fixture");
    private LocalAdapterPackageStore Store => new(Path.Combine(root, "data", "extensions"));
    private string Data => Path.Combine(root, "data");

    [Fact]
    public async Task DisabledFamilyKeepsExistingPinAndDoesNotHashPackagesForBrowsing()
    {
        var store = Store;
        var package = await PublishAsync("one");
        await store.SetFamilyEnabledAsync(Family.AdapterId, false);
        Assert.Empty(new LocalAdapterPackageStore(store.RootDirectory).ListReusablePackages());
        Assert.Equal("one", store.LoadActive(GameId)!.Version);
        Assert.Contains(Family.AdapterId, store.ReadDisabledFamilyIds());

        // Browsing uses metadata; execution must continue to reject tampered package bytes.
        await File.AppendAllTextAsync(Path.Combine(package.DirectoryPath, "adapter.dll"), "tampered");
        var snapshot = await store.ReadEngineLibraryAsync([], Data);
        var family = Assert.Single(snapshot.Families);
        Assert.False(family.Enabled);
        Assert.True(Assert.Single(family.Versions).Exists);
        Assert.Empty(snapshot.Diagnostics);
        Assert.Throws<GeneratedAdapterException>(() => store.LoadActive(GameId));
    }

    [Fact]
    public async Task PublishedVersionsRemainCandidatesWithoutProjectAndRollbackExcludesWithdrawnLegacyPin()
    {
        var store = Store;
        await PublishAsync("one");
        await PublishAsync("two");
        Assert.Equal(2, store.ListReusablePackages().Count);
        Assert.Equal("one", store.LoadVersion(Family.AdapterId, "one").Version);
        // Another legacy activation must not put the withdrawn version back into the global candidates.
        await store.PublishAndActivateAsync("other", store.LoadActive(GameId)!, []);
        Assert.True(await store.RollbackAsync(GameId));
        Assert.Equal("two", store.LoadActive("other")!.Version);
        Assert.Equal("one", Assert.Single(store.ListReusablePackages()).Version);
        var indexed = Assert.Single((await store.ReadEngineLibraryAsync([], Data)).Families);
        Assert.True(indexed.Versions.Single(version => version.Version == "one").IsCandidate);
        Assert.False(indexed.Versions.Single(version => version.Version == "two").IsCandidate);
        Directory.Delete(Path.Combine(store.RootDirectory, "games"), recursive: true);
        Assert.Equal("one", Assert.Single(new LocalAdapterPackageStore(store.RootDirectory).ListReusablePackages()).Version);
    }

    [Theory]
    [InlineData("game.json", EngineLibraryReferenceKind.Project)]
    [InlineData("game.json.bak", EngineLibraryReferenceKind.Project)]
    [InlineData("source.json", EngineLibraryReferenceKind.Source)]
    [InlineData("source.json.bak", EngineLibraryReferenceKind.Source)]
    [InlineData("job.json", EngineLibraryReferenceKind.Job)]
    [InlineData("tasks/saved/job.json", EngineLibraryReferenceKind.Job)]
    [InlineData("extraction/cache/manifest.json", EngineLibraryReferenceKind.Extraction)]
    public async Task DeletionChecksEveryPersistedReference(string relativePath, EngineLibraryReferenceKind kind)
    {
        var store = Store;
        var unused = await PublishAsync("one");
        await PublishAsync("two");
        await PublishAsync("three");
        var file = Path.Combine(Data, "games", GameId, relativePath.Replace('/', Path.DirectorySeparatorChar));
        await LocalFiles.WriteJsonAsync(file, new { id = GameId, adapterId = Family.AdapterId, adapterVersion = "one" });
        var blocked = await store.DeleteVersionAsync(Family.AdapterId, "one", Data);
        Assert.False(blocked.Deleted);
        Assert.Equal(kind, Assert.Single(blocked.References).Kind);
        Assert.True(Directory.Exists(unused.DirectoryPath));
        File.Delete(file);
        await store.RecordCompatibilityAsync(Family, new(GameId, "Old game", "fixture", "one",
            new Dictionary<string, string>(), "passed"));
        var deleted = await store.DeleteVersionAsync(Family.AdapterId, "one", Data);
        Assert.True(deleted.Deleted, string.Join('\n', deleted.Diagnostics));
        Assert.False(Directory.Exists(unused.DirectoryPath));
        var retained = Assert.Single((await store.ReadEngineLibraryAsync([], Data)).Families).Versions.Single(item => item.Version == "one");
        Assert.False(retained.Exists);
        Assert.False(retained.IsCandidate);
        Assert.Single(retained.Games);
        Assert.Single(store.ReadCatalog([]));
        Assert.True(Directory.Exists(Path.Combine(Data, "games", GameId)));
    }

    [Fact]
    public async Task ExplicitProjectDeletionReleasesPinsAndRetainsLegacyPublishedCapability()
    {
        var store = Store;
        await PublishAsync("one");
        await PublishAsync("two");
        File.Delete(Path.Combine(store.RootDirectory, "library-state.json"));
        await store.ForgetGameAsync(GameId);
        Assert.Null(store.LoadActive(GameId));
        Assert.Equal("two", Assert.Single(new LocalAdapterPackageStore(store.RootDirectory).ListReusablePackages()).Version);
        var deleted = await store.DeleteVersionAsync(Family.AdapterId, "one", Data);
        Assert.True(deleted.Deleted, string.Join('\n', deleted.Diagnostics));
        Assert.Equal("two", Assert.Single(store.ListReusablePackages()).Version);
    }

    [Fact]
    public async Task DamagedReferenceAndRunningSessionPreventDeletion()
    {
        var store = Store;
        var unused = await PublishAsync("one");
        await PublishAsync("two");
        await PublishAsync("three");
        var file = Path.Combine(Data, "games", GameId, "source.json");
        Directory.CreateDirectory(Path.GetDirectoryName(file)!);
        await File.WriteAllTextAsync(file, "{\"adapterId\":");
        var damaged = await store.DeleteVersionAsync(Family.AdapterId, "one", Data);
        Assert.False(damaged.Deleted);
        Assert.NotEmpty(damaged.Diagnostics);
        File.Delete(file);
        var running = new EngineLibraryReference(Family.AdapterId, "one", GameId, "Running game", EngineLibraryReferenceKind.Running, null);
        var busy = await store.DeleteVersionAsync(Family.AdapterId, "one", Data, [running]);
        Assert.False(busy.Deleted);
        Assert.Equal(running, Assert.Single(busy.References));
        Assert.True(Directory.Exists(unused.DirectoryPath));
    }

    [Fact]
    public async Task BrowsingUsesIndexAndMissingFilesRemainVisibleAfterSelectedInspection()
    {
        var store = Store;
        var package = await PublishAsync("one");
        await store.RecordCompatibilityAsync(Family, new(GameId, "Indexed game", "fixture", "one",
            new Dictionary<string, string> { ["script.dat"] = "original-resource-hash" }, "passed"));
        var indexPath = Path.Combine(store.RootDirectory, "library-index.json");
        var contents = await File.ReadAllTextAsync(indexPath);
        Assert.DoesNotContain("original-resource-hash", contents);
        Assert.DoesNotContain("sourceHashes", contents);
        Assert.DoesNotContain("assemblyHash", contents);

        // Neither disappeared registry files nor damaged project snapshots are opened by list reads.
        Directory.Delete(package.DirectoryPath, recursive: true);
        Directory.Delete(Path.Combine(store.RootDirectory, "catalog"), recursive: true);
        var project = Path.Combine(Data, "games", GameId);
        Directory.CreateDirectory(project);
        await File.WriteAllTextAsync(Path.Combine(project, "source.json"), "{damaged");
        var listed = await new LocalAdapterPackageStore(store.RootDirectory).ReadEngineLibraryAsync([], Data);
        Assert.Empty(listed.Diagnostics);
        var version = Assert.Single(Assert.Single(listed.Families).Versions);
        Assert.True(version.Exists); // Last observation; only an explicit selection checks files.
        Assert.False(version.ReferencesKnown);
        Assert.Empty(version.References);
        Assert.Single(version.Games);
        var inspected = await store.InspectLibraryVersionAsync(version);
        Assert.False(inspected.Exists);
        var refreshed = await new LocalAdapterPackageStore(store.RootDirectory).ReadEngineLibraryAsync([], Data);
        var retained = Assert.Single(Assert.Single(refreshed.Families).Versions);
        Assert.False((await store.InspectLibraryVersionAsync(retained)).Exists);
        Assert.Single(Assert.Single(refreshed.Families).Games);
        Assert.Equal(contents, await File.ReadAllTextAsync(indexPath)); // File checks are read-only; no index rewrite per click.
    }

    [Theory]
    [InlineData(null)]
    [InlineData("{\"families\":null}")]
    [InlineData("{}")]
    [InlineData("{\"schemaVersion\":1}")]
    [InlineData("{invalid")]
    public async Task MissingOrDamagedIndexRebuildsOnlyRegistryMetadata(string? damagedContents)
    {
        var store = Store;
        var package = await PublishAsync("one");
        await store.RecordCompatibilityAsync(Family, new(GameId, "Recovered game", "fixture", "one",
            new Dictionary<string, string>(), "passed"));
        await store.SetFamilyEnabledAsync(Family.AdapterId, false);
        var indexPath = Path.Combine(store.RootDirectory, "library-index.json");
        if (damagedContents is not null) await File.WriteAllTextAsync(indexPath, damagedContents);
        else File.Delete(indexPath);
        File.Delete(Path.Combine(package.DirectoryPath, "adapter.dll"));
        var project = Path.Combine(Data, "games", GameId, "extraction", "large");
        Directory.CreateDirectory(project);
        await File.WriteAllTextAsync(Path.Combine(project, "manifest.json"), "{damaged");
        await File.WriteAllTextAsync(Directory.GetFiles(Path.Combine(store.RootDirectory, "games"), "active.json", SearchOption.AllDirectories).Single(), "{damaged");

        var rebuilt = await store.ReadEngineLibraryAsync([], Data);
        Assert.Empty(rebuilt.Diagnostics);
        var family = Assert.Single(rebuilt.Families);
        Assert.False(family.Enabled);
        Assert.Single(family.Games);
        var version = Assert.Single(family.Versions);
        Assert.False((await store.InspectLibraryVersionAsync(version)).Exists);
        Assert.True(File.Exists(indexPath));
    }

    [Fact]
    public async Task BuiltInDisplayTracksApplicationCatalogWithoutPackageScans()
    {
        var store = Store;
        await store.SetFamilyEnabledAsync(Family.AdapterId, false);
        var before = await store.ReadEngineLibraryAsync([new(Family, "old", [])], Data);
        Assert.Equal("old", Assert.Single(Assert.Single(before.Families).Versions).Version);
        var after = await store.ReadEngineLibraryAsync([new(Family, "new", [])], Data);
        var version = Assert.Single(Assert.Single(after.Families).Versions);
        Assert.Equal("new", version.Version);
        Assert.False(version.Enabled);
        Assert.True(version.Exists);
        await store.SetFamilyEnabledAsync(Family.AdapterId, true);
        Assert.True(Assert.Single((await store.ReadEngineLibraryAsync([new(Family, "new", [])], Data)).Families).Enabled);
    }

    [Fact]
    public async Task CompatibilityVersionsSurviveIndexRebuildForIdenticalGameResources()
    {
        var store = Store;
        var hashes = new Dictionary<string, string> { ["script.dat"] = "same-original-resource" };
        await store.RecordCompatibilityAsync(Family, new(GameId, "Game", "fixture", "one", hashes, "passed"));
        await store.RecordCompatibilityAsync(Family, new(GameId, "Game", "fixture", "two", hashes, "passed"));
        var indexed = Assert.Single((await store.ReadEngineLibraryAsync([], Data)).Families);
        Assert.Equal(2, indexed.Games.Count);
        File.Delete(Path.Combine(store.RootDirectory, "library-index.json"));
        var rebuilt = Assert.Single((await store.ReadEngineLibraryAsync([], Data)).Families);
        Assert.Equal(indexed.Games.Select(game => game.AdapterVersion).Order(), rebuilt.Games.Select(game => game.AdapterVersion).Order());
    }

    [Fact]
    public async Task DiscoveryIndexDoesNotOpenPayloadsAndRegistryChangesApplyWithoutRebuilding()
    {
        var store = Store;
        var first = await PublishAsync("one");
        await PublishAsync("two");
        var indexed = await store.ReadDiscoveryCandidatesAsync();
        Assert.Equal(2, indexed.Count);
        await store.RollbackAsync(GameId);
        File.Delete(Path.Combine(first.DirectoryPath, "adapter.dll"));
        File.Delete(Path.Combine(first.DirectoryPath, "package.json"));
        Assert.Single(await new LocalAdapterPackageStore(store.RootDirectory).ReadDiscoveryCandidatesAsync());
        Assert.ThrowsAny<Exception>(() => store.LoadVersion(Family.AdapterId, "one"));
        Assert.Equal("one", Assert.Single(await store.ReadDiscoveryCandidatesAsync()).Version);
        await store.SetFamilyEnabledAsync(Family.AdapterId, false);
        Assert.Empty(await store.ReadDiscoveryCandidatesAsync());
        await store.SetFamilyEnabledAsync(Family.AdapterId, true);
        Assert.Equal("one", Assert.Single(await store.ReadDiscoveryCandidatesAsync()).Version);
    }

    [Fact]
    public async Task OldIndexAndMissingIndexRecoverDiscoveryMetadataWithoutHashingPayloads()
    {
        var store = Store;
        var package = await PublishAsync("one");
        var indexPath = Path.Combine(store.RootDirectory, "library-index.json");
        var old = System.Text.Json.Nodes.JsonNode.Parse(await File.ReadAllTextAsync(indexPath))!;
        old.AsObject().Remove("discoveryInitialized");
        var version = old["families"]![Family.AdapterId]!["versions"]!["one"]!.AsObject();
        version.Remove("discovery"); version.Remove("discoveryLoaded");
        await File.WriteAllTextAsync(indexPath, old.ToJsonString());
        File.Delete(Path.Combine(package.DirectoryPath, "adapter.dll"));
        var recovered = Assert.Single(await store.ReadDiscoveryCandidatesAsync());
        Assert.Equal("script.dat", Assert.Single(recovered.Descriptor!.ResourceFiles));
        File.Delete(indexPath);
        var rebuilt = Assert.Single(await store.ReadDiscoveryCandidatesAsync());
        Assert.Equal(recovered.Descriptor.ResourceFiles, rebuilt.Descriptor!.ResourceFiles);
    }

    private async Task<GeneratedAdapterPackage> PublishAsync(string version)
    {
        var directory = Path.Combine(root, "build", version);
        Directory.CreateDirectory(directory);
        await File.WriteAllTextAsync(Path.Combine(directory, "Program.cs"), "// fixture");
        await File.WriteAllTextAsync(Path.Combine(directory, "adapter.dll"), "fixture-assembly");
        await File.WriteAllTextAsync(Path.Combine(directory, "adapter.runtimeconfig.json"), "{}");
        var package = new GeneratedAdapterPackage(Family.AdapterId, version, directory, ["script.dat"],
            await LocalFiles.HashFileAsync(Path.Combine(directory, "Program.cs")),
            await LocalFiles.HashFileAsync(Path.Combine(directory, "adapter.dll")), Family);
        return await Store.PublishAndActivateAsync(GameId, package, []);
    }

    public void Dispose()
    {
        if (Directory.Exists(root)) Directory.Delete(root, recursive: true);
    }
}
