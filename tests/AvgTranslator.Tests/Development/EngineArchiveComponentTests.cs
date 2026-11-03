using System.Security.Cryptography;
using System.Text.Json;
using AvgTranslator.Core.Development;
using AvgTranslator.Core.Isolation;

namespace AvgTranslator.Tests.Development;

public sealed class EngineArchiveComponentTests : IDisposable
{
    private readonly EngineArchiveFixture fixture = new();

    [Theory]
    [InlineData("archive.rpa3")]
    [InlineData("archive.xp3")]
    public async Task RealComponentsExtractSelectedScriptsAndPreserveOriginalBytesAndUntouchedMembers(string component)
    {
        var archive = await fixture.CreateArchiveAsync(component);
        var original = await File.ReadAllBytesAsync(archive);
        var inspected = await fixture.InvokeAsync(component, "inspect", archive);
        Assert.True(inspected.GetProperty("success").GetBoolean(), inspected.ToString());
        Assert.Equal(2, inspected.GetProperty("entryCount").GetInt32());
        var extracted = await fixture.InvokeAsync(component, "extract", archive, extensions: [".rpy"]);
        Assert.True(extracted.GetProperty("success").GetBoolean(), extracted.ToString());
        Assert.Single(extracted.GetProperty("extractedFiles").EnumerateArray());
        var script = Path.Combine(fixture.LastOutput, "files", "script.rpy");
        Assert.Contains("こんにちは", await File.ReadAllTextAsync(script));
        Assert.False(File.Exists(Path.Combine(fixture.LastOutput, "files", "images", "logo.bin")));

        var roundtrip = await fixture.InvokeAsync(component, "rebuild", archive);
        Assert.True(roundtrip.GetProperty("success").GetBoolean(), roundtrip.ToString());
        Assert.True(roundtrip.GetProperty("byteIdentical").GetBoolean());
        Assert.Equal(original, await File.ReadAllBytesAsync(Path.Combine(fixture.LastOutput, "rebuilt.bin")));

        await File.WriteAllTextAsync(script, "label start:\n    \"你好，世界。\"\n");
        var patched = await fixture.InvokeAsync(component, "rebuild", archive,
            replacements: new Dictionary<string, string> { ["script.rpy"] = script });
        Assert.True(patched.GetProperty("success").GetBoolean(), patched.ToString());
        Assert.Equal(1, patched.GetProperty("changedCount").GetInt32());
        var patchPath = Path.Combine(fixture.LastOutput, "rebuilt.bin");
        var verified = await fixture.InvokeAsync(component, "extract", patchPath);
        Assert.True(verified.GetProperty("success").GetBoolean(), verified.ToString());
        Assert.Contains("你好", await File.ReadAllTextAsync(Path.Combine(fixture.LastOutput, "files", "script.rpy")));
        Assert.Equal(new byte[] { 0, 1, 255, 10 }, await File.ReadAllBytesAsync(Path.Combine(fixture.LastOutput, "files", "images", "logo.bin")));
        Assert.Equal(original, await File.ReadAllBytesAsync(archive));
        Assert.Empty(Directory.EnumerateDirectories(fixture.Root, ".engine-component-*"));
    }

    [Theory]
    [InlineData("../escape.rpy")]
    [InlineData("CON.txt")]
    [InlineData("scripts/evil:rpy")]
    [InlineData("Script.rpy")]
    [InlineData("script.rpy/child")]
    public async Task UnsafeOrCollidingArchiveNamesAreRejectedBeforeAnyExtraction(string name)
    {
        var archive = await fixture.CreateArchiveAsync("archive.rpa3", name);
        var response = await fixture.InvokeAsync("archive.rpa3", "extract", archive);
        Assert.False(response.GetProperty("success").GetBoolean());
        Assert.False(Directory.Exists(Path.Combine(fixture.LastOutput, "files")));
        Assert.False(File.Exists(Path.Combine(fixture.Root, "escape.rpy")));
    }

