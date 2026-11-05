using System.IO.Compression;
using System.Net;
using System.Text;
using System.Text.Json;
using GameL10n.Core.Development;

namespace GameL10n.Tests.Development;

public sealed class EngineReferenceToolkitTests : IDisposable
{
    private readonly string root = Path.Combine(Path.GetTempPath(), "gamel10n-engine-reference-" + Guid.NewGuid().ToString("N"));
    private const string Revision = "0123456789012345678901234567890123456789";
    private static readonly JsonSerializerOptions Json = new(JsonSerializerDefaults.Web);

    [Fact]
    public async Task BundledToolsAreReusedOfflineWithoutProjectCopies()
    {
        var downloads = new List<string>();
        using var client = Client(downloads);
        var bundle = Path.Combine(root, "bundle");
        var preparation = new EngineReferenceToolkit(bundle, client, allowDownloads: true);
        var pin = await preparation.EnsureReferenceAsync(null, default);
        var prepared = await preparation.EnsurePythonAsync(pin, null, default);
        int count = downloads.Count;
        var consumerRoot = Path.Combine(root, "consumer");
        var consumer = new EngineReferenceToolkit(consumerRoot, client, bundledDirectory: bundle);
        Assert.Equal(pin, await consumer.EnsureReferenceAsync(null, default));
        Assert.Equal(prepared, await consumer.EnsurePythonAsync(pin, prepared.Dependency, default));
        Assert.Equal(preparation.LibraryPath(pin), consumer.LibraryPath(pin));
        Assert.False(Directory.Exists(consumerRoot));
        Assert.Equal(count, downloads.Count);
    }

    [Fact]
    public async Task MissingBundledToolsDoNotTriggerRuntimeDownloads()
    {
        var downloads = new List<string>();
        using var client = Client(downloads);
        var toolkit = new EngineReferenceToolkit(root, client, bundledDirectory: Path.Combine(root, "missing"));
        var error = await Assert.ThrowsAsync<InvalidDataException>(() => toolkit.EnsureReferenceAsync(null, default));
        Assert.Contains("完整发行包", error.Message);
        Assert.Empty(downloads);
    }

    [Fact]
    public async Task BundledSiglusCoexistsWithReferenceLibraryAndRestoresOnlyMissingNativeModule()
    {
        var downloads = new List<string>();
        using var client = Client(downloads);
        var toolkit = new EngineReferenceToolkit(root, client, allowDownloads: true);
        var reference = await toolkit.EnsureReferenceAsync(null, default);
        var pins = await Task.WhenAll(toolkit.EnsureReferenceAsync(SiglusAssets.Pin, default),
            new EngineReferenceToolkit(root, client, allowDownloads: true).EnsureReferenceAsync(SiglusAssets.Pin, default));
        Assert.All(pins, pin => Assert.Equal(SiglusAssets.Pin, pin));
        Assert.Equal(reference, toolkit.CurrentReference());
        Assert.Equal(2, downloads.Count);
        var native = Path.Combine(Path.GetDirectoryName(toolkit.LibraryPath(pins[0]))!, "native", "siglus_ssu", "native_accel.cp314-win_amd64.pyd");
        var bytes = await File.ReadAllBytesAsync(native);
        File.Delete(native);
        await toolkit.EnsureReferenceAsync(pins[0], default);
        Assert.Equal(bytes, await File.ReadAllBytesAsync(native));
        await File.AppendAllTextAsync(native, "corrupt");
        await Assert.ThrowsAsync<InvalidDataException>(() => toolkit.EnsureReferenceAsync(pins[0], default));
        Assert.Equal(2, downloads.Count);
    }

    [Fact]
    public async Task SharedLibraryIsPinnedFilteredAndRestorableWithoutResolvingMainAgain()
    {
        var downloads = new List<string>();
        using var client = Client(downloads);
        var first = new EngineReferenceToolkit(root, client, allowDownloads: true);
        var pins = await Task.WhenAll(first.EnsureReferenceAsync(null, default), new EngineReferenceToolkit(root, client, allowDownloads: true).EnsureReferenceAsync(null, default));
        Assert.Equal(pins[0], pins[1]);
        Assert.Equal(2, downloads.Count);
        Assert.Equal(Revision, pins[0].Revision);
        Assert.Equal("engine guide", first.Read(pins[0], "engines/fixture.md"));
        var missing = Assert.Throws<InvalidDataException>(() => first.RequireRenpyRpyc(pins[0]));
        Assert.Contains("缺少 RPYC2 模块", missing.Message);
        Assert.Equal(2, downloads.Count);
        using (var zip = ZipFile.OpenRead(first.LibraryPath(pins[0])))
        {
            Assert.Null(zip.GetEntry("assets/hook.dll"));
            Assert.NotNull(zip.GetEntry("python/engines/fixture.py"));
            Assert.NotNull(zip.GetEntry("LICENSE"));
            Assert.NotNull(zip.GetEntry("provenance/NOTICE.md"));
        }
        File.Delete(first.LibraryPath(pins[0]));
        var restored = await first.EnsureReferenceAsync(pins[0], default);
        Assert.Equal(pins[0], restored);
        Assert.Equal(3, downloads.Count);
        Assert.EndsWith("/zip/" + Revision, downloads[^1]);
        Assert.Empty(Directory.EnumerateFiles(root, "*.bak", SearchOption.AllDirectories));
        Assert.Equal(2, Directory.EnumerateFiles(root, "*", SearchOption.AllDirectories).Count());
    }

