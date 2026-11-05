using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using GameL10n.Core;
using GameL10n.Core.Adapters;
using GameL10n.Core.Development;

namespace GameL10n.Tests.Development;

public sealed class GeneratedAdapterRuntimeTests : IDisposable
{
    private readonly string root = Path.Combine(Path.GetTempPath(), "gamel10n-generated-runtime-" + Guid.NewGuid().ToString("N"));
    private static readonly JsonSerializerOptions Json = new(JsonSerializerDefaults.Web);

    public GeneratedAdapterRuntimeTests() => Directory.CreateDirectory(root);

    [Fact]
    public async Task PythonAdapterBuildsPublishesAndLoadsWithoutDotnetAndRejectsModifiedSource()
    {
        using var fixture = new EngineArchiveFixture();
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var dependency = await fixture.PrepareSharedToolsAsync(store.RootDirectory);
        var source = Directory.CreateDirectory(Path.Combine(root, "src")).FullName;
        Directory.CreateDirectory(Path.Combine(source, "tools"));
        await File.WriteAllTextAsync(Path.Combine(source, EngineReferenceToolkit.DependencyPath), JsonSerializer.Serialize(dependency, Json));
        await File.WriteAllTextAsync(Path.Combine(source, "adapter.py"), """
            import json, sys
            from pathlib import Path
            request = json.load(sys.stdin)
            response = {'protocolVersion': 1, 'success': True}
            if request['operation'] == 'probe':
                response['probe'] = dict(engineId='python-fixture', adapterId='python-fixture', adapterVersion='1',
                    description='中文路径', requiredFiles=request['resourceFiles'], warnings=[])
            else:
                response['extraction'] = dict(units=[dict(id='line:1', resourcePath=request['resourceFiles'][0],
                    locator='line:1', sourceText=Path(request['sourceDirectory'], request['resourceFiles'][0]).read_text(encoding='utf-8'), context='fixture')], warnings=[])
            Path(request['responseFile']).write_text(json.dumps(response, ensure_ascii=False), encoding='utf-8')
            """, new UTF8Encoding(false));
        var builder = new GeneratedAdapterBuilder(fixture.Runner);
        AdapterReleaseFile[] release = [new("src/adapter.py", "source", "Entry"), new(EngineReferenceToolkit.DependencyPath, "runtime", "Pinned Python")];
        var package = await builder.BuildWorkspaceAsync(source, Path.Combine(root, "build"), ["*.lines"], release, fixture.Paths,
            family: new("engine", "python-fixture", "Python fixture"));
        Assert.True(package.IsPython);
        Assert.False(File.Exists(Path.Combine(package.DirectoryPath, "adapter.dll")));
        var published = await store.PublishAndActivateAsync("game", package, []);
        var loaded = store.LoadActive("game")!;
        Assert.Equal(published.Version, loaded.Version);
        var toolkit = new EngineReferenceToolkit(Path.Combine(store.RootDirectory, "engine-tools"));
        var adapter = new GeneratedGameAdapter(loaded, new(Path.Combine(root, "absent-dotnet"), "10.0.402", "10.0.13"),
            fixture.Runner, store.WorkDirectory, toolkit);
        var game = Directory.CreateDirectory(Path.Combine(root, "游戏 目录")).FullName;
        await File.WriteAllTextAsync(Path.Combine(game, "中文.lines"), "原文 日本語𠮷", new UTF8Encoding(false));
        Assert.NotNull(await adapter.ProbeAsync(game));
        Assert.Equal("原文 日本語𠮷", Assert.Single((await adapter.ExtractAsync(game)).Units).SourceText);
        await File.AppendAllTextAsync(Path.Combine(loaded.DirectoryPath, loaded.EntryFile), "\n# changed");
        await Assert.ThrowsAsync<GeneratedAdapterException>(() => adapter.ExtractAsync(game));
    }

    [Fact]
    public async Task SharedPythonBridgeCompilesWithOnlyFrameworkReferences()
    {
        var source = Path.Combine(root, "src");
        Directory.CreateDirectory(source);
        await File.WriteAllTextAsync(Path.Combine(source, "EngineReferencePython.cs"), EnginePythonBridge.Source);
        await File.WriteAllTextAsync(Path.Combine(source, "Program.cs"),
            "using System; Console.WriteLine(typeof(EngineReferencePython).Name);");
        var toolchain = FindToolchain();
        var runner = new AdapterProcessRunner();
        var package = await new GeneratedAdapterCompiler(toolchain, runner).CompileWorkspaceAsync(source,
            Path.Combine(root, "package"), ["**/*.dat"], releaseFiles:
            [new("src/Program.cs", "source", "Entry"), new("src/EngineReferencePython.cs", "source", "Python bridge")]);
        var result = await runner.RunAsync(new(toolchain.ExecutablePath, [Path.Combine(package.DirectoryPath, "adapter.dll")], root), default);
        Assert.Equal(0, result.ExitCode);
        Assert.Equal("EngineReferencePython", result.StandardOutput.Trim());
    }

