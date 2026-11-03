using System.IO.Compression;
using System.Reflection;
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
        Assert.Contains("二进制模式", description.GetProperty("usage").GetString());
        Assert.Equal(new[] { "extract", "build", "validate" }, description.GetProperty("operations").EnumerateArray().Select(item => item.GetString()));
        Assert.NotNull(assets.ReadSource("Renpy", "generated/BuiltinScriptParser.cs"));

        var error = Assert.Throws<BuiltinAdapterAssetException>(() => assets.ReadSource("Renpy", "vendor/Scripts/RenpyScript.cs"));
        Assert.Equal(code, error.Code);
        Assert.Equal("sources.zip", error.AssetName);
        Assert.Throws<ArgumentException>(() => assets.Describe("unknown-format"));
        string editableWorkspace = Path.Combine(root, "editable");
        Assert.Equal(code, (await Assert.ThrowsAsync<BuiltinAdapterAssetException>(() => assets.CopyAsync("Renpy", editableWorkspace, true))).Code);
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
            Path.Combine(first, "src", BuiltinAdapterAssets.SharedDependencyPath)))!;
        var legacyFiles = dependency.Files.ToDictionary(pair => pair.Key.Replace("GameL10n.VnTextPatch.dll", "AvgTranslator.VnTextPatch.dll"), pair => pair.Value);
        var legacyDirectory = Path.Combine(shared, "vntextpatch", GeneratedAdapterResources.HashEntries(legacyFiles));
        Directory.CreateDirectory(legacyDirectory);
        foreach (var file in dependency.Files.Keys)
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
        await assets.CopyAsync("Kaguya", workspace, true);
        Assert.False(Directory.Exists(Path.Combine(workspace, "src/BuiltinVnTextPatch/reference")));
        var unrelated = JsonSerializer.SerializeToElement(assets.Describe("Renpy"), Json);
        Assert.Equal(JsonValueKind.Null, unrelated.GetProperty("engineGuidance").ValueKind);
        Assert.DoesNotContain("KaGuYa", unrelated.GetRawText());
    }

    [Fact]
    public async Task SourceIndexIsBoundedAndBinaryCopyCanUpgradeWithoutReplacingExistingSource()
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
        Assert.Contains(release, file => file.Path == "src/BuiltinScriptParser.cs" && file.Role == "source");
        Assert.Contains(release, file => file.Role == "runtime" && file.Path == BuiltinAdapterAssets.SharedDependencyPath);
        Assert.Equal(2, release.Count);
        string bridge = Path.Combine(workspace, "src/BuiltinScriptParser.cs");
        byte[] original = await File.ReadAllBytesAsync(bridge);
        Assert.False(File.Exists(Path.Combine(workspace, "src/tools/vntextpatch/GameL10n.VnTextPatch.dll")));
        Assert.True(File.Exists(Path.Combine(workspace, "src", BuiltinAdapterAssets.SharedDependencyPath)));
        Assert.False(Directory.Exists(Path.Combine(workspace, "src/BuiltinVnTextPatch")));
        var upgrade = JsonSerializer.SerializeToElement(await assets.CopyAsync("Renpy", workspace, true), Json);
        Assert.Contains("src/BuiltinScriptParser.cs", upgrade.GetProperty("reused").EnumerateArray().Select(item => item.GetString()));
        Assert.Equal(original, await File.ReadAllBytesAsync(bridge));
        Assert.True(File.Exists(Path.Combine(workspace, "src/BuiltinVnTextPatch/vendor/Scripts/RenpyScript.cs")));
        Assert.False(File.Exists(Path.Combine(workspace, "src/BuiltinVnTextPatch/vendor/Scripts/FolderScriptCollection.cs")));
        Assert.False(File.Exists(Path.Combine(workspace, "src/Program.cs")));
    }

    [Fact]
    public async Task AnyConflictingDestinationPreventsAllWrites()
    {
        var assets = CreateAssets();
        string workspace = Path.Combine(root, "workspace");
        string conflict = Path.Combine(workspace, "src/tools/vntextpatch/VNTextPatch.NOTICE.md");
        Directory.CreateDirectory(Path.GetDirectoryName(conflict)!);
        await File.WriteAllTextAsync(conflict, "Keep my modifications.");
        await Assert.ThrowsAsync<IOException>(() => assets.CopyAsync("Renpy", workspace, true));
        Assert.Equal(new[] { Path.GetFullPath(conflict) }, Directory.GetFiles(workspace, "*", SearchOption.AllDirectories));
        Assert.Equal("Keep my modifications.", await File.ReadAllTextAsync(conflict));
    }

    [Theory]
    [InlineData("Renpy", false)]
    [InlineData("Renpy", true)]
    [InlineData("KirikiriKs", true)]
    public async Task CopiedParserCompilesWithPinnedFrameworkOnlyCompilerAndWritesChinese(string format, bool editableSource)
    {
        var assets = CreateAssets();
        string workspace = Path.Combine(root, "workspace");
        await assets.CopyAsync(format, workspace, editableSource);
        await File.WriteAllTextAsync(Path.Combine(workspace, "src/Program.cs"), """
            using System;
            using System.Text;
            Console.InputEncoding = new UTF8Encoding(false);
            Console.OutputEncoding = new UTF8Encoding(false);
            try { Console.Write(BuiltinScriptParser.ExecuteJson(Console.In.ReadToEnd())); return 0; }
            catch (Exception error) { Console.Error.WriteLine(error.Message); return 3; }
            """);
        var toolchain = new PinnedDotNetToolchain(Path.Combine(Repository(), ".tools/dotnet"), "10.0.402", "10.0.13");
        var runner = new AdapterProcessRunner();
        var compiler = new GeneratedAdapterCompiler(toolchain, runner);
        var source = Path.Combine(workspace, "src");
        var releaseFiles = Directory.GetFiles(source, "*", SearchOption.AllDirectories).Select(file =>
        {
            var relative = Path.GetRelativePath(source, file).Replace('\\', '/');
            return relative.StartsWith("tools/", StringComparison.Ordinal)
                ? new AdapterReleaseFile(relative, relative.EndsWith(".dll") ? "runtime" : "license", "Selected parser dependency")
                : new AdapterReleaseFile("src/" + relative, "source", "Selected parser implementation");
        }).ToArray();
        var package = await compiler.CompileWorkspaceAsync(source, Path.Combine(root, "compiled"), ["**/*"], releaseFiles: releaseFiles);
        string resources = Directory.CreateDirectory(Path.Combine(root, "resources")).FullName;
        string file = format == "Renpy" ? "scene.rpy" : "scene.ks";
        string script = format == "Renpy" ? "label start:\n    \"こんにちは。\"\n" : "*start\r\nこんにちは。[p]\r\n";
        await File.WriteAllTextAsync(Path.Combine(resources, file), script, format == "Renpy" ? new UTF8Encoding(false) : Encoding.Unicode);
        var sharedParser = editableSource ? null : await new EngineReferenceToolkit(Path.Combine(root, "shared"), parserAssets: assets)
            .ResolveVnTextPatchAsync(source, default);
        async Task<AdapterProcessResult> Invoke(object request) => await runner.RunAsync(new(toolchain.ExecutablePath,
            [Path.Combine(package.DirectoryPath, "adapter.dll")], package.DirectoryPath, JsonSerializer.Serialize(request, Json), VnTextPatchAssemblyPath: sharedParser), default);
        var extraction = await Invoke(new { protocolVersion = 1, operation = "extract", sourceDirectory = resources, resourceFiles = new[] { file } });
        Assert.Equal(0, extraction.ExitCode);
        using var parsed = JsonDocument.Parse(extraction.StandardOutput);
        Assert.True(parsed.RootElement.GetProperty("success").GetBoolean(), extraction.StandardOutput);
        string id = Assert.Single(parsed.RootElement.GetProperty("extraction").GetProperty("units").EnumerateArray()).GetProperty("id").GetString()!;
        string output = Path.Combine(root, "patched");
        var build = await Invoke(new { protocolVersion = 1, operation = "build", sourceDirectory = resources, outputDirectory = output,
            resourceFiles = new[] { file }, translations = new Dictionary<string, string> { [id] = "你好，中文。" } });
        using var built = JsonDocument.Parse(build.StandardOutput);
        Assert.True(built.RootElement.GetProperty("success").GetBoolean(), build.StandardOutput);
        Assert.Equal(script.Replace("こんにちは。", "你好，中文。"), await File.ReadAllTextAsync(Path.Combine(output, file)));
        var probe = await Invoke(new { protocolVersion = 1, operation = "probe", sourceDirectory = resources });
        Assert.Equal(3, probe.ExitCode);
        Assert.Contains("65536", probe.StandardError);
    }

    [Theory]
    [InlineData("SilkysMes")]
    [InlineData("Renpy")]
    public async Task ReturnedLibrarySourceListCompilesProbesWithoutIncludingAdapterEntryPoint(string format)
    {
        var assets = CreateAssets();
        string workspace = Path.Combine(root, "workspace");
        var copied = JsonSerializer.SerializeToElement(await assets.CopyAsync(format, workspace, true), Json);
        var sources = copied.GetProperty("librarySourceFiles").EnumerateArray().Select(item => item.GetString()!).ToArray();
        Assert.Contains("src/BuiltinVnTextPatch/generated/GlobalUsings.cs", sources);
        Assert.Equal(Directory.GetFiles(Path.Combine(workspace, "src"), "*.cs", SearchOption.AllDirectories)
            .Select(path => Path.GetRelativePath(workspace, path).Replace('\\', '/')).Order(StringComparer.Ordinal), sources);
        await File.WriteAllTextAsync(Path.Combine(workspace, "src/Program.cs"), "This deliberately invalid adapter entry point must not be compiled.");
        Directory.CreateDirectory(Path.Combine(workspace, "work"));
        string probe = Path.Combine(workspace, "work/Probe.cs");
        await File.WriteAllTextAsync(probe, "System.Console.Write(typeof(BuiltinScriptParser).Name);");
        var compiler = new GeneratedAdapterCompiler(new(Path.Combine(Repository(), ".tools/dotnet"), "10.0.402", "10.0.13"), new AdapterProcessRunner());
        var result = await compiler.RunCSharpAsync(workspace, [probe, .. sources.Select(path => Path.Combine(workspace, path))],
            [], [], workspace, "", TimeSpan.FromSeconds(60));
        Assert.True(result.Compiled, result.Result.StandardError);
        Assert.Equal(0, result.Result.ExitCode);
        Assert.Equal("BuiltinScriptParser", result.Result.StandardOutput);
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
        string assets = Directory.CreateDirectory(Path.Combine(root, "assets")).FullName;
        File.Copy(typeof(Worker).Assembly.Location, Path.Combine(assets, "GameL10n.VnTextPatch.dll"));
        string vendor = Path.Combine(Repository(), "third_party/VNTextPatch");
        File.Copy(Path.Combine(vendor, "LICENSE"), Path.Combine(assets, "VNTextPatch.LICENSE"));
        File.Copy(Path.Combine(vendor, "NOTICE.md"), Path.Combine(assets, "VNTextPatch.NOTICE.md"));
        using var archive = ZipFile.Open(Path.Combine(assets, "sources.zip"), ZipArchiveMode.Create);
        foreach (string path in Directory.EnumerateFiles(vendor, "*.cs", SearchOption.AllDirectories))
            archive.CreateEntryFromFile(path, "vendor/" + Path.GetRelativePath(vendor, path).Replace('\\', '/'));
        foreach (string name in new[] { "Worker.cs", "Protocol.cs" })
            archive.CreateEntryFromFile(Path.Combine(Repository(), "tools/GameL10n.VnTextPatch", name), "worker/" + name);
        return new(assets);
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
