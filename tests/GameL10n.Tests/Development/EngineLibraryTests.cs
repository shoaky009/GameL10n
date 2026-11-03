using GameL10n.Core.Adapters;
using GameL10n.Core.Development;
using GameL10n.Core.Storage;

namespace GameL10n.Tests.Development;

public sealed class EngineLibraryTests : IDisposable
{
    private readonly string root = Path.Combine(Path.GetTempPath(), "gamel10n-engine-library-" + Guid.NewGuid().ToString("N"));
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

    [Fact]
    public async Task DeletionRetainsProjectDataAndCompatibilityDespitePersistedOrDamagedReferences()
    {
        var store = Store;
        var package = await PublishAsync("one");
        await PublishAsync("two");
        await PublishAsync("three");
        var project = Path.Combine(Data, "games", GameId);
        foreach (var relativePath in new[] { "game.json", "game.json.bak", "source.json", "source.json.bak",
            "job.json", "job.json.bak", "tasks/saved/job.json", "extraction/cache/manifest.json" })
        {
            var file = Path.Combine(project, relativePath.Replace('/', Path.DirectorySeparatorChar));
            await LocalFiles.WriteJsonAsync(file, new { id = GameId, adapterId = Family.AdapterId, adapterVersion = "one" });
        }
        await File.WriteAllTextAsync(Path.Combine(project, "source.json.bak"), "{damaged");
        await File.WriteAllTextAsync(Path.Combine(project, "extraction", "cache", "units.jsonl"), "original text");
        await File.WriteAllTextAsync(Path.Combine(project, "translations.json"), "translated text");
        Directory.CreateDirectory(Path.Combine(project, "patches", "final"));
        await File.WriteAllTextAsync(Path.Combine(project, "patches", "final", "script.dat"), "finished patch");
        await store.RecordCompatibilityAsync(Family, new(GameId, "Old game", "fixture", "one",
            "passed"));
        var retainedFiles = new[] { project, Path.Combine(store.RootDirectory, "games"), Path.Combine(store.RootDirectory, "catalog") }
            .SelectMany(directory => Directory.GetFiles(directory, "*", SearchOption.AllDirectories))
            .ToDictionary(file => file, File.ReadAllBytes);

        var deleted = await store.DeleteVersionAsync(Family.AdapterId, "one");
        Assert.True(deleted.Deleted, string.Join('\n', deleted.Diagnostics));
        Assert.Empty(deleted.References);
        Assert.Empty(deleted.Diagnostics);
        Assert.False(Directory.Exists(package.DirectoryPath));
        foreach (var (file, contents) in retainedFiles) Assert.Equal(contents, await File.ReadAllBytesAsync(file));
        var retained = Assert.Single((await store.ReadEngineLibraryAsync([], Data)).Families).Versions.Single(item => item.Version == "one");
        Assert.False(retained.Exists);
        Assert.False(retained.IsCandidate);
        Assert.Single(retained.Games);
        Assert.Single(store.ReadCatalog([]));
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
        var deleted = await store.DeleteVersionAsync(Family.AdapterId, "one");
        Assert.True(deleted.Deleted, string.Join('\n', deleted.Diagnostics));
        Assert.Equal("two", Assert.Single(store.ListReusablePackages()).Version);
    }

    [Fact]
    public async Task OnlyRunningTasksUsingTheSelectedVersionPreventDeletion()
    {
        var store = Store;
        var package = await PublishAsync("one");
        await PublishAsync("two");
        var running = new EngineLibraryReference(Family.AdapterId, "one", GameId, "Running game", EngineLibraryReferenceKind.Running, null);
        var otherReferences = Enum.GetValues<EngineLibraryReferenceKind>()
            .Where(kind => kind != EngineLibraryReferenceKind.Running)
            .Select(kind => running with { Kind = kind })
            .Append(running with { Version = "two" })
            .Append(running with { AdapterId = "engine.other" }).ToArray();
        var busy = await store.DeleteVersionAsync(Family.AdapterId, "one", [running, .. otherReferences]);
        Assert.False(busy.Deleted);
        Assert.Equal(running, Assert.Single(busy.References));
        Assert.True(Directory.Exists(package.DirectoryPath));
        var deleted = await store.DeleteVersionAsync(Family.AdapterId, "one", otherReferences);
        Assert.True(deleted.Deleted, string.Join('\n', deleted.Diagnostics));
        Assert.False(Directory.Exists(package.DirectoryPath));
    }