    [Fact]
    public async Task ComposedPythonEntryCallsBuiltinBranchWithUnicodeJsonAndCorrectDotnetArguments()
    {
        using var fixture = new EngineArchiveFixture();
        var source = Directory.CreateDirectory(Path.Combine(root, "src")).FullName;
        Directory.CreateDirectory(Path.Combine(source, "tools"));
        await File.WriteAllTextAsync(Path.Combine(source, "EngineReferencePython.cs"), EnginePythonBridge.Source);
        await File.WriteAllTextAsync(Path.Combine(source, "Program.cs"), """
            using System.Text.Json.Nodes;
            return EngineReferencePython.Run("tools/compose.py", args,
                json => JsonNode.Parse(json)!.ToJsonString());
            """);
        await File.WriteAllTextAsync(Path.Combine(source, "tools/compose.py"), """
            import json, subprocess, sys
            request = json.load(sys.stdin)
            payload = {'text': request['text']}
            result = subprocess.run(request['builtinCommand'], input=json.dumps(payload, ensure_ascii=False),
                                    encoding='utf-8', stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            if result.returncode != 0: raise RuntimeError(result.stderr)
            assert json.loads(result.stdout) == payload
            print(result.stdout)
            """, new UTF8Encoding(false));
        var toolchain = FindToolchain();
        var runner = new AdapterProcessRunner();
        var package = await new GeneratedAdapterCompiler(toolchain, runner).CompileWorkspaceAsync(source,
            Path.Combine(root, "composed-package"), ["*.scn"], releaseFiles:
            [new("src/Program.cs", "source", "Entry"), new("src/EngineReferencePython.cs", "source", "Bridge"),
             new("tools/compose.py", "runtime", "Python entry")]);
        const string text = "中文 日本語𠮷\\n保留控制符";
        var request = JsonSerializer.Serialize(new { text, toolPaths = new { python = fixture.Paths.Python, engineLibrary = fixture.Paths.EngineLibrary } });
        var result = await runner.RunAsync(new(toolchain.ExecutablePath, [Path.Combine(package.DirectoryPath, "adapter.dll")],
            root, request), default);
        Assert.True(result.ExitCode == 0, result.StandardError);
        using var response = JsonDocument.Parse(result.StandardOutput);
        Assert.Equal(text, response.RootElement.GetProperty("text").GetString());
    }

    [Fact]
    public async Task PythonAnalysisUsesSharedInterpreterWithUnicodeIoWithoutCompilingAdapterSources()
    {
        using var fixture = new EngineArchiveFixture();
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        await fixture.PrepareSharedToolsAsync(store.RootDirectory);
        var work = Directory.CreateDirectory(Path.Combine(root, "work")).FullName;
        var source = Directory.CreateDirectory(Path.Combine(root, "src")).FullName;
        await File.WriteAllTextAsync(Path.Combine(source, "Program.cs"), "Invalid adapter source must not be compiled for analysis.");
        var script = Path.Combine(work, "probe.py");
        await File.WriteAllTextAsync(script, """
            import sys
            from pathlib import Path
            print(sys.argv[1] + ':' + sys.stdin.read())
            print('诊断', file=sys.stderr)
            Path('result.txt').write_text('已运行', encoding='utf-8')
            sys.exit(7)
            """, new UTF8Encoding(false));
        var compiler = new GeneratedAdapterCompiler(new(Path.Combine(root, "absent-dotnet"), "10.0.402", "10.0.13"), fixture.Runner);
        var tools = new AdaptationTools(new("analysis", root, []), new(), root, compiler, store);
        var result = await tools.ExecuteAsync(new("analysis", "run_engine_python", """
            {"script":"work/probe.py","arguments":["参数"],"stdin":"输入","workingDirectory":"work"}
            """), default);
        using var parsed = JsonDocument.Parse(result);
        var output = parsed.RootElement.GetProperty("result");
        Assert.Equal(7, output.GetProperty("exitCode").GetInt32());
        Assert.Equal("参数:输入", output.GetProperty("stdout").GetString()!.Trim());
        Assert.Equal("诊断", output.GetProperty("stderr").GetString()!.Trim());
        Assert.Equal("已运行", await File.ReadAllTextAsync(Path.Combine(work, "result.txt")));
        Assert.True(File.Exists(script));
        Assert.False(Directory.Exists(Path.Combine(root, "build")));
        Assert.Empty(Directory.EnumerateDirectories(work));
        Assert.DoesNotContain(AdaptationTools.Definitions, tool => tool.Name == "run_csharp");
    }

    [Theory]
    [InlineData(".cs", "utf8")]
    [InlineData(".py", "utf8")]
    [InlineData(".ps1", "utf8")]
    [InlineData(".md", "utf8")]
    [InlineData(".json", "utf8")]
    [InlineData(".csproj", "utf8")]
    [InlineData(".rsp", "utf8")]
    [InlineData(".txt", "utf8")]
    [InlineData(".dat", "hex")]
    public async Task ReadFileUsesUtf8ForSourceFilesAndKeepsUnknownEncodingsAsHex(string extension, string expectedEncoding)
    {
        var work = Path.Combine(root, "work");
        Directory.CreateDirectory(work);
        await File.WriteAllTextAsync(Path.Combine(work, "file" + extension), "中文", new UTF8Encoding(false));
        var tools = AnalysisTools(new(FindToolchain(), new AdapterProcessRunner()));
        var result = await tools.ExecuteAsync(new("read", "read_file", JsonSerializer.Serialize(new
            { root = "workspace", path = "work/file" + extension })), CancellationToken.None);
        using var parsed = JsonDocument.Parse(result);
        var output = parsed.RootElement.GetProperty("result");
        Assert.Equal(expectedEncoding, output.GetProperty("encoding").GetString());
        Assert.Equal(expectedEncoding == "utf8" ? "中文" : Convert.ToHexString(Encoding.UTF8.GetBytes("中文")),
            output.GetProperty("content").GetString());
        if (expectedEncoding == "utf8") Assert.Contains("中文", result);
    }

