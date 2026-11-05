using System.Text.Json;
using GameL10n.Core.Storage;

namespace GameL10n.Tests.Pipeline;

public sealed class DataDirectoryTransferTests : IDisposable
{
    private readonly string root = Path.Combine(Path.GetTempPath(), "gamel10n-data-transfer-" + Guid.NewGuid().ToString("N"));
    private string Source => Path.Combine(root, "source");
    private string Target => Path.Combine(root, "target");

    [Fact]
    public async Task RenamedApplicationKeepsLegacyDataUntilANewLocationIsConfigured()
    {
        var current = Path.Combine(root, "GameL10n");
        var legacy = Path.Combine(root, "AvgTranslator");
        Assert.Equal(current, DataLocation.Resolve(current, legacy));
        Directory.CreateDirectory(Path.Combine(current, "logs"));
        await WriteAsync(legacy, "settings.json", "{}");
        Assert.Equal(legacy, DataLocation.Resolve(current, legacy));
        await LocalFiles.WriteJsonAsync(Path.Combine(legacy, "location.json"), new { dataDirectory = Source }, FixtureJson.TypeInfo<object>(FixtureJson.Storage));
        Assert.Equal(Source, DataLocation.Resolve(current, legacy));
        await LocalFiles.WriteJsonAsync(Path.Combine(current, "location.json"), new { dataDirectory = Target }, FixtureJson.TypeInfo<object>(FixtureJson.Storage));
        Assert.Equal(Target, DataLocation.Resolve(current, legacy));
        Assert.Equal(Source, DataLocation.Resolve(current, legacy, Source));
        await WriteAsync(current, "location.json", "invalid-json");
        Assert.Throws<InvalidDataException>(() => DataLocation.Resolve(current, legacy));
    }

    [Fact]
    public async Task CopiesAllDataWithLocksHeldAndRebasesOnlyWorkspacePaths()
    {
        using var sourceLock = DataDirectoryTransfer.AcquireLock(Source);
        using var targetLock = DataDirectoryTransfer.AcquireLock(Target);
        var project = new GameProjectStore(Source).Create(Path.Combine(root, "installed-game"));
        await new GameProjectStore(Source).SaveAsync(project);
        var relative = "games/" + project.Id;
        await WriteAsync(Source, "settings.json", "{\"ciphertext\":\"unchanged\"}");
        await WriteAsync(Source, "location.json", "{\"dataDirectory\":\"old-location\"}");
        await WriteAsync(Source, relative + "/memory/00.json", "cached translations");
        var snapshot = await WriteAsync(Source, relative + "/sources/hash/script.bin", "original snapshot");
        File.SetAttributes(snapshot, FileAttributes.ReadOnly);
        await WriteAsync(Source, relative + "/responses/request.json", "persisted response");
        await WriteAsync(Source, relative + "/logs/operations.log", "log history");
        await WriteAsync(Source, relative + "/.task.lock", "");
        await WriteAsync(Source, relative + "/memory/.memory.lock", "");
        await WriteAsync(Source, "extensions/packages/template/v1/adapter.dll", "package");
        await LocalFiles.WriteJsonAsync(Path.Combine(Source, relative, "source.json"), new { directory = Path.Combine(Source, relative, "sources/hash") }, FixtureJson.TypeInfo<object>(FixtureJson.Storage));
        await LocalFiles.WriteJsonAsync(Path.Combine(Source, relative, "latest-result.json"), new { patchDirectory = Path.Combine(Source, relative, "patches/hash") }, FixtureJson.TypeInfo<object>(FixtureJson.Storage));
        var hashes = Directory.EnumerateFiles(Source, "*", SearchOption.AllDirectories)
            .Where(file => Path.GetFileName(file) != ".app.lock").ToDictionary(file => file, file => LocalFiles.HashFileAsync(file).GetAwaiter().GetResult());

        await DataDirectoryTransfer.CopyAsync(Source, Target);

        foreach (var pair in hashes) Assert.Equal(pair.Value, await LocalFiles.HashFileAsync(pair.Key));
        Assert.Equal("persisted response", await File.ReadAllTextAsync(Path.Combine(Target, relative, "responses/request.json")));
        Assert.Equal("cached translations", await File.ReadAllTextAsync(Path.Combine(Target, relative, "memory/00.json")));
        Assert.Equal("log history", await File.ReadAllTextAsync(Path.Combine(Target, relative, "logs/operations.log")));
        Assert.Equal("package", await File.ReadAllTextAsync(Path.Combine(Target, "extensions/packages/template/v1/adapter.dll")));
        Assert.Equal(hashes[Path.Combine(Source, "settings.json")], await LocalFiles.HashFileAsync(Path.Combine(Target, "settings.json")));
        Assert.False(File.Exists(Path.Combine(Target, "location.json")));
        Assert.False(File.Exists(Path.Combine(Target, relative, ".task.lock")));
        Assert.False(File.Exists(Path.Combine(Target, relative, "memory/.memory.lock")));
        Assert.False(File.Exists(Path.Combine(Target, ".relocation.json")));
        var result = await LocalFiles.ReadJsonAsync<JsonElement>(Path.Combine(Target, relative, "latest-result.json"), FixtureJson.TypeInfo<global::System.Text.Json.JsonElement>(FixtureJson.Storage));
        Assert.Equal(Path.Combine("patches", "hash"), result.GetProperty("patchDirectory").GetString());
        Assert.Equal(project.GameDirectory, (await new GameProjectStore(Target).GetAsync(project.Id))!.GameDirectory);
    }