    [Fact]
    public async Task MissingUnknownOrOversizedReplacementDoesNotProduceAnArchive()
    {
        var archive = await fixture.CreateArchiveAsync("archive.rpa3");
        var response = await fixture.InvokeAsync("archive.rpa3", "rebuild", archive,
            replacements: new Dictionary<string, string> { ["missing.rpy"] = archive });
        Assert.False(response.GetProperty("success").GetBoolean());
        Assert.Contains("unknown replacement", response.GetProperty("error").GetString());
        Assert.False(File.Exists(Path.Combine(fixture.LastOutput, "rebuilt.bin")));
        var large = Path.Combine(fixture.Root, "large");
        await using (var file = File.Create(large)) file.SetLength(64L * 1024 * 1024 + 1);
        response = await fixture.InvokeAsync("archive.rpa3", "rebuild", archive,
            replacements: new Dictionary<string, string> { ["script.rpy"] = large });
        Assert.False(response.GetProperty("success").GetBoolean());
        Assert.Contains("size limit", response.GetProperty("error").GetString());
        Assert.False(File.Exists(Path.Combine(fixture.LastOutput, "rebuilt.bin")));
    }

    [Fact]
    public async Task Xp3RequiresExplicitFilterAndAlteredChecksumsFail()
    {
        var archive = await fixture.CreateArchiveAsync("archive.xp3");
        var client = new EngineArchiveComponentClient(new NeverRunner());
        await Assert.ThrowsAsync<InvalidDataException>(() => client.InvokeAsync(fixture.Paths,
            new(1, "archive.xp3", "inspect", archive, Path.Combine(fixture.Root, "response.json")), fixture.Root));
        var bytes = await File.ReadAllBytesAsync(archive);
        bytes[20] ^= 1; // First uncompressed payload byte.
        await File.WriteAllBytesAsync(archive, bytes);
        var response = await fixture.InvokeAsync("archive.xp3", "inspect", archive);
        Assert.False(response.GetProperty("success").GetBoolean());
        Assert.Contains("Adler-32", response.GetProperty("error").GetString());
    }

    [Fact]
    public async Task HostRejectsGamePathsAndSourceOutputsWithoutRunningOrDownloading()
    {
        var workspace = Directory.CreateDirectory(Path.Combine(fixture.Root, "project")).FullName;
        var resources = Directory.CreateDirectory(Path.Combine(workspace, "resources")).FullName;
        await File.WriteAllTextAsync(Path.Combine(resources, "a.rpa"), "header");
        var tools = new AdaptationTools(new("game", fixture.Root, []), new(), workspace,
            new(new(fixture.Root, "10.0.402", "10.0.13"), new NeverRunner()), new(Path.Combine(fixture.Root, "store")));
        var inspection = await tools.ExecuteAsync(new("inspect", "inspect_engine_component", "{}"), default);
        Assert.Contains("archive.rpa3", inspection);
        foreach (var args in new[]
        {
            """{"id":"archive.rpa3","operation":"extract","archive":"../game/a.rpa","output":"work/out"}""",
            """{"id":"archive.rpa3","operation":"extract","archive":"resources/a.rpa","output":"src/out"}""",
            """{"id":"archive.xp3","operation":"extract","archive":"resources/a.rpa","output":"work/out"}"""
        })
            await Assert.ThrowsAsync<InvalidDataException>(() => tools.ExecuteAsync(new("run", "run_engine_component", args), default));
        Assert.False(Directory.Exists(Path.Combine(workspace, "work")));
    }