    private AdaptationTools AnalysisTools(GeneratedAdapterCompiler compiler, AdaptationSession? session = null) =>
        new(new("analysis", root, []), session ?? new(), root, compiler, new(Path.Combine(root, "store")));

    [Fact]
    public async Task DiscoveryReportsMissingResourcesBeforeRoundtripAndRechecksBeforePublication()
    {
        var source = Path.Combine(root, "src");
        Directory.CreateDirectory(Path.Combine(source, "tools"));
        await File.WriteAllTextAsync(Path.Combine(source, "Program.cs"), FixtureProgram);
        await File.WriteAllTextAsync(Path.Combine(source, "Records.cs"), FixtureRecords);
        await File.WriteAllTextAsync(Path.Combine(source, "tools", "font.ini"), "font fixture");
        var game = Path.Combine(root, "game");
        Directory.CreateDirectory(game);
        await File.WriteAllTextAsync(Path.Combine(game, "first.lines"), "AVG_LINE_V1\nFirst");
        await File.WriteAllTextAsync(Path.Combine(game, "second.lines"), "AVG_LINE_V1\nSecond");
        var session = new AdaptationSession();
        var tools = new AdaptationTools(new("discovery", game, []), session, root,
            new(FindToolchain(), new AdapterProcessRunner()), new(Path.Combine(root, "store")));
        await tools.ExecuteAsync(new("stage", "stage_resources", """{"files":["first.lines"]}"""), default);
        const string options = """{"kind":"template","key":"fixture-lines","name":"Fixture lines","resourcePatterns":["*.lines"],"releaseFiles":[{"path":"src/Program.cs","role":"source","purpose":"Entry"},{"path":"src/Records.cs","role":"source","purpose":"Parser types"},{"path":"tools/font.ini","role":"asset","purpose":"Font configuration"}]}""";
        using (var failed = JsonDocument.Parse(await tools.ExecuteAsync(new("test", "test_adapter", options), default)))
        {
            var result = failed.RootElement.GetProperty("result");
            Assert.False(result.GetProperty("passed").GetBoolean());
            Assert.Equal("discovery", result.GetProperty("stage").GetString());
            Assert.Contains("second.lines", result.GetProperty("diagnostics")[0].GetString());
            Assert.Null(session.VerifiedPackage);
        }
        await tools.ExecuteAsync(new("stage", "stage_resources", """{"files":["second.lines"]}"""), default);
        using (var probe = JsonDocument.Parse(await tools.ExecuteAsync(new("probe", "test_adapter", options[..^1] + ",\"discoveryOnly\":true}"), default)))
        {
            Assert.True(probe.RootElement.GetProperty("result").GetProperty("passed").GetBoolean());
            Assert.Null(session.VerifiedPackage);
        }
        await Assert.ThrowsAsync<InvalidDataException>(() => tools.ExecuteAsync(new("finish", "finish", """{"summary":"unverified"}"""), default));
        using (var full = JsonDocument.Parse(await tools.ExecuteAsync(new("test", "test_adapter", options), default)))
            Assert.True(full.RootElement.GetProperty("result").GetProperty("passed").GetBoolean(), full.RootElement.ToString());
        Assert.NotNull(session.VerifiedPackage);
        await File.WriteAllTextAsync(Path.Combine(game, "third.lines"), "AVG_LINE_V1\nThird");
        var error = await Assert.ThrowsAsync<InvalidDataException>(() => tools.ExecuteAsync(new("finish", "finish", """{"summary":"validated"}"""), default));
        Assert.Contains("third.lines", error.Message);
        Assert.Null(tools.PublishedAdapter);
    }

