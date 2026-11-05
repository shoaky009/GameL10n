using System.IO.Compression;
using System.Reflection;
using System.Diagnostics;
using System.Text;
using System.Text.Json;
using GameL10n.Core.Adapters;
using GameL10n.Core.Development;
using GameL10n.VnTextPatch;

namespace GameL10n.Tests.Development;

public sealed class BuiltinAdapterAssetsTests : IDisposable
{
    private readonly string root = Path.Combine(Path.GetTempPath(), "gamel10n-builtin-assets-" + Guid.NewGuid().ToString("N"));
    private static readonly JsonSerializerOptions Json = new(JsonSerializerDefaults.Web);

    [Theory]
    [InlineData("corrupt")]
    [InlineData("missing")]
    [InlineData("unavailable")]
    public async Task UnreadableSourceBundlePreservesCapabilitiesAndBinaryReuse(string code)
    {
        var assets = CreateAssets();
        string sourceBundle = Path.Combine(root, "assets", "sources.zip");
        if (code == "corrupt") await File.WriteAllBytesAsync(sourceBundle, []);
        else if (code == "missing") File.Delete(sourceBundle);
        using var held = code == "unavailable" ? new FileStream(sourceBundle, FileMode.Open, FileAccess.Read, FileShare.None) : null;

        var description = JsonSerializer.SerializeToElement(assets.Describe("Renpy"), Json);
        Assert.Equal("Renpy", description.GetProperty("format").GetString());
        Assert.True(description.GetProperty("binaryAvailable").GetBoolean());
        Assert.False(description.GetProperty("sourceAvailable").GetBoolean());
        var issue = description.GetProperty("sourceBundleIssue");
        Assert.Equal(code, issue.GetProperty("code").GetString());
        Assert.Equal("sources.zip", issue.GetProperty("asset").GetString());
        Assert.Contains("预编译", description.GetProperty("usage").GetString());
        Assert.Equal(new[] { "extract", "build", "validate" }, description.GetProperty("operations").EnumerateArray().Select(item => item.GetString()));
        Assert.NotNull(assets.ReadSource("Renpy", "tools/gamel10n_host.py"));

        var error = Assert.Throws<BuiltinAdapterAssetException>(() => assets.ReadSource("Renpy", "vendor/Scripts/RenpyScript.cs"));
        Assert.Equal(code, error.Code);
        Assert.Equal("sources.zip", error.AssetName);
        Assert.Throws<ArgumentException>(() => assets.Describe("unknown-format"));
        string editableWorkspace = Path.Combine(root, "editable");
        await Assert.ThrowsAsync<InvalidDataException>(() => assets.CopyAsync("Renpy", editableWorkspace, true));
        Assert.False(Directory.Exists(editableWorkspace));

        string workspace = Path.Combine(root, "binary");
        await assets.CopyAsync("Renpy", workspace, false);
        var toolkit = new EngineReferenceToolkit(Path.Combine(root, "shared"), parserAssets: assets);
        Assert.True(File.Exists(await toolkit.ResolveVnTextPatchAsync(Path.Combine(workspace, "src"), default)));
    }