    [Fact]
    public async Task HostRunsFixedEntryPointOfflineAndReturnsCompactEvidenceWithoutChangingSourceSelection()
    {
        var workspace = Directory.CreateDirectory(Path.Combine(fixture.Root, "project")).FullName;
        var resources = Directory.CreateDirectory(Path.Combine(workspace, "resources")).FullName;
        File.Copy(await fixture.CreateArchiveAsync("archive.rpa3"), Path.Combine(resources, "scripts.rpa"));
        var store = new LocalAdapterPackageStore(Path.Combine(fixture.Root, "store"));
        await fixture.PrepareSharedToolsAsync(store.RootDirectory);
        var state = new AdaptationSession();
        var tools = new AdaptationTools(new("game", fixture.Root, []), state, workspace,
            new(new(fixture.Root, "10.0.402", "10.0.13"), fixture.Runner), store);
        using var response = JsonDocument.Parse(await tools.ExecuteAsync(new("run", "run_engine_component",
            """{"id":"archive.rpa3","operation":"extract","archive":"resources/scripts.rpa","output":"work/extracted","extensions":[".rpy"]}"""), default));
        var result = response.RootElement.GetProperty("result");
        Assert.True(result.GetProperty("success").GetBoolean(), response.RootElement.ToString());
        Assert.Equal(2, result.GetProperty("entryCount").GetInt32());
        Assert.Equal(1, result.GetProperty("selectedCount").GetInt32());
        Assert.Equal("work/extracted/manifest.json", result.GetProperty("manifest").GetString());
        Assert.True(File.Exists(Path.Combine(workspace, "work", "extracted", "files", "script.rpy")));
        Assert.NotNull(state.EngineReference);
        Assert.Null(state.ReleaseFiles);
        Assert.False(Directory.Exists(Path.Combine(workspace, "src")));
    }

    private sealed class NeverRunner : IAdapterProcessRunner
    {
        public Task<AdapterProcessResult> RunAsync(AdapterProcessRequest request, CancellationToken ct) => throw new Xunit.Sdk.XunitException("The process must not start.");
    }
    public void Dispose() => fixture.Dispose();
}

internal sealed class EngineArchiveFixture : IDisposable
{
    public string Root { get; } = Path.Combine(Path.GetTempPath(), "avg-engine-archive-" + Guid.NewGuid().ToString("N"));
    public string LastOutput { get; private set; } = "";
    public EnginePythonPaths Paths { get; }
    public IAdapterProcessRunner Runner => new TestPythonRunner(Paths.Python);
    public EngineArchiveFixture()
    {
        Directory.CreateDirectory(Root);
        var library = Path.Combine(Root, "library.zip");
        using var source = typeof(EngineArchiveFixture).Assembly.GetManifestResourceStream("EngineArchiveReference.zip")!;
        using (var file = File.Create(library)) source.CopyTo(file);
        Paths = new(FindPython(), library);
    }

    public async Task<EnginePythonDependency> PrepareSharedToolsAsync(string storeRoot)
    {
        const string revision = "e9b801879f37cee98a9dfc8afbbcf5ffa929c04c";
        var shared = Path.Combine(storeRoot, "engine-tools");
        var hash = Convert.ToHexStringLower(SHA256.HashData(await File.ReadAllBytesAsync(Paths.EngineLibrary)));
        var pin = new EngineReferencePin(revision, hash, hash);
        var library = Path.Combine(shared, "references", revision, "library.zip");
        Directory.CreateDirectory(Path.GetDirectoryName(library)!);
        File.Copy(Paths.EngineLibrary, library);
        var json = new JsonSerializerOptions(JsonSerializerDefaults.Web);
        await File.WriteAllTextAsync(Path.Combine(shared, "current.json"), JsonSerializer.Serialize(pin, json));
        var runtime = Directory.CreateDirectory(Path.Combine(shared, "python", EngineReferenceToolkit.PythonVersion)).FullName;
        var files = new Dictionary<string, string>();
        foreach (var name in new[] { "python.exe", "python314.zip" })
        {
            var file = Path.Combine(runtime, name);
            await File.WriteAllTextAsync(file, "Test receipt; the test runner substitutes its explicitly selected test interpreter.");
            files[name] = Convert.ToHexStringLower(SHA256.HashData(await File.ReadAllBytesAsync(file)));
        }
        var pythonHash = new string('a', 64);
        await File.WriteAllTextAsync(Path.Combine(runtime, "runtime.json"), JsonSerializer.Serialize(new { archiveSha256 = pythonHash, files }, json));
        return new(pin, EngineReferenceToolkit.PythonVersion, pythonHash);
    }