    [Fact]
    public async Task ProbeReadsFooterAndIndexLocallyWithoutCopyingMediaPayloads()
    {
        var source = Path.Combine(root, "src");
        Directory.CreateDirectory(source);
        await File.WriteAllTextAsync(Path.Combine(source, "Program.cs"), """
            using System;
            using System.IO;
            using System.Linq;
            using System.Text;
            using System.Text.Json;
            using var input = JsonDocument.Parse(Console.In.ReadToEnd());
            var r = input.RootElement;
            var files = r.GetProperty("probeFiles").EnumerateArray().ToArray();
            foreach (var file in files)
                if (new FileInfo(Path.Combine(r.GetProperty("sourceDirectory").GetString()!, file.GetProperty("path").GetString()!)).Length > 65536)
                    throw new Exception("Probe copied a complete payload.");
            var ranges = r.GetProperty("probeRanges").EnumerateArray().ToArray();
            object response;
            if (ranges.Length == 0)
                response = new { protocolVersion = 1, success = true, probeReads = files.Select(f => new {
                    path = f.GetProperty("path").GetString(), offset = f.GetProperty("length").GetInt64() - 16, length = 16 }) };
            else if (ranges.Length == files.Length)
                response = new { protocolVersion = 1, success = true, probeReads = ranges.Select(f => {
                    var footer = File.ReadAllBytes(f.GetProperty("dataPath").GetString()!);
                    return new { path = f.GetProperty("path").GetString(), offset = BitConverter.ToInt64(footer, 0), length = 4 };
                }) };
            else
                response = new { protocolVersion = 1, success = true, probe = new { engineId = "footer-index", adapterId = "fixture",
                    adapterVersion = "1", description = "Footer indexed scripts", warnings = Array.Empty<string>(),
                    requiredFiles = ranges.Where(f => f.GetProperty("length").GetInt32() == 4 &&
                        File.ReadAllText(f.GetProperty("dataPath").GetString()!) == "TEXT").Select(f => f.GetProperty("path").GetString()).ToArray() } };
            Console.Write(JsonSerializer.Serialize(response));
            """);
        var compiler = new GeneratedAdapterCompiler(FindToolchain(), new AdapterProcessRunner());
        var package = await compiler.CompileWorkspaceAsync(source, Path.Combine(root, "build"), ["*.pack"],
            family: new("template", "footer-index", "Footer indexed scripts"), releaseFiles: [new("src/Program.cs", "source", "Parser")]);
        var game = Path.Combine(root, "game");
        Directory.CreateDirectory(game);
        foreach (var (name, size, kind) in new[] { ("media.pack", 8 * 1024 * 1024, "DATA"), ("scripts.pack", 128 * 1024, "TEXT") })
        {
            var bytes = new byte[size];
            Encoding.ASCII.GetBytes(kind).CopyTo(bytes, size - 256);
            BitConverter.GetBytes((long)size - 256).CopyTo(bytes, size - 16);
            await File.WriteAllBytesAsync(Path.Combine(game, name), bytes);
        }
        var hash = Hash(Path.Combine(game, "media.pack"));
        var adapter = new GeneratedGameAdapter(package, compiler.Toolchain, compiler.Runner, Path.Combine(root, "runs"));
        var probe = await adapter.ProbeAsync(game);
        Assert.Equal("scripts.pack", Assert.Single(probe!.RequiredFiles));
        var samples = Path.Combine(root, "samples");
        Directory.CreateDirectory(samples);
        File.Copy(Path.Combine(game, "scripts.pack"), Path.Combine(samples, "scripts.pack"));
        Assert.Equal(probe.RequiredFiles, (await adapter.ProbeAsync(samples))!.RequiredFiles);
        Assert.Equal(hash, Hash(Path.Combine(game, "media.pack")));
        Assert.Empty(Directory.EnumerateFileSystemEntries(Path.Combine(root, "runs")));
    }

    [Theory]
    [InlineData("outside")]
    [InlineData("bounds")]
    [InlineData("oversized")]
    [InlineData("repeated")]
    [InlineData("modified")]
    public async Task ProbeRejectsInvalidRangesAndModifiedInputs(string mode)
    {
        var source = Path.Combine(root, "src");
        Directory.CreateDirectory(source);
        await File.WriteAllTextAsync(Path.Combine(source, "Program.cs"), "System.Console.WriteLine(\"unused\");");
        var compiler = new GeneratedAdapterCompiler(FindToolchain(), new AdapterProcessRunner());
        var package = await compiler.CompileWorkspaceAsync(source, Path.Combine(root, "build"), ["*.bin"],
            family: new("template", "range-validation", "Range validation"), releaseFiles: [new("src/Program.cs", "source", "Parser")]);
        var game = Path.Combine(root, "game");
        Directory.CreateDirectory(game);
        var original = new byte[128 * 1024];
        await File.WriteAllBytesAsync(Path.Combine(game, "payload.bin"), original);
        var adapter = new GeneratedGameAdapter(package, compiler.Toolchain, new InvalidRangeRunner(mode), Path.Combine(root, "runs"));
        await Assert.ThrowsAsync<GeneratedAdapterException>(() => adapter.ProbeAsync(game));
        Assert.Equal(original, await File.ReadAllBytesAsync(Path.Combine(game, "payload.bin")));
    }

    private sealed class InvalidRangeRunner(string mode) : IAdapterProcessRunner
    {
        public Task<AdapterProcessResult> RunAsync(AdapterProcessRequest request, CancellationToken cancellationToken)
        {
            using var document = JsonDocument.Parse(request.StandardInput);
            var ranges = document.RootElement.GetProperty("probeRanges");
            if (mode == "modified" && ranges.GetArrayLength() > 0)
                File.WriteAllText(ranges[0].GetProperty("dataPath").GetString()!, "changed");
            var response = new { protocolVersion = 1, success = true, probeReads = new[] { new
            {
                path = mode == "outside" ? "../outside.bin" : "payload.bin",
                offset = mode == "bounds" ? 128 * 1024 : 0, length = mode == "oversized" ? 1024 * 1024 + 1 : 16
            } } };
            return Task.FromResult(new AdapterProcessResult(0, JsonSerializer.Serialize(response), ""));
        }
    }