    [Fact]
    public async Task BinaryAdaptersSharePinnedParserAndDoNotSilentlyUpgrade()
    {
        var assets = CreateAssets();
        var first = Path.Combine(root, "first");
        var second = Path.Combine(root, "second");
        await assets.CopyAsync("Renpy", first, false);
        await assets.CopyAsync("KirikiriKs", second, false);
        var shared = Path.Combine(root, "shared");
        var toolkit = new EngineReferenceToolkit(shared, parserAssets: assets);
        var parser = await toolkit.ResolveVnTextPatchAsync(Path.Combine(first, "src"), default);
        Assert.Equal(parser, await toolkit.ResolveVnTextPatchAsync(Path.Combine(second, "src"), default));
        Assert.Single(Directory.EnumerateFiles(shared, "*.dll", SearchOption.AllDirectories));
        Assert.Empty(Directory.EnumerateFiles(first, "*.dll", SearchOption.AllDirectories));
        Assert.Empty(Directory.EnumerateFiles(second, "*.dll", SearchOption.AllDirectories));
        var dependency = (await GameL10n.Core.Storage.LocalFiles.ReadJsonAsync<BuiltinParserDependency>(
            Path.Combine(first, "src", BuiltinAdapterAssets.SharedDependencyPath), FixtureJson.TypeInfo<global::GameL10n.Core.Development.BuiltinParserDependency>(FixtureJson.Storage)))!;
        Assert.Equal(2, dependency.SchemaVersion);
        await Assert.ThrowsAsync<InvalidDataException>(() => assets.ResolveSharedAsync(Path.Combine(root, "incomplete-tool"),
            dependency with { Files = dependency.Files.Where(pair => !pair.Key.EndsWith(".runtimeconfig.json")).ToDictionary() }));
        Assert.False(Directory.Exists(Path.Combine(root, "incomplete-tool")));
        var command = await toolkit.ResolveParserAsync(Path.Combine(first, "src"), ParserToolFixture.Toolchain, default);
        Assert.Equal(new[] { ParserToolFixture.Toolchain.ExecutablePath, parser! }, command!.Command);
        var legacyFiles = dependency.Files.Where(pair => pair.Key is "GameL10n.VnTextPatch.dll" or "VNTextPatch.LICENSE" or "VNTextPatch.NOTICE.md")
            .ToDictionary(pair => pair.Key.Replace("GameL10n.VnTextPatch.dll", "AvgTranslator.VnTextPatch.dll"), pair => pair.Value);
        var legacyDirectory = Path.Combine(shared, "vntextpatch", GeneratedAdapterResources.HashEntries(legacyFiles));
        Directory.CreateDirectory(legacyDirectory);
        foreach (var file in dependency.Files.Keys.Where(file => file is "GameL10n.VnTextPatch.dll" or "VNTextPatch.LICENSE" or "VNTextPatch.NOTICE.md"))
            File.Copy(Path.Combine(Path.GetDirectoryName(parser!)!, file),
                Path.Combine(legacyDirectory, file.Replace("GameL10n.VnTextPatch.dll", "AvgTranslator.VnTextPatch.dll")));
        var legacyDependency = new BuiltinParserDependency(1, legacyFiles);
        var legacyParser = await assets.ResolveSharedAsync(shared, legacyDependency);
        Assert.Equal(Path.Combine(legacyDirectory, "AvgTranslator.VnTextPatch.dll"), legacyParser);
        await File.AppendAllTextAsync(Path.Combine(root, "assets", "GameL10n.VnTextPatch.dll"), "changed app version");
        Assert.Equal(parser, await toolkit.ResolveVnTextPatchAsync(Path.Combine(first, "src"), default));
        Assert.Equal(legacyParser, await assets.ResolveSharedAsync(shared, legacyDependency));
        var emptyCache = new EngineReferenceToolkit(Path.Combine(root, "new-shared"), parserAssets: assets);
        await Assert.ThrowsAsync<InvalidDataException>(() => emptyCache.ResolveVnTextPatchAsync(Path.Combine(first, "src"), default));
        await File.AppendAllTextAsync(parser!, "corrupted shared file");
        await Assert.ThrowsAsync<InvalidDataException>(() => toolkit.ResolveVnTextPatchAsync(Path.Combine(first, "src"), default));
    }