    [Fact]
    public async Task ActiveAndPreviousVersionsCanBeDeletedWithoutRevivingDiscoveryCandidates()
    {
        var store = Store;
        await PublishAsync("one");
        await PublishAsync("two");
        var pins = Directory.GetFiles(Path.Combine(store.RootDirectory, "games"), "*.json", SearchOption.AllDirectories)
            .ToDictionary(file => file, File.ReadAllBytes);
        Assert.Equal(2, (await store.ReadDiscoveryCandidatesAsync()).Count);

        Assert.True((await store.DeleteVersionAsync(Family.AdapterId, "one")).Deleted);
        Assert.Equal("two", store.LoadActive(GameId)!.Version);
        Assert.Equal("two", Assert.Single(await store.ReadDiscoveryCandidatesAsync()).Version);
        Assert.Equal("two", Assert.Single(store.ListReusablePackages()).Version);
        Assert.True((await store.DeleteVersionAsync(Family.AdapterId, "two")).Deleted);
        Assert.Null(store.LoadActive(GameId));
        Assert.Contains("包目录不存在", Assert.Throws<GeneratedAdapterException>(() => store.LoadVersion(Family.AdapterId, "two")).Message);
        foreach (var (file, contents) in pins) Assert.Equal(contents, await File.ReadAllBytesAsync(file));
        Assert.Empty(await store.ReadDiscoveryCandidatesAsync());
        Assert.Empty(store.ListReusablePackages());

        File.Delete(Path.Combine(store.RootDirectory, "library-index.json"));
        var reopened = new LocalAdapterPackageStore(store.RootDirectory);
        Assert.Empty(await reopened.ReadDiscoveryCandidatesAsync());
        Assert.Empty(reopened.ListReusablePackages());
        Assert.Null(reopened.LoadActive(GameId));
    }

    [Theory]
    [InlineData(null, null)]
    [InlineData("adapter.dll", null)]
    [InlineData("package.json", null)]
    [InlineData("adapter.runtimeconfig.json", null)]
    [InlineData("package.json", "{damaged")]
    public async Task MissingPackagesOrDamagedFilesCanBeRemovedIdempotently(string? fileName, string? damagedContents)
    {
        var store = Store;
        var package = await PublishAsync("one");
        if (fileName is null) Directory.Delete(package.DirectoryPath, recursive: true);
        else if (damagedContents is null) File.Delete(Path.Combine(package.DirectoryPath, fileName));
        else await File.WriteAllTextAsync(Path.Combine(package.DirectoryPath, fileName), damagedContents);
        if (fileName is null) Assert.Null(store.LoadActive(GameId));

        for (var attempt = 0; attempt < 2; attempt++)
        {
            var deleted = await store.DeleteVersionAsync(Family.AdapterId, "one");
            Assert.True(deleted.Deleted, string.Join('\n', deleted.Diagnostics));
            Assert.Empty(deleted.Diagnostics);
            Assert.False(Directory.Exists(package.DirectoryPath));
            var retained = Assert.Single(Assert.Single((await store.ReadEngineLibraryAsync([], Data)).Families).Versions);
            Assert.False(retained.Exists);
            Assert.False(retained.IsCandidate);
        }
        Assert.Null(store.LoadActive(GameId));
    }