    [Fact]
    public async Task FullFileProbeFailureExplainsTruncatedInputContract()
    {
        var compiler = new GeneratedAdapterCompiler(FindToolchain(), new AdapterProcessRunner());
        var source = Path.Combine(root, "src");
        Directory.CreateDirectory(source);
        await File.WriteAllTextAsync(Path.Combine(source, "Program.cs"), """
            using System;
            using System.IO;
            using System.Text.Json;
            using var request = JsonDocument.Parse(Console.In.ReadToEnd());
            var path = Path.Combine(request.RootElement.GetProperty("sourceDirectory").GetString(), "payload.bin");
            using var stream = File.OpenRead(path);
            stream.Position = 100000;
            try { stream.ReadExactly(new byte[4]); }
            catch (EndOfStreamException) { Console.Error.WriteLine("probe payload is incomplete"); return 1; }
            return 0;
            """);
        var package = await compiler.CompileWorkspaceAsync(source, Path.Combine(root, "build"), ["*.bin"], family: new("template", "probe-contract", "Probe contract"),
            releaseFiles: [new("src/Program.cs", "source", "Parser")]);
        var game = Path.Combine(root, "game");
        Directory.CreateDirectory(game);
        await File.WriteAllBytesAsync(Path.Combine(game, "payload.bin"), new byte[120000]);
        var adapter = new GeneratedGameAdapter(package, compiler.Toolchain, compiler.Runner, Path.Combine(root, "runs"));
        var error = await Assert.ThrowsAsync<GeneratedAdapterException>(() => adapter.ProbeAsync(game));
        Assert.Contains("65536", error.Message);
        Assert.Contains("probeFiles.length", error.Message);
        Assert.Contains("probeReads", error.Message);
    }

    [Fact]
    public async Task PinnedCompilerBuildsPortableMultiSourceTemplateAndVerifiesLargeRenamedResources()
    {
        var source = Path.Combine(root, "src");
        Directory.CreateDirectory(Path.Combine(source, "tools"));
        await File.WriteAllTextAsync(Path.Combine(source, "Program.cs"), FixtureProgram);
        await File.WriteAllTextAsync(Path.Combine(source, "Records.cs"), FixtureRecords);
        await File.WriteAllTextAsync(Path.Combine(source, "tools", "font.ini"), "charset=gbk");
        await File.WriteAllTextAsync(Path.Combine(source, "tools", "sources.json"), "{}");
        await File.WriteAllTextAsync(Path.Combine(source, "UnusedProbe.cs"), "not a compilable adapter source");
        await File.WriteAllTextAsync(Path.Combine(source, "tools", "unused.log"), "analysis output");
        await File.WriteAllTextAsync(Path.Combine(source, "unused.csproj"), "must not execute or publish");
        var toolchain = FindToolchain();
        var runner = new RecordingRunner();
        var compiler = new GeneratedAdapterCompiler(toolchain, runner);
        var family = new AdapterFamily("template", "fixture-lines", "Fixture line scripts");
        AdapterReleaseFile[] releaseFiles = [new("src/Program.cs", "source", "Entry"), new("src/Records.cs", "source", "Parser types"),
            new("tools/font.ini", "asset", "Font configuration"), new("tools/sources.json", "license", "Dependency provenance")];
        var package = await compiler.CompileWorkspaceAsync(source, Path.Combine(root, "build"), ["**/*.lines", "**/*.pad"], family: family, releaseFiles: releaseFiles);
        var repeated = await compiler.CompileWorkspaceAsync(source, Path.Combine(root, "repeated"), ["**/*.lines", "**/*.pad"], family: family, releaseFiles: releaseFiles.Reverse().ToArray());
        Assert.Equal(package.Version, repeated.Version);
        Assert.Equal(3, package.ManifestVersion);
        Assert.Empty(package.ResourceFiles);
        Assert.Contains("src/Records.cs", package.PackageFiles!.Keys);
        Assert.Contains("tools/font.ini", package.PackageFiles.Keys);
        Assert.DoesNotContain("src/unused.csproj", package.PackageFiles.Keys);
        Assert.DoesNotContain("src/UnusedProbe.cs", package.PackageFiles.Keys);
        Assert.DoesNotContain("tools/unused.log", package.PackageFiles.Keys);

        var game = Path.Combine(root, "first-game");
        Directory.CreateDirectory(game);
        var lines = Enumerable.Range(0, 40_001).Select(index => "Dialogue " + index + " " + new string('x', 80)).ToArray();
        await File.WriteAllTextAsync(Path.Combine(game, "first.lines"), "AVG_LINE_V1\n" + string.Join('\n', lines), new UTF8Encoding(false));
        // The payload is deliberately larger than the previous per-resource staging limit.
        await using (var padding = File.Create(Path.Combine(game, "archive.pad"))) padding.SetLength(33L * 1024 * 1024);
        var originalHash = Hash(Path.Combine(game, "first.lines"));
        var adapter = new GeneratedGameAdapter(package, toolchain, runner, Path.Combine(root, "runs"));
        var verification = await new GeneratedAdapterVerifier().VerifyAsync(adapter, game, Path.Combine(root, "verification"));
        Assert.True(verification.Passed, string.Join('\n', verification.Diagnostics));
        Assert.True(runner.SawTruncatedHeader);
        Assert.True(runner.SawResponseAboveFourMiB);
        Assert.True(runner.SawFileTranslations);
        Assert.Equal(originalHash, Hash(Path.Combine(game, "first.lines")));

        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var published = await store.PublishAndActivateAsync("first", package, verification.Diagnostics);
        var loaded = new LocalAdapterPackageStore(store.RootDirectory).LoadActive("first")!;
        Assert.Equal(package.SourceSha256, loaded.SourceSha256);
        Assert.Equal("charset=gbk", await File.ReadAllTextAsync(Path.Combine(loaded.DirectoryPath, "tools", "font.ini")));
        var other = Path.Combine(root, "second-game", "scripts");
        Directory.CreateDirectory(other);
        await File.WriteAllTextAsync(Path.Combine(other, "different.lines"), "AVG_LINE_V1\nSecond game text", new UTF8Encoding(false));
        var reusable = new GeneratedGameAdapter(loaded, toolchain, runner, Path.Combine(root, "runs"));
        var probe = await reusable.ProbeAsync(Path.GetDirectoryName(other)!);
        Assert.Equal("scripts/different.lines", Assert.Single(probe!.RequiredFiles));
        var secondVerification = await new GeneratedAdapterVerifier().VerifyAsync(reusable, Path.GetDirectoryName(other)!, Path.Combine(root, "second-verification"));
        Assert.True(secondVerification.Passed, string.Join('\n', secondVerification.Diagnostics));
        await store.PublishAndActivateAsync("second", loaded, secondVerification.Diagnostics);
        Assert.Single(store.ListReusablePackages());
        await File.AppendAllTextAsync(Path.Combine(published.DirectoryPath, "tools", "font.ini"), "tampered");
        Assert.Throws<GeneratedAdapterException>(() => store.LoadActive("first"));
    }