    [Fact]
    public async Task ExistingDestinationIsNeverOverwritten()
    {
        using var sourceLock = DataDirectoryTransfer.AcquireLock(Source);
        using var targetLock = DataDirectoryTransfer.AcquireLock(Target);
        await WriteAsync(Source, "settings.json", "source");
        var targetFile = await WriteAsync(Target, "settings.json", "target");
        await Assert.ThrowsAsync<IOException>(() => DataDirectoryTransfer.CopyAsync(Source, Target));
        Assert.Equal("target", await File.ReadAllTextAsync(targetFile));
        Assert.False(File.Exists(Path.Combine(Target, ".relocation.json")));
    }

    [Fact]
    public async Task FailedCopyCannotBeOpenedAndCanBeRetriedFromSource()
    {
        using var sourceLock = DataDirectoryTransfer.AcquireLock(Source);
        var file = await WriteAsync(Source, "games/project/blocked.json", "persisted data");
        using (var targetLock = DataDirectoryTransfer.AcquireLock(Target))
        using (var writer = new FileStream(file, FileMode.Open, FileAccess.ReadWrite, FileShare.None))
            await Assert.ThrowsAsync<IOException>(() => DataDirectoryTransfer.CopyAsync(Source, Target));

        Assert.Throws<InvalidDataException>(() => DataDirectoryTransfer.AcquireLock(Target));
        Assert.Equal("persisted data", await File.ReadAllTextAsync(file));
        using (var targetLock = DataDirectoryTransfer.AcquireLock(Target, allowIncompleteTransfer: true))
            await DataDirectoryTransfer.CopyAsync(Source, Target);
        using var opened = DataDirectoryTransfer.AcquireLock(Target);
        Assert.Equal("persisted data", await File.ReadAllTextAsync(Path.Combine(Target, "games/project/blocked.json")));
    }

    [Fact]
    public void RejectsOverlappingDirectoriesAndConcurrentAccess()
    {
        Assert.True(DataDirectoryTransfer.Contains(Path.GetPathRoot(root)!, root));
        Assert.Throws<InvalidOperationException>(() => DataDirectoryTransfer.ValidateDestination(Source, Path.Combine(Source, "nested"), []));
        Assert.Throws<InvalidOperationException>(() => DataDirectoryTransfer.ValidateDestination(Source, root, []));
        Assert.Throws<InvalidOperationException>(() => DataDirectoryTransfer.ValidateDestination(Source, Target, [Path.Combine(Target, "game")]));
        using var targetLock = DataDirectoryTransfer.AcquireLock(Target);
        Assert.Throws<IOException>(() => DataDirectoryTransfer.AcquireLock(Target));
    }

    private static async Task<string> WriteAsync(string root, string relative, string content)
    {
        var path = Path.Combine(root, relative);
        Directory.CreateDirectory(Path.GetDirectoryName(path)!);
        await File.WriteAllTextAsync(path, content);
        return path;
    }

    public void Dispose()
    {
        if (!Directory.Exists(root)) return;
        foreach (var file in Directory.EnumerateFiles(root, "*", SearchOption.AllDirectories))
            File.SetAttributes(file, FileAttributes.Normal);
        Directory.Delete(root, recursive: true);
    }
}