    [Fact]
    public async Task FailedPreparationDoesNotCommitAndChangedSourceCannotReplacePinnedLibrary()
    {
        var failDownload = true;
        var changed = false;
        using var client = new HttpClient(new Handler(request =>
        {
            if (request.RequestUri!.Host == "api.github.com") return Text("{\"sha\":\"" + Revision + "\"}");
            if (failDownload) return new(HttpStatusCode.ServiceUnavailable);
            return Bytes(Library(changed ? "different" : "engine guide"));
        }));
        var toolkit = new EngineReferenceToolkit(root, client, allowDownloads: true);
        await Assert.ThrowsAsync<HttpRequestException>(() => toolkit.EnsureReferenceAsync(null, default));
        Assert.Null(toolkit.CurrentReference());
        failDownload = false;
        var pin = await toolkit.EnsureReferenceAsync(null, default);
        File.Delete(toolkit.LibraryPath(pin));
        changed = true;
        await Assert.ThrowsAsync<InvalidDataException>(() => toolkit.EnsureReferenceAsync(pin, default));
        Assert.False(File.Exists(toolkit.LibraryPath(pin)));
        Assert.Equal(pin, toolkit.CurrentReference());
        changed = false;
        Assert.Equal(pin, await toolkit.EnsureReferenceAsync(pin, default));
        await File.AppendAllTextAsync(toolkit.LibraryPath(pin), "changed locally");
        await Assert.ThrowsAsync<InvalidDataException>(() => toolkit.EnsureReferenceAsync(pin, default));
    }

    [Fact]
    public async Task ArchiveTraversalIsRejectedBeforePublishingFiles()
    {
        using var client = new HttpClient(new Handler(request => request.RequestUri!.Host == "api.github.com"
            ? Text("{\"sha\":\"" + Revision + "\"}")
            : Bytes(Zip(new Dictionary<string, string> { ["repo/../escape.py"] = "invalid" }))));
        var toolkit = new EngineReferenceToolkit(root, client, allowDownloads: true);
        await Assert.ThrowsAsync<InvalidDataException>(() => toolkit.EnsureReferenceAsync(null, default));
        Assert.Null(toolkit.CurrentReference());
        Assert.False(File.Exists(Path.Combine(root, "escape.py")));
    }

    [Fact]
    public async Task PythonRestoresMissingFilesButRejectsAlteredVersions()
    {
        var downloads = new List<string>();
        using var client = Client(downloads);
        var toolkit = new EngineReferenceToolkit(root, client, allowDownloads: true);
        var pin = await toolkit.EnsureReferenceAsync(null, default);
        var prepared = await toolkit.EnsurePythonAsync(pin, null, default);
        Assert.True(File.Exists(prepared.Paths.Python));
        Assert.Equal(3, downloads.Count);
        var reused = await new EngineReferenceToolkit(root, client, allowDownloads: true).EnsurePythonAsync(pin, prepared.Dependency, default);
        Assert.Equal(prepared, reused);
        Assert.Equal(3, downloads.Count);
        File.Delete(prepared.Paths.Python);
        Assert.Equal(prepared, await toolkit.EnsurePythonAsync(pin, prepared.Dependency, default));
        Assert.Equal(4, downloads.Count);
        await File.AppendAllTextAsync(prepared.Paths.Python, "altered");
        await Assert.ThrowsAsync<InvalidDataException>(() => toolkit.EnsurePythonAsync(pin, prepared.Dependency, default));
        Assert.Equal(4, downloads.Count);
    }