    [Fact]
    public async Task WhitespaceSlotsArePreservedWhileVisibleRepeatedTextKeepsIndependentIdentities()
    {
        var source = Path.Combine(root, "src");
        Directory.CreateDirectory(Path.Combine(source, "tools"));
        await File.WriteAllTextAsync(Path.Combine(source, "Program.cs"), FixtureProgram);
        await File.WriteAllTextAsync(Path.Combine(source, "Records.cs"), FixtureRecords);
        await File.WriteAllTextAsync(Path.Combine(source, "tools", "font.ini"), "charset=gbk");
        var toolchain = FindToolchain();
        var runner = new AdapterProcessRunner();
        var package = await new GeneratedAdapterCompiler(toolchain, runner).CompileWorkspaceAsync(source,
            Path.Combine(root, "build"), ["*.lines"], family: new("template", "spacing-fixture", "Spacing fixture"), releaseFiles:
            [new("src/Program.cs", "source", "Entry"), new("src/Records.cs", "source", "Parser types"), new("tools/font.ini", "asset", "Font configuration")]);
        var game = Path.Combine(root, "game");
        Directory.CreateDirectory(game);
        var original = "AVG_LINE_V1\n  \nrepeat\n\nrepeat\n\t\u3000";
        await File.WriteAllTextAsync(Path.Combine(game, "script.lines"), original, new UTF8Encoding(false));
        var adapter = new GeneratedGameAdapter(package, toolchain, runner, Path.Combine(root, "runs"));
        var extracted = await adapter.ExtractAsync(game);
        Assert.Equal(["script.lines:2", "script.lines:4"], extracted.Units.Select(unit => unit.Id));
        Assert.All(extracted.Units, unit => Assert.Equal("repeat", unit.SourceText));

        var verification = await new GeneratedAdapterVerifier().VerifyAsync(adapter, game, Path.Combine(root, "verification"));
        Assert.True(verification.Passed, string.Join('\n', verification.Diagnostics));
        var patch = Path.Combine(root, "partial");
        await adapter.BuildPatchAsync(game, new Dictionary<string, string> { ["script.lines:4"] = "中文译文" }, patch);
        Assert.Equal("AVG_LINE_V1\n  \nrepeat\n\n中文译文\n\t\u3000", await File.ReadAllTextAsync(Path.Combine(patch, "script.lines")));
        Assert.Equal(original, await File.ReadAllTextAsync(Path.Combine(game, "script.lines")));
    }

    [Theory]
    [InlineData("duplicate")]
    [InlineData("path")]
    [InlineData("null")]
    [InlineData("all-whitespace")]
    public async Task WhitespaceFilteringCannotHideMalformedRecords(string problem)
    {
        var packageDirectory = Path.Combine(root, "package");
        Directory.CreateDirectory(packageDirectory);
        await File.WriteAllTextAsync(Path.Combine(packageDirectory, "Program.cs"), "// protocol fixture");
        await File.WriteAllBytesAsync(Path.Combine(packageDirectory, "adapter.dll"), [1, 2, 3]);
        await File.WriteAllTextAsync(Path.Combine(packageDirectory, "adapter.runtimeconfig.json"), "{}");
        var package = new GeneratedAdapterPackage("template.invalid-extraction", "one", packageDirectory, ["script.dat"],
            Hash(Path.Combine(packageDirectory, "Program.cs")), Hash(Path.Combine(packageDirectory, "adapter.dll")),
            new("template", "invalid-extraction", "Extraction fixture"));
        var game = Path.Combine(root, "game");
        Directory.CreateDirectory(game);
        await File.WriteAllTextAsync(Path.Combine(game, "script.dat"), "original");
        var valid = new TranslationUnit("visible", "script.dat", "entry:0", "visible text", "fixture");
        var spacing = new TranslationUnit("spacing", "script.dat", "entry:1", "  ", "fixture");
        TranslationUnit[] units = problem switch
        {
            "duplicate" => [valid, spacing with { Id = valid.Id }],
            "path" => [valid, spacing with { ResourcePath = "../outside.dat" }],
            "null" => [valid, spacing with { SourceText = null! }],
            _ => [spacing]
        };
        var adapter = new GeneratedGameAdapter(package, new(root, "10.0.402", "10.0.13"),
            new ExtractionRunner(new(units, [])), Path.Combine(root, "runs"));
        if (problem == "path")
        {
            await Assert.ThrowsAsync<InvalidDataException>(() => adapter.ExtractAsync(game));
            return;
        }
        var error = await Assert.ThrowsAsync<GeneratedAdapterException>(() => adapter.ExtractAsync(game));
        if (problem == "duplicate") Assert.Contains("重复的文本 ID：visible", error.Message);
        if (problem == "all-whitespace") Assert.Contains("没有可翻译内容", error.Message);
    }