    [Fact]
    public async Task KaguyaRuntimeResearchIsConditionalReadOnlyAndKeepsSourceResponseShape()
    {
        var assets = CreateAssets();
        var description = JsonSerializer.SerializeToElement(assets.Describe("Kaguya"), Json);
        var guidance = description.GetProperty("engineGuidance");
        Assert.Contains("CP932", guidance.GetProperty("runtimeSupport").GetString());
        Assert.Contains("UnitId", guidance.GetProperty("translationMemory").GetString());
        Assert.Equal(JsonValueKind.Null, guidance.GetProperty("runtimeInspection").ValueKind);
        string referencePath = guidance.GetProperty("referenceSource").GetString()!;
        Assert.Contains(description.GetProperty("sourceFiles").EnumerateArray(), item => item.GetProperty("path").GetString() == referencePath);
        var slice = JsonSerializer.SerializeToElement(assets.ReadSource("Kaguya", referencePath, 12, 24), Json);
        Assert.Equal(new[] { "format", "path", "offset", "length", "totalLength", "nextOffset", "truncated", "content" },
            slice.EnumerateObject().Select(property => property.Name));
        Assert.Equal(24, slice.GetProperty("content").GetString()!.Length);
        Assert.Equal(36, slice.GetProperty("nextOffset").GetInt32());

        string workspace = Path.Combine(root, "workspace");
        await assets.CopyAsync("Kaguya", workspace, false);
        Assert.False(Directory.Exists(Path.Combine(workspace, "src/BuiltinVnTextPatch/reference")));
        var unrelated = JsonSerializer.SerializeToElement(assets.Describe("Renpy"), Json);
        Assert.Equal(JsonValueKind.Null, unrelated.GetProperty("engineGuidance").ValueKind);
        Assert.DoesNotContain("KaGuYa", unrelated.GetRawText());
    }

    [Fact]
    public async Task KirikiriEncodingGuidanceIsScopedAndExcludedFromAdapterSources()
    {
        var assets = CreateAssets();
        var description = JsonSerializer.SerializeToElement(assets.Describe("KirikiriKs"), Json);
        var guidance = description.GetProperty("engineGuidance");
        Assert.Contains("UTF-16", guidance.GetProperty("parserReuse").GetString());
        var reference = guidance.GetProperty("referenceSource").GetString()!;
        var details = JsonSerializer.SerializeToElement(assets.ReadSource("KirikiriKs", reference), Json);
        Assert.Contains("同一文件混写", details.GetProperty("content").GetString());
        Assert.DoesNotContain(description.GetProperty("limitations").EnumerateArray(),
            item => item.GetString()!.StartsWith("保留内置解析器的原编码约束"));
        Assert.Equal(JsonValueKind.Null, JsonSerializer.SerializeToElement(assets.Describe("Renpy"), Json)
            .GetProperty("engineGuidance").ValueKind);
        var workspace = Path.Combine(root, "workspace");
        await assets.CopyAsync("KirikiriKs", workspace, false);
        Assert.False(Directory.Exists(Path.Combine(workspace, "src/BuiltinVnTextPatch/reference")));
    }

    [Fact]
    public async Task SourceIndexIsBoundedAndPythonCopyIsRepeatableWithoutReplacingExistingFiles()
    {
        var assets = CreateAssets();
        var description = JsonSerializer.SerializeToElement(assets.Describe("Renpy"), Json);
        var index = description.GetProperty("sourceFiles").EnumerateArray().Select(item => item.GetProperty("path").GetString()).ToArray();
        Assert.Contains("vendor/Scripts/RenpyScript.cs", index);
        Assert.DoesNotContain(index, path => path!.Contains("Majiro"));
        Assert.DoesNotContain(index, path => path!.EndsWith("Program.cs"));
        var slice = JsonSerializer.SerializeToElement(assets.ReadSource("Renpy", "vendor/Scripts/RenpyScript.cs", 12, 24), Json);
        Assert.Equal(24, slice.GetProperty("content").GetString()!.Length);
        Assert.Equal(36, slice.GetProperty("nextOffset").GetInt32());
        Assert.Throws<ArgumentOutOfRangeException>(() => assets.ReadSource("Renpy", "vendor/Scripts/RenpyScript.cs", 0, 24001));
        Assert.Throws<InvalidDataException>(() => assets.ReadSource("Renpy", "../Worker.cs"));

        string workspace = Path.Combine(root, "workspace");
        var release = new List<AdapterReleaseFile>();
        await assets.CopyAsync("Renpy", workspace, false, releaseFiles: release);
        Assert.Contains(release, file => file.Path == "tools/gamel10n_host.py" && file.Role == "runtime");
        Assert.Contains(release, file => file.Role == "runtime" && file.Path == BuiltinAdapterAssets.SharedDependencyPath);
        Assert.Equal(2, release.Count);
        string bridge = Path.Combine(workspace, "src/tools/gamel10n_host.py");
        byte[] original = await File.ReadAllBytesAsync(bridge);
        Assert.False(File.Exists(Path.Combine(workspace, "src/tools/vntextpatch/GameL10n.VnTextPatch.dll")));
        Assert.True(File.Exists(Path.Combine(workspace, "src", BuiltinAdapterAssets.SharedDependencyPath)));
        Assert.False(Directory.Exists(Path.Combine(workspace, "src/BuiltinVnTextPatch")));
        var repeat = JsonSerializer.SerializeToElement(await assets.CopyAsync("Renpy", workspace, false), Json);
        Assert.Contains("src/tools/gamel10n_host.py", repeat.GetProperty("reused").EnumerateArray().Select(item => item.GetString()));
        Assert.Equal(original, await File.ReadAllBytesAsync(bridge));
        await Assert.ThrowsAsync<InvalidDataException>(() => assets.CopyAsync("Renpy", workspace, true));
        Assert.Empty(Directory.GetFiles(Path.Combine(workspace, "src"), "*.cs", SearchOption.AllDirectories));
    }