    [Fact]
    public async Task AnalysisToolsExposeLocalApiAndPublishedRuntimeResolvesSameDependency()
    {
        var downloads = new List<string>();
        using var client = Client(downloads);
        var state = new AdaptationSession();
        var workspace = Path.Combine(root, "project");
        Directory.CreateDirectory(Path.Combine(workspace, "work"));
        var store = new LocalAdapterPackageStore(Path.Combine(root, "extensions"));
        var runner = new CaptureRunner();
        var compiler = new AdapterRuntime(new(root, "10.0.402", "10.0.13"), runner);
        var preparation = new EngineReferenceToolkit(Path.Combine(store.RootDirectory, "engine-tools"), client, allowDownloads: true);
        var preparedPin = await preparation.EnsureReferenceAsync(null, default);
        await preparation.EnsurePythonAsync(preparedPin, null, default);
        var tools = new AdaptationTools(new("fixture", workspace, []), state, workspace, compiler, store, client);
        using var inspected = JsonDocument.Parse(await tools.ExecuteAsync(new("inspect", "inspect_engine_reference", """{"engine":"fixture"}"""), default));
        var guide = inspected.RootElement.GetProperty("result");
        Assert.Equal("engine guide", guide.GetProperty("content").GetString());
        Assert.Contains("read", guide.GetProperty("publicSymbols").ToString());
        Assert.Equal(Revision, state.EngineReference!.Revision);
        await tools.ExecuteAsync(new("bind", "use_engine_reference", "{}"), default);
        var source = Path.Combine(workspace, "src");
        Assert.Equal(2, Directory.EnumerateFiles(source, "*", SearchOption.AllDirectories).Count());
        Assert.False(Directory.Exists(Path.Combine(workspace, "python")));
        var toolkit = new EngineReferenceToolkit(Path.Combine(store.RootDirectory, "engine-tools"), client);
        var paths = await toolkit.ResolvePackageAsync(source, default);
        Assert.NotNull(paths);
        await File.WriteAllTextAsync(Path.Combine(workspace, "work", "sample.py"), "print('测试')");
        await tools.ExecuteAsync(new("run", "run_engine_python", """{"script":"work/sample.py","arguments":["中文 空格"],"stdin":"输入"}"""), default);
        Assert.Equal(paths.Python, runner.Last!.ExecutablePath);
        Assert.Contains(paths.EngineLibrary, runner.Last.Arguments);
        Assert.Contains("-I", runner.Last.Arguments);
        Assert.Contains("-B", runner.Last.Arguments);
        Assert.Equal("中文 空格", runner.Last.Arguments[^1]);
        Assert.Equal("输入", runner.Last.StandardInput);
        Assert.Equal(3, downloads.Count);
        await tools.ExecuteAsync(new("module", "run_engine_python",
            """{"module":"python.engines.fixture","arguments":["--help"],"stdin":"模块输入"}"""), default);
        Assert.Contains(EngineReferenceToolkit.ModuleBootstrap, runner.Last.Arguments);
        Assert.Contains("python.engines.fixture", runner.Last.Arguments);
        Assert.Equal("--help", runner.Last.Arguments[^1]);
        Assert.Equal("模块输入", runner.Last.StandardInput);
        await Assert.ThrowsAsync<InvalidDataException>(() => tools.ExecuteAsync(new("both", "run_engine_python",
            """{"script":"work/sample.py","module":"python.engines.fixture"}"""), default));
        await Assert.ThrowsAsync<InvalidDataException>(() => tools.ExecuteAsync(new("outside", "run_engine_python",
            """{"module":"os"}"""), default));
        await Assert.ThrowsAsync<FileNotFoundException>(() => tools.ExecuteAsync(new("absent", "run_engine_python",
            """{"module":"python.engines.absent"}"""), default));
        // Subsequent search uses the pinned local catalog, not main or another HTTP request.
        using var searched = JsonDocument.Parse(await tools.ExecuteAsync(new("search", "search_engine_references", """{"query":"fixture"}"""), default));
        Assert.Contains(Revision, searched.RootElement.GetProperty("result").GetProperty("matches")[0].GetProperty("documentUrl").GetString());
        Assert.Equal(3, downloads.Count);
    }