    private sealed class ExtractionRunner(ExtractionResult extraction) : IAdapterProcessRunner
    {
        public Task<AdapterProcessResult> RunAsync(AdapterProcessRequest request, CancellationToken cancellationToken) =>
            Task.FromResult(new AdapterProcessResult(0, JsonSerializer.Serialize(new { protocolVersion = 1, success = true, extraction }, Json), ""));
    }

    [Fact]
    public async Task HostRejectsAnInvalidNewLaunchExecutableBeforeCopyingAnyOutput()
    {
        var packageDirectory = Path.Combine(root, "legacy");
        Directory.CreateDirectory(packageDirectory);
        await File.WriteAllTextAsync(Path.Combine(packageDirectory, "Program.cs"), "// legacy fixture");
        await File.WriteAllBytesAsync(Path.Combine(packageDirectory, "adapter.dll"), [1, 2, 3]);
        await File.WriteAllTextAsync(Path.Combine(packageDirectory, "adapter.runtimeconfig.json"), "{}");
        var package = new GeneratedAdapterPackage("template.invalid-exe", "one", packageDirectory, ["script.dat"],
            Hash(Path.Combine(packageDirectory, "Program.cs")), Hash(Path.Combine(packageDirectory, "adapter.dll")),
            new("template", "invalid-exe", "Executable fixture"));
        var resources = Path.Combine(root, "resources");
        Directory.CreateDirectory(resources);
        await File.WriteAllTextAsync(Path.Combine(resources, "script.dat"), "original");
        var output = Path.Combine(root, "patch");
        Directory.CreateDirectory(output);
        var adapter = new GeneratedGameAdapter(package, new(root, "10.0.402", "10.0.13"), new InvalidExecutableRunner(), Path.Combine(root, "runs"));
        var error = await Assert.ThrowsAsync<GeneratedAdapterException>(() => adapter.BuildPatchAsync(resources, new Dictionary<string, string>(), output));
        Assert.Contains("static validation", error.Message);
        Assert.Empty(Directory.EnumerateFileSystemEntries(output));
        Assert.Equal("original", await File.ReadAllTextAsync(Path.Combine(resources, "script.dat")));
    }

    private static PinnedDotNetToolchain FindToolchain()
    {
        var directory = new DirectoryInfo(AppContext.BaseDirectory);
        while (directory is not null && !File.Exists(Path.Combine(directory.FullName, "global.json"))) directory = directory.Parent;
        Assert.NotNull(directory);
        var toolchain = Path.Combine(directory!.FullName, ".tools", "dotnet");
        Assert.True(File.Exists(Path.Combine(toolchain, "dotnet.exe")), "The fixture requires the repository's pinned .NET toolchain.");
        return new(toolchain, "10.0.402", "10.0.13");
    }

    private sealed class RecordingRunner : IAdapterProcessRunner
    {
        private readonly AdapterProcessRunner inner = new();
        public bool SawTruncatedHeader { get; private set; }
        public bool SawResponseAboveFourMiB { get; private set; }
        public bool SawFileTranslations { get; private set; }

        public async Task<AdapterProcessResult> RunAsync(AdapterProcessRequest request, CancellationToken cancellationToken)
        {
            if (request.StandardInput.Length == 0) return await inner.RunAsync(request, cancellationToken);
            using var parsed = JsonDocument.Parse(request.StandardInput);
            var input = parsed.RootElement;
            var operation = input.GetProperty("operation").GetString();
            if (operation == "probe")
                foreach (var file in input.GetProperty("probeFiles").EnumerateArray())
                {
                    var length = file.GetProperty("length").GetInt64();
                    var bytes = file.GetProperty("headerBytes").GetInt64();
                    Assert.InRange(bytes, 0, 65536);
                    Assert.Equal(bytes, new FileInfo(Path.Combine(input.GetProperty("sourceDirectory").GetString()!, file.GetProperty("path").GetString()!)).Length);
                    SawTruncatedHeader |= length > bytes;
                }
            else
            {
                Assert.Equal(TimeSpan.FromMinutes(10), request.Timeout);
                Assert.Equal(8L * 1024 * 1024, request.MaximumOutputBytes);
                if (operation == "build") SawFileTranslations |= File.Exists(input.GetProperty("translationsFile").GetString());
            }
            var result = await inner.RunAsync(request, cancellationToken);
            var responsePath = input.GetProperty("responseFile").GetString()!;
            if (File.Exists(responsePath)) SawResponseAboveFourMiB |= new FileInfo(responsePath).Length > 4 * 1024 * 1024;
            return result;
        }
    }