    [Fact]
    public async Task AnyConflictingDestinationPreventsAllWrites()
    {
        var assets = CreateAssets();
        string workspace = Path.Combine(root, "workspace");
        string conflict = Path.Combine(workspace, "src/tools/gamel10n_host.py");
        Directory.CreateDirectory(Path.GetDirectoryName(conflict)!);
        await File.WriteAllTextAsync(conflict, "Keep my modifications.");
        await Assert.ThrowsAsync<IOException>(() => assets.CopyAsync("Renpy", workspace, false));
        Assert.Equal(new[] { Path.GetFullPath(conflict) }, Directory.GetFiles(workspace, "*", SearchOption.AllDirectories));
        Assert.Equal("Keep my modifications.", await File.ReadAllTextAsync(conflict));
    }

    [Theory]
    [InlineData("Renpy", 2)]
    [InlineData("KirikiriKs", 2)]
    [InlineData("Renpy", 1)]
    [InlineData("KirikiriKs", 1)]
    public async Task CopiedPythonHelperCallsPrecompiledParserAndWritesChinese(string format, int schemaVersion)
    {
        var assets = CreateAssets();
        string workspace = Path.Combine(root, "workspace");
        await assets.CopyAsync(format, workspace, false);
        using var fixture = new EngineArchiveFixture();
        var runner = fixture.Runner;
        var source = Path.Combine(workspace, "src");
        var entry = Path.Combine(source, "adapter.py");
        await File.WriteAllTextAsync(entry, """
            import json, sys
            from tools.gamel10n_host import parser
            request = json.load(sys.stdin)
            print(json.dumps(parser(request, request['format']), ensure_ascii=False))
            """);
        string resources = Directory.CreateDirectory(Path.Combine(root, "resources")).FullName;
        string file = format == "Renpy" ? "scene.rpy" : "scene.ks";
        string sourceText = format == "Renpy" ? "こんにちは。" : "こんにちは♪・。";
        string targetText = format == "Renpy" ? "你好，中文。" : "你好，中文♪・。";
        string script = format == "Renpy" ? $"label start:\n    \"{sourceText}\"\n" : $"*start\r\n{sourceText}[p]\r\n";
        Encoding.RegisterProvider(CodePagesEncodingProvider.Instance);
        await File.WriteAllTextAsync(Path.Combine(resources, file), script, format == "Renpy" ? new UTF8Encoding(false) : Encoding.GetEncoding(932));
        if (schemaVersion == 1)
        {
            var legacy = new BuiltinParserDependency(1, ParserToolFixture.Files.Where(name => name.EndsWith(".dll") || name.StartsWith("VNTextPatch."))
                .ToDictionary(name => name, name => GeneratedAdapterResources.HashFile(Path.Combine(root, "assets", name))));
            await File.WriteAllTextAsync(Path.Combine(source, BuiltinAdapterAssets.SharedDependencyPath), JsonSerializer.Serialize(legacy, Json));
        }
        var sharedParser = await new EngineReferenceToolkit(Path.Combine(root, "shared"), parserAssets: assets)
            .ResolveParserAsync(source, ParserToolFixture.Toolchain, default, legacyHostPath: typeof(GameL10n.ParserCompat.Program).Assembly.Location);
        var finalResponse = Path.Combine(root, "adapter-final.json");
        await File.WriteAllTextAsync(finalResponse, "final response is not an intermediate parser response");
        async Task<AdapterProcessResult> Invoke(object request)
        {
            var payload = JsonSerializer.SerializeToNode(request, Json)!;
            payload["format"] = format;
            payload["responseFile"] = finalResponse;
            payload["toolPaths"] = JsonSerializer.SerializeToNode(new { hostCommand = EngineArchiveFixture.HostCommand,
                vnTextPatch = sharedParser!.Path, vnTextPatchCommand = sharedParser.Command }, Json);
            return await runner.RunAsync(new(fixture.Paths.Python,
                EngineReferenceToolkit.PythonArguments(fixture.Paths, entry, []), source, payload.ToJsonString()), default);
        }
        var extraction = await Invoke(new { protocolVersion = 1, operation = "extract", sourceDirectory = resources, resourceFiles = new[] { file } });
        Assert.Equal(0, extraction.ExitCode);
        using var parsed = JsonDocument.Parse(extraction.StandardOutput);
        Assert.True(parsed.RootElement.GetProperty("success").GetBoolean(), extraction.StandardOutput);
        string id = Assert.Single(parsed.RootElement.GetProperty("extraction").GetProperty("units").EnumerateArray()).GetProperty("id").GetString()!;
        string output = Path.Combine(root, "patched");
        var build = await Invoke(new { protocolVersion = 1, operation = "build", sourceDirectory = resources, outputDirectory = output,
            resourceFiles = new[] { file }, translations = new Dictionary<string, string> { [id] = targetText } });
        using var built = JsonDocument.Parse(build.StandardOutput);
        Assert.True(built.RootElement.GetProperty("success").GetBoolean(), build.StandardOutput);
        Assert.Equal(script.Replace(sourceText, targetText), await File.ReadAllTextAsync(Path.Combine(output, file)));
        Assert.Equal("final response is not an intermediate parser response", await File.ReadAllTextAsync(finalResponse));
        Assert.Empty(Directory.EnumerateDirectories(source, "parser-*"));
        if (format == "KirikiriKs") Assert.True(File.ReadAllBytes(Path.Combine(output, file)).AsSpan().StartsWith(Encoding.Unicode.GetPreamble()));
        var probe = await Invoke(new { protocolVersion = 1, operation = "probe", sourceDirectory = resources });
        Assert.NotEqual(0, probe.ExitCode);
        Assert.Contains("有界范围", probe.StandardError);

        var host = EngineArchiveFixture.HostCommand;
        var failure = await runner.RunAsync(new(host[0], host.Skip(1).Append("parser").ToArray(), source,
            JsonSerializer.Serialize(new { protocolVersion = 1, operation = "unknown", sourceDirectory = resources, format }, Json),
            VnTextPatchCommand: sharedParser!.Command), default);
        Assert.NotEqual(0, failure.ExitCode);
        using var failed = JsonDocument.Parse(failure.StandardOutput);
        Assert.False(failed.RootElement.GetProperty("success").GetBoolean());
        Assert.Equal(1, failed.RootElement.GetProperty("protocolVersion").GetInt32());
        Assert.Equal(JsonValueKind.Object, failed.RootElement.GetProperty("diagnostic").ValueKind);
    }