    [Fact]
    public async Task InlineProbeWritesUtf8AndInvalidOrFailedWritesNeverExecute()
    {
        using var client = Client([]);
        var workspace = Path.Combine(root, "project");
        var store = new LocalAdapterPackageStore(Path.Combine(root, "extensions"));
        var runner = new CaptureRunner();
        var runtime = new AdapterRuntime(new(root, "10.0.402", "10.0.13"), runner);
        var preparation = new EngineReferenceToolkit(Path.Combine(store.RootDirectory, "engine-tools"), client, allowDownloads: true);
        var pin = await preparation.EnsureReferenceAsync(null, default);
        await preparation.EnsurePythonAsync(pin, null, default);
        var state = new AdaptationSession();
        var tools = new AdaptationTools(new("fixture", workspace, []), state, workspace, runtime, store, client);
        const string code = "print('探针：中文')\n";
        await tools.ExecuteAsync(new("inline", "run_engine_python", JsonSerializer.Serialize(new
        {
            script = "work/probes/sample.py", content = code, arguments = new[] { "中文 参数" }, stdin = "输入"
        })), default);
        var path = Path.Combine(workspace, "work/probes/sample.py");
        Assert.Equal(Encoding.UTF8.GetBytes(code), await File.ReadAllBytesAsync(path));
        Assert.Contains(Path.GetFullPath(path), runner.Last!.Arguments);
        Assert.Equal("中文 参数", runner.Last.Arguments[^1]);
        Assert.Equal("输入", runner.Last.StandardInput);
        Assert.Null(state.ReleaseFiles);

        runner.Last = null;
        foreach (var invalid in new[]
        {
            """{"module":"python.engines.fixture","content":"print(1)"}""",
            """{"script":"src/tools/runtime.py","content":"print(1)"}""",
            """{"script":"work/new.py","content":null}""",
            """{"script":"work/new.py","content":"print(1)","timeoutSeconds":0}"""
        })
            await Assert.ThrowsAsync<InvalidDataException>(() => tools.ExecuteAsync(new("invalid", "run_engine_python", invalid), default));
        Assert.False(File.Exists(Path.Combine(workspace, "work/new.py")));
        Assert.False(File.Exists(Path.Combine(workspace, "src/tools/runtime.py")));
        Directory.CreateDirectory(Path.Combine(workspace, "work/blocked.py"));
        var error = await Record.ExceptionAsync(() => tools.ExecuteAsync(new("blocked", "run_engine_python",
            """{"script":"work/blocked.py","content":"print(1)"}"""), default));
        Assert.True(error is IOException or UnauthorizedAccessException, error?.ToString() ?? "Expected a failed write.");
        Assert.Null(runner.Last);
        Assert.False(File.Exists(Path.Combine(workspace, "work/blocked.py.write.tmp")));
    }

    private static HttpClient Client(List<string> requests) => new(new Handler(request =>
    {
        requests.Add(request.RequestUri!.AbsoluteUri);
        return request.RequestUri.Host switch
        {
            "api.github.com" => Text("{\"sha\":\"" + Revision + "\"}"),
            "codeload.github.com" => Bytes(Library()),
            "www.python.org" => Bytes(Zip(new Dictionary<string, string>
            {
                ["python.exe"] = "fixture executable, never executed",
                ["python314.zip"] = "fixture stdlib",
                ["LICENSE.txt"] = "fixture license"
            })),
            _ => throw new InvalidOperationException("Unexpected network request: " + request.RequestUri)
        };
    }));

    private static byte[] Library(string guide = "engine guide") => Zip(new Dictionary<string, string>
    {
        ["repo/catalog/engines.json"] = """
            {"engines":[{"id":"fixture","title":"Fixture","document":"engines/fixture.md",
              "python_references":["python/engines/fixture.py"],"public_symbols":{"python/engines/fixture.py":["read","patch"]}}]}
            """,
        ["repo/engines/fixture.md"] = guide,
        ["repo/python/detect.py"] = "def detect_directory(root): return {}",
        ["repo/python/__init__.py"] = "",
        ["repo/python/engines/__init__.py"] = "",
        ["repo/python/engines/fixture.py"] = "def read(data): return data",
        ["repo/LICENSE"] = "fixture license",
        ["repo/provenance/NOTICE.md"] = "fixture notice",
        ["repo/assets/hook.dll"] = "must not be retained"
    });
    private static byte[] Zip(Dictionary<string, string> files)
    {
        using var memory = new MemoryStream();
        using (var zip = new ZipArchive(memory, ZipArchiveMode.Create, true))
            foreach (var (path, content) in files)
            {
                var entry = zip.CreateEntry(path);
                entry.LastWriteTime = new DateTimeOffset(2000, 1, 1, 0, 0, 0, TimeSpan.Zero);
                using var writer = new StreamWriter(entry.Open(), new UTF8Encoding(false));
                writer.Write(content);
            }
        return memory.ToArray();
    }
    private static HttpResponseMessage Text(string text) => new(HttpStatusCode.OK) { Content = new StringContent(text) };
    private static HttpResponseMessage Bytes(byte[] bytes) => new(HttpStatusCode.OK) { Content = new ByteArrayContent(bytes) };
    private sealed class Handler(Func<HttpRequestMessage, HttpResponseMessage> response) : HttpMessageHandler
    {
        protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken) => Task.FromResult(response(request));
    }
    private sealed class CaptureRunner : IAdapterProcessRunner
    {
        public AdapterProcessRequest? Last;
        public Task<AdapterProcessResult> RunAsync(AdapterProcessRequest request, CancellationToken cancellationToken)
        { Last = request; return Task.FromResult(new AdapterProcessResult(0, "测试", "")); }
    }
    public void Dispose() { if (Directory.Exists(root)) Directory.Delete(root, true); }
}