    private sealed class InvalidExecutableRunner : IAdapterProcessRunner
    {
        public Task<AdapterProcessResult> RunAsync(AdapterProcessRequest request, CancellationToken cancellationToken)
        {
            using var parsed = JsonDocument.Parse(request.StandardInput);
            var output = parsed.RootElement.GetProperty("outputDirectory").GetString()!;
            File.WriteAllText(Path.Combine(output, "script.dat"), "changed");
            File.WriteAllBytes(Path.Combine(output, "Chinese.exe"), [0, 1, 2]);
            var response = new { protocolVersion = 1, success = true,
                build = new AdapterBuildResult(["script.dat", "Chinese.exe"], [], "Chinese.exe") };
            return Task.FromResult(new AdapterProcessResult(0, JsonSerializer.Serialize(response, Json), ""));
        }
    }

    private static string Hash(string path)
    {
        using var stream = File.OpenRead(path);
        return Convert.ToHexStringLower(SHA256.HashData(stream));
    }

    public void Dispose() { if (Directory.Exists(root)) Directory.Delete(root, true); }

    private const string FixtureRecords = """
        public sealed record Unit(string Id, string ResourcePath, string Locator, string SourceText,
            string Context, string? Speaker = null, string Kind = "dialogue");
        """;

    private const string FixtureProgram = """
        using System;
        using System.Collections.Generic;
        using System.IO;
        using System.Linq;
        using System.Text;
        using System.Text.Json;
        using System.Threading.Tasks;

        public static class Program
        {
            private static readonly JsonSerializerOptions Json = new(JsonSerializerDefaults.Web);
            public static async Task Main()
            {
                using var input = JsonDocument.Parse(await Console.In.ReadToEndAsync());
                var request = input.RootElement;
                var operation = request.GetProperty("operation").GetString();
                var source = request.GetProperty("sourceDirectory").GetString()!;
                var output = request.GetProperty("outputDirectory").GetString()!;
                var files = request.GetProperty("resourceFiles").EnumerateArray().Select(item => item.GetString()!).ToArray();
                object response;
                if (operation == "probe")
                {
                    var supported = files.Where(file => file.EndsWith(".lines", StringComparison.OrdinalIgnoreCase))
                        .All(file => { using var reader = File.OpenText(Path.Combine(source, file)); return reader.ReadLine() == "AVG_LINE_V1"; });
                    response = new { protocolVersion = 1, success = true, probe = supported ? new {
                        engineId = "fixture", adapterId = "fixture", adapterVersion = "1", description = "Fixture line scripts", requiredFiles = files,
                        warnings = Array.Empty<string>(), originalExecutable = (string?)null } : null };
                }
                else if (operation == "extract")
                {
                    var units = new List<Unit>();
                    foreach (var file in files.Where(file => file.EndsWith(".lines", StringComparison.OrdinalIgnoreCase)))
                    {
                        var lines = await File.ReadAllLinesAsync(Path.Combine(source, file));
                        for (var index = 1; index < lines.Length; index++)
                            units.Add(new(file + ":" + index, file, "line:" + index, lines[index], "fixture"));
                    }
                    response = new { protocolVersion = 1, success = true, extraction = new { units, warnings = Array.Empty<string>() } };
                }
                else if (operation == "build")
                {
                    using var translationsInput = File.OpenRead(request.GetProperty("translationsFile").GetString()!);
                    var translations = (await JsonSerializer.DeserializeAsync<Dictionary<string, string>>(translationsInput, Json))!;
                    foreach (var file in files)
                    {
                        var target = Path.Combine(output, file);
                        Directory.CreateDirectory(Path.GetDirectoryName(target)!);
                        if (file.EndsWith(".lines", StringComparison.OrdinalIgnoreCase))
                        {
                            var lines = await File.ReadAllLinesAsync(Path.Combine(source, file));
                            for (var index = 1; index < lines.Length; index++)
                                if (translations.TryGetValue(file + ":" + index, out var text)) lines[index] = text;
                            await File.WriteAllTextAsync(target, string.Join('\n', lines), new UTF8Encoding(false));
                        }
                        else File.Copy(Path.Combine(source, file), target);
                    }
                    File.Copy(Path.Combine(AppContext.BaseDirectory, "tools", "font.ini"), Path.Combine(output, "font.ini"));
                    response = new { protocolVersion = 1, success = true, build = new { files = files.Append("font.ini"), warnings = Array.Empty<string>(), launchExecutable = (string?)null } };
                }
                else response = new { protocolVersion = 1, success = true, validation = new { passed = true,
                    issues = Array.Empty<object>(), runtimeStatus = "passed" } };
                await using var responseStream = File.Create(request.GetProperty("responseFile").GetString()!);
                await JsonSerializer.SerializeAsync(responseStream, response, Json);
            }
        }
        """;
}
