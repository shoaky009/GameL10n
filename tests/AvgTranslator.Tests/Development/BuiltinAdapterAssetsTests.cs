using System.IO.Compression;
using System.Reflection;
using System.Text;
using System.Text.Json;
using AvgTranslator.Core.Adapters;
using AvgTranslator.Core.Development;
using AvgTranslator.VnTextPatch;

namespace AvgTranslator.Tests.Development;

public sealed class BuiltinAdapterAssetsTests : IDisposable
{
    private readonly string root = Path.Combine(Path.GetTempPath(), "avg-builtin-assets-" + Guid.NewGuid().ToString("N"));
    private static readonly JsonSerializerOptions Json = new(JsonSerializerDefaults.Web);

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
        await assets.CopyAsync("Renpy", workspace, false);
        string bridge = Path.Combine(workspace, "src/BuiltinScriptParser.cs");
        byte[] original = await File.ReadAllBytesAsync(bridge);
        Assert.True(File.Exists(Path.Combine(workspace, "src/tools/vntextpatch/AvgTranslator.VnTextPatch.dll")));
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
        var package = await compiler.CompileWorkspaceAsync(Path.Combine(workspace, "src"), Path.Combine(root, "compiled"), ["**/*"]);
        string resources = Directory.CreateDirectory(Path.Combine(root, "resources")).FullName;
        string file = format == "Renpy" ? "scene.rpy" : "scene.ks";
        string script = format == "Renpy" ? "label start:\n    \"こんにちは。\"\n" : "*start\r\nこんにちは。[p]\r\n";
        await File.WriteAllTextAsync(Path.Combine(resources, file), script, format == "Renpy" ? new UTF8Encoding(false) : Encoding.Unicode);
        async Task<AdapterProcessResult> Invoke(object request) => await runner.RunAsync(new(toolchain.ExecutablePath,
            [Path.Combine(package.DirectoryPath, "adapter.dll")], package.DirectoryPath, JsonSerializer.Serialize(request, Json)), default);
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
        File.Copy(typeof(Worker).Assembly.Location, Path.Combine(assets, "AvgTranslator.VnTextPatch.dll"));
        string vendor = Path.Combine(Repository(), "third_party/VNTextPatch");
        File.Copy(Path.Combine(vendor, "LICENSE"), Path.Combine(assets, "VNTextPatch.LICENSE"));
        File.Copy(Path.Combine(vendor, "NOTICE.md"), Path.Combine(assets, "VNTextPatch.NOTICE.md"));
        using var archive = ZipFile.Open(Path.Combine(assets, "sources.zip"), ZipArchiveMode.Create);
        foreach (string path in Directory.EnumerateFiles(vendor, "*.cs", SearchOption.AllDirectories))
            archive.CreateEntryFromFile(path, "vendor/" + Path.GetRelativePath(vendor, path).Replace('\\', '/'));
        foreach (string name in new[] { "Worker.cs", "Protocol.cs" })
            archive.CreateEntryFromFile(Path.Combine(Repository(), "tools/AvgTranslator.VnTextPatch", name), "worker/" + name);
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