    private sealed class TestPythonRunner(string python) : IAdapterProcessRunner
    {
        public Task<AdapterProcessResult> RunAsync(AdapterProcessRequest request, CancellationToken ct) => new AdapterProcessRunner().RunAsync(
            Path.GetFileName(request.ExecutablePath) == "python.exe" ? request with { ExecutablePath = python } : request, ct);
    }

    private static string FindPython()
    {
        var configured = Environment.GetEnvironmentVariable("AVG_TEST_PYTHON");
        if (!string.IsNullOrEmpty(configured) && File.Exists(configured)) return configured;
        var bundled = Path.Combine(AppContext.BaseDirectory, "tools", "engine-tools", "python", EngineReferenceToolkit.PythonVersion, "python.exe");
        if (File.Exists(bundled)) return bundled;
        foreach (var directory in (Environment.GetEnvironmentVariable("PATH") ?? "").Split(Path.PathSeparator))
            foreach (var name in OperatingSystem.IsWindows() ? new[] { "python.exe", "python3.exe" } : ["python3", "python"])
            {
                var path = Path.Combine(directory, name);
                if (File.Exists(path)) return Path.GetFullPath(path);
            }
        throw new InvalidOperationException("Set AVG_TEST_PYTHON to a Python 3.10+ executable for offline archive integration tests.");
    }

    public async Task<string> CreateArchiveAsync(string component, string otherName = "images/logo.bin", string? script = null)
    {
        var archive = Path.Combine(Root, Guid.NewGuid().ToString("N") + (component == "archive.rpa3" ? ".rpa" : ".xp3"));
        var make = Path.Combine(Root, "make.py");
        await File.WriteAllTextAsync(make, """
            import sys, json, pickle, zlib
            from python.archives import xp3
            args = json.load(sys.stdin)
            files = [("script.rpy", args["script"].encode("utf-8")), (args["otherName"], bytes([0, 1, 255, 10]))]
            if args["component"] == "archive.xp3":
                data = xp3.build(files, filter_name="none", compress_index=False)
            else:
                data, mapping, key = bytearray(b"RPA-3.0 " + b"0"*16 + b" 12345678\n"), {}, 0x12345678
                for name, value in files:
                    mapping[name] = [(len(data)^key, len(value)^key, b"")]
                    data.extend(value)
                offset = len(data)
                data[8:24] = f"{offset:016x}".encode("ascii")
                data.extend(zlib.compress(pickle.dumps(mapping, protocol=3)))
            with open(args["archive"], "xb") as f: f.write(data)
            """);
        var result = await new AdapterProcessRunner().RunAsync(new(Paths.Python,
            EngineReferenceToolkit.PythonArguments(Paths, make, []), Root,
            JsonSerializer.Serialize(new { component, archive, otherName, script = script ?? "label start:\n    \"こんにちは。\"\n" })), default);
        Assert.Equal(0, result.ExitCode);
        return archive;
    }

    public async Task<JsonElement> InvokeAsync(string component, string operation, string archive,
        IReadOnlyList<string>? extensions = null, IReadOnlyDictionary<string, string>? replacements = null)
    {
        LastOutput = Path.Combine(Root, Guid.NewGuid().ToString("N"));
        return await new EngineArchiveComponentClient(new AdapterProcessRunner()).InvokeAsync(Paths,
            new(1, component, operation, archive, Path.Combine(LastOutput, "manifest.json"), Path.Combine(LastOutput, "files"),
                Path.Combine(LastOutput, "rebuilt.bin"), component == "archive.xp3" ? "none" : null,
                Extensions: extensions, Replacements: replacements), Root);
    }
    public void Dispose() => GeneratedGameAdapter.DeleteStagedTree(Root);
}