    [Fact]
    public async Task BrowsingUsesIndexAndMissingFilesRemainVisibleAfterSelectedInspection()
    {
        var store = Store;
        var package = await PublishAsync("one");
        await store.RecordCompatibilityAsync(Family, new(GameId, "Indexed game", "fixture", "one",
            "passed"));
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
            "passed"));
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
    public async Task CompatibilityUpdatesKeepOnlyTheLatestEntryPerGameAcrossIndexRebuilds()
    {
        var store = Store;
        await store.RecordCompatibilityAsync(Family, new(GameId, "Game", "识别", "one", "not-tested"));
        await store.RecordCompatibilityAsync(Family, new(GameId, "Game", "试译", "one", "passed"));
        await store.RecordCompatibilityAsync(Family, new(GameId, "Game", "识别", "two", "not-tested"));
        await store.RecordCompatibilityAsync(Family, new(GameId, "Game", "试译", "two", "passed"));
        var before = DateTimeOffset.UtcNow;
        await store.RecordCompatibilityAsync(Family, new(GameId, "Game", "最新说明", "two", "failed", "not-tested",
            DateTimeOffset.MinValue));
        await store.RecordCompatibilityAsync(Family, new("different-game", "Game", "同名游戏", "one", "passed"));
        var catalogDirectory = Path.Combine(store.RootDirectory, "catalog", Family.AdapterId);
        Assert.Equal("games.json", Path.GetFileName(Assert.Single(Directory.GetFiles(catalogDirectory))));
        var catalog = store.ReadCatalog([]).SelectMany(entry => entry.Games).ToArray();
        Assert.Equal(2, catalog.Length);
        var latest = Assert.Single(catalog, game => game.GameId == GameId);
        Assert.Equal("two", latest.AdapterVersion);
        Assert.Equal("最新说明", latest.Variant);
        Assert.Equal("failed", latest.StructureStatus);
        Assert.True(latest.UpdatedAt >= before);
        var indexed = Assert.Single((await store.ReadEngineLibraryAsync([], Data)).Families);
        Assert.Equal(2, indexed.Games.Count);
        Assert.Equal(latest, Assert.Single(indexed.Games, game => game.GameId == GameId));
        File.Delete(Path.Combine(store.RootDirectory, "library-index.json"));
        var rebuilt = Assert.Single((await store.ReadEngineLibraryAsync([], Data)).Families);
        Assert.Equal(indexed.Games.OrderBy(game => game.GameId), rebuilt.Games.OrderBy(game => game.GameId));
    }

    [Fact]
    public async Task LegacyCompatibilityUsesFileTimestampsAndMergesIntoOneCatalogOnUpdate()
    {
        var store = Store;
        var epoch = DateTimeOffset.UtcNow.AddDays(-10);
        var legacy = new[]
        {
            await WriteLegacyCompatibilityAsync("ffff-recognition.json", new(GameId, "Game", "识别", "one", "not-tested"), epoch),
            await WriteLegacyCompatibilityAsync("eeee-trial.json", new(GameId, "Game", "试译", "one", "passed"), epoch.AddHours(1)),
            await WriteLegacyCompatibilityAsync("dddd-recognition.json", new(GameId, "Game", "识别", "two", "not-tested"), epoch.AddHours(3)),
            await WriteLegacyCompatibilityAsync("aaaa-trial.json", new(GameId, "Game", "试译", "two", "passed"), epoch.AddHours(2)),
            await WriteLegacyCompatibilityAsync("bbbb-same-title.json", new("another-id", "Game", "独立游戏", "one", "passed"), epoch)
        };
        var diagnostics = new List<string>();
        var read = store.ReadCatalog([], diagnostics: diagnostics).SelectMany(entry => entry.Games).ToArray();
        Assert.Empty(diagnostics);
        Assert.Equal(2, read.Length);
        var latest = Assert.Single(read, game => game.GameId == GameId);
        Assert.Equal("two", latest.AdapterVersion);
        Assert.Equal("识别", latest.Variant); // The newest observation can precede trial translation.
        Assert.Equal("not-tested", latest.StructureStatus);
        Assert.Equal(epoch.AddHours(3), latest.UpdatedAt);
        Assert.All(legacy, file => Assert.True(File.Exists(file)));
        var rebuiltLegacy = Assert.Single((await store.ReadEngineLibraryAsync([], Data)).Families);
        Assert.Equal(2, rebuiltLegacy.Games.Count);
        Assert.Equal(latest, Assert.Single(rebuiltLegacy.Games, game => game.GameId == GameId));

        await store.RecordCompatibilityAsync(Family, new("third-id", "Third game", "新记录", "three", "passed"));
        var directory = Path.GetDirectoryName(legacy[0])!;
        var catalogPath = Assert.Single(Directory.GetFiles(directory));
        Assert.Equal("games.json", Path.GetFileName(catalogPath));
        var contents = await File.ReadAllTextAsync(catalogPath);
        Assert.DoesNotContain("sourceHashes", contents);
        Assert.DoesNotContain("original-resource-hash", contents);
        Assert.DoesNotContain("patchDirectory", contents);
        Assert.DoesNotContain("old-patch-path", contents);
        Assert.DoesNotContain("\"game\":", contents);
        Assert.Equal(3, store.ReadCatalog([]).SelectMany(entry => entry.Games).Count());
        Assert.Equal(latest, Assert.Single(store.ReadCatalog([]).SelectMany(entry => entry.Games), game => game.GameId == GameId));
        Assert.Equal(3, Assert.Single((await store.ReadEngineLibraryAsync([], Data)).Families).Games.Count);
        File.Delete(Path.Combine(store.RootDirectory, "library-index.json"));
        var rebuilt = await store.ReadEngineLibraryAsync([], Data);
        Assert.Empty(rebuilt.Diagnostics);
        Assert.Equal(3, Assert.Single(rebuilt.Families).Games.Count);
    }

    [Fact]
    public async Task LegacyExplicitTimestampTakesPrecedenceOverFileTimestamp()
    {
        var store = Store;
        var epoch = DateTimeOffset.UtcNow.AddDays(-10);
        await WriteLegacyCompatibilityAsync("older-file.json", new(GameId, "Game", "explicit", "one", "failed",
            UpdatedAt: epoch.AddDays(2)), epoch);
        await WriteLegacyCompatibilityAsync("newer-file.json", new(GameId, "Game", "implicit", "two", "passed"), epoch.AddDays(1));
        var latest = Assert.Single(Assert.Single(store.ReadCatalog([])).Games);
        Assert.Equal("explicit", latest.Variant);
        await store.RecordCompatibilityAsync(Family, new("other", "Other", "", "one", "passed"));
        Assert.Equal(latest, Assert.Single(store.ReadCatalog([]).SelectMany(entry => entry.Games), game => game.GameId == GameId));
    }

    [Fact]
    public async Task CatalogAndOldIndexPreferTheLatestTimestampThenTheLastEqualTimestamp()
    {
        var store = Store;
        var epoch = DateTimeOffset.UtcNow.AddDays(-1);
        var first = new GameCompatibility(GameId, "Game", "first", "one", "passed", UpdatedAt: epoch);
        var older = first with { Variant = "older", AdapterVersion = "two", UpdatedAt = epoch.AddHours(-1) };
        var last = first with { Variant = "last", AdapterVersion = "three", StructureStatus = "failed" };
        await LocalFiles.WriteJsonAsync(Path.Combine(store.RootDirectory, "catalog", Family.AdapterId, "games.json"),
            new { family = Family, games = new[] { first, older, last } });
        Assert.Equal(last, Assert.Single(Assert.Single(store.ReadCatalog([])).Games));
        var family = Assert.Single((await store.ReadEngineLibraryAsync([], Data)).Families);
        Assert.Equal(last, Assert.Single(family.Games));

        var indexPath = Path.Combine(store.RootDirectory, "library-index.json");
        var index = System.Text.Json.Nodes.JsonNode.Parse(await File.ReadAllTextAsync(indexPath))!;
        var games = index["families"]![Family.AdapterId]!["games"]!.AsArray();
        games.Insert(0, System.Text.Json.JsonSerializer.SerializeToNode(older, LocalFiles.JsonOptions));
        var final = last with { Variant = "last-in-old-index" };
        games.Add(System.Text.Json.JsonSerializer.SerializeToNode(final, LocalFiles.JsonOptions));
        await File.WriteAllTextAsync(indexPath, index.ToJsonString());
        var normalized = Assert.Single((await store.ReadEngineLibraryAsync([], Data)).Families);
        Assert.Equal(final, Assert.Single(normalized.Games));
    }

    [Theory]
    [InlineData("{damaged")]
    [InlineData("{}")]
    [InlineData("{\"family\":null,\"games\":[]}")]
    [InlineData("{\"family\":{\"kind\":\"engine\",\"key\":\"other\",\"name\":\"Other\"},\"games\":[]}")]
    [InlineData("{\"family\":{\"kind\":\"engine\",\"key\":\"library-fixture\",\"name\":\"Fixture\"},\"games\":[{}]}")]
    public async Task DamagedCatalogIsReportedAndNeverOverwritten(string contents)
    {
        var store = Store;
        var legacyPath = await WriteLegacyCompatibilityAsync("legacy.json", new(GameId, "Game", "", "one", "passed"), DateTimeOffset.UtcNow);
        var catalogPath = Path.Combine(Path.GetDirectoryName(legacyPath)!, "games.json");
        await File.WriteAllTextAsync(catalogPath, contents);
        var diagnostics = new List<string>();
        Assert.Empty(store.ReadCatalog([], diagnostics: diagnostics));
        Assert.Contains("games.json", Assert.Single(diagnostics));
        var error = await Assert.ThrowsAsync<InvalidDataException>(() =>
            store.RecordCompatibilityAsync(Family, new(GameId, "Game", "new", "two", "passed")));
        Assert.Contains("games.json", error.Message);
        Assert.Equal(contents, await File.ReadAllTextAsync(catalogPath));
        Assert.True(File.Exists(legacyPath));
    }

    [Theory]
    [InlineData("json")]
    [InlineData("family")]
    [InlineData("game")]
    public async Task InvalidLegacyRecordStopsMigrationAndRetainsAllOriginalFiles(string damage)
    {
        var store = Store;
        var valid = await WriteLegacyCompatibilityAsync("valid.json", new(GameId, "Game", "", "one", "passed"), DateTimeOffset.UtcNow);
        var invalid = await WriteLegacyCompatibilityAsync("invalid.json", new("other", "Other", "", "one", "passed"), DateTimeOffset.UtcNow);
        var contents = System.Text.Json.Nodes.JsonNode.Parse(await File.ReadAllTextAsync(invalid))!;
        if (damage == "family") contents["family"]!["key"] = "different";
        else if (damage == "game") contents["game"]!.AsObject().Remove("title");
        await File.WriteAllTextAsync(invalid, damage == "json" ? "{damaged" : contents.ToJsonString());
        var originals = new[] { valid, invalid }.ToDictionary(file => file, File.ReadAllBytes);
        var diagnostics = new List<string>();
        Assert.Single(store.ReadCatalog([], diagnostics: diagnostics));
        Assert.Contains("invalid.json", Assert.Single(diagnostics));
        var error = await Assert.ThrowsAsync<InvalidDataException>(() =>
            store.RecordCompatibilityAsync(Family, new(GameId, "Game", "new", "two", "passed")));
        Assert.Contains("invalid.json", error.Message);
        Assert.False(File.Exists(Path.Combine(Path.GetDirectoryName(valid)!, "games.json")));
        foreach (var (file, original) in originals) Assert.Equal(original, await File.ReadAllBytesAsync(file));
    }

    [Fact]
    public async Task ExistingCatalogUpdatesAndReadsDoNotOpenLeftoverLegacyFiles()
    {
        var store = Store;
        await store.RecordCompatibilityAsync(Family, new(GameId, "Game", "", "one", "passed"));
        var leftover = Path.Combine(store.RootDirectory, "catalog", Family.AdapterId, "leftover.json");
        await File.WriteAllTextAsync(leftover, "{damaged");
        await store.RecordCompatibilityAsync(Family, new(GameId, "Game", "updated", "two", "failed"));
        var diagnostics = new List<string>();
        Assert.Equal("updated", Assert.Single(Assert.Single(store.ReadCatalog([], diagnostics: diagnostics)).Games).Variant);
        Assert.Empty(diagnostics);
        Assert.Equal("{damaged", await File.ReadAllTextAsync(leftover));
        File.Delete(Path.Combine(store.RootDirectory, "library-index.json"));
        var rebuilt = await store.ReadEngineLibraryAsync([], Data);
        Assert.Empty(rebuilt.Diagnostics);
        Assert.Single(Assert.Single(rebuilt.Families).Games);
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

    private async Task<string> WriteLegacyCompatibilityAsync(string fileName, GameCompatibility game, DateTimeOffset modifiedAt)
    {
        var path = Path.Combine(Store.RootDirectory, "catalog", Family.AdapterId, fileName);
        await LocalFiles.WriteJsonAsync(path, new
        {
            family = Family,
            game = new
            {
                game.GameId, game.Title, game.Variant, game.AdapterVersion, game.StructureStatus, game.RuntimeStatus, game.UpdatedAt,
                sourceHashes = new Dictionary<string, string> { ["script.dat"] = "original-resource-hash" },
                patchDirectory = "old-patch-path"
            }
        });
        File.SetLastWriteTimeUtc(path, modifiedAt.UtcDateTime);
        return path;
    }

    public void Dispose()
    {
        if (Directory.Exists(root)) Directory.Delete(root, recursive: true);
    }
}