    [Fact]
    public async Task CancelingTheHostStopsParserAndItsChildren()
    {
        using var fixture = new EngineArchiveFixture();
        var source = Directory.CreateDirectory(Path.Combine(fixture.Root, "resources")).FullName;
        var marker = Path.Combine(fixture.Root, "child-pids.json");
        var script = Path.Combine(fixture.Root, "waiting-parser.py");
        await File.WriteAllTextAsync(script, """
            import json, os, subprocess, sys, time
            request = json.load(sys.stdin)
            child = subprocess.Popen([sys.executable, '-I', '-c', 'import time; time.sleep(120)'])
            with open(sys.argv[1] + '.tmp', 'w', encoding='utf-8') as output:
                json.dump([os.getpid(), child.pid], output)
            os.replace(sys.argv[1] + '.tmp', sys.argv[1])
            time.sleep(120)
            """);
        var host = EngineArchiveFixture.HostCommand;
        using var cancel = new CancellationTokenSource();
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(20));
        var execution = fixture.Runner.RunAsync(new(host[0], host.Skip(1).Append("parser").ToArray(), fixture.Root,
            JsonSerializer.Serialize(new { protocolVersion = 1, operation = "extract", format = "Renpy", sourceDirectory = source,
                toolPaths = new { vnTextPatchCommand = new[] { fixture.Paths.Python, "-I", "-B", script, marker } } }, Json)), cancel.Token);
        try
        {
            while (!File.Exists(marker))
            {
                if (execution.IsCompleted) Assert.Fail((await execution).StandardError);
                await Task.Delay(25, deadline.Token);
            }
            var ids = JsonSerializer.Deserialize<int[]>(await File.ReadAllTextAsync(marker))!;
            using var parser = Process.GetProcessById(ids[0]);
            using var child = Process.GetProcessById(ids[1]);
            cancel.Cancel();
            await Assert.ThrowsAnyAsync<OperationCanceledException>(() => execution);
            await Task.WhenAll(parser.WaitForExitAsync(), child.WaitForExitAsync()).WaitAsync(deadline.Token);
            Assert.Empty(Directory.EnumerateFiles(source));
        }
        finally
        {
            cancel.Cancel();
            try { await execution; }
            catch (OperationCanceledException) { }
        }
    }

    [Fact]
    public void SafeXorMatchesBytewiseReferenceForOffsetsAndRepeatingKeys()
    {
        var method = typeof(Worker).Assembly.GetType("VNTextPatch.Shared.Util.BinaryUtil")!.GetMethod("Xor",
            BindingFlags.Public | BindingFlags.Static, [typeof(byte[]), typeof(int), typeof(int), typeof(byte[])])!;
        foreach (int keyLength in new[] { 1, 3, 4, 7, 8, 11 })
        foreach (int length in new[] { 0, 1, 3, 4, 8, 13, 29 })
        {
            byte[] key = Enumerable.Range(1, keyLength).Select(i => (byte)(i * 17)).ToArray();
            byte[] data = Enumerable.Range(0, 40).Select(i => (byte)i).ToArray();
            byte[] expected = data.ToArray();
            for (int i = 0; i < length; i++) expected[i + 3] ^= key[i % key.Length];
            method.Invoke(null, [data, 3, length, key]);
            Assert.Equal(expected, data);
        }
    }

    private BuiltinAdapterAssets CreateAssets()
    {
        string assets = Path.Combine(root, "assets");
        var result = ParserToolFixture.Create(assets);
        string vendor = Path.Combine(Repository(), "third_party/VNTextPatch");
        using var archive = ZipFile.Open(Path.Combine(assets, "sources.zip"), ZipArchiveMode.Create);
        foreach (string path in Directory.EnumerateFiles(vendor, "*.cs", SearchOption.AllDirectories))
            archive.CreateEntryFromFile(path, "vendor/" + Path.GetRelativePath(vendor, path).Replace('\\', '/'));
        foreach (string name in new[] { "Worker.cs", "Protocol.cs", "Serialization/VnTextPatchJson.cs" })
            archive.CreateEntryFromFile(Path.Combine(Repository(), "tools/GameL10n.VnTextPatch", name), "worker/" + name);
        return result;
    }
    private static string Repository()
    {
        var directory = new DirectoryInfo(AppContext.BaseDirectory);
        while (directory is not null && !File.Exists(Path.Combine(directory.FullName, "global.json"))) directory = directory.Parent;
        return directory?.FullName ?? throw new InvalidOperationException("Repository fixture was not found.");
    }
    public void Dispose()
    {
        if (Directory.Exists(root)) Directory.Delete(root, true);
    }
}
