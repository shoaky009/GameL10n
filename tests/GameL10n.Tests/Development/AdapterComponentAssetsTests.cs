using System.Reflection.Metadata;
using System.Reflection.PortableExecutable;
using System.Diagnostics;
using System.Text;
using System.Text.Json;
using GameL10n.AdapterComponents;
using GameL10n.Core.Development;

namespace GameL10n.Tests.Development;

public sealed class AdapterComponentAssetsTests : IDisposable
{
    private readonly string root = Path.Combine(Path.GetTempPath(), "gamel10n-component-test-" + Guid.NewGuid().ToString("N"));
    private static readonly JsonSerializerOptions Json = new(JsonSerializerDefaults.Web);

    [Fact]
    public async Task WholePatchPlanPreservesJapaneseAndExistingMappingsRegardlessOfTextOrder()
    {
        string[] text = ["こんにちは。\ue000", "译文与简体𠮷"];
        int[] slots = [0xe003, 0xe001, 0xe000, 0xe002, 0xe004, 0xe005, 0xe006, 0xe007];
        var saved = new Dictionary<int, int> { [0xe001] = '译' };
        var first = Cp932GlyphEncoding.Plan(text, slots, ["UI\ue003"], saved);
        var second = Cp932GlyphEncoding.Plan(text.Reverse(), slots.Reverse(), ["UI\ue003"], saved);
        Assert.Equal(first.SlotToUnicode.ToArray(), second.SlotToUnicode.ToArray());
        Assert.Equal((int)'译', first.SlotToUnicode[0xe001]);
        Assert.DoesNotContain(0xe000, first.SlotToUnicode.Keys);
        Assert.DoesNotContain(0xe003, first.SlotToUnicode.Keys);
        var restored = new Cp932GlyphEncoding(first.SlotToUnicode);
        foreach (var value in text) Assert.Equal(value, restored.GetString(first.GetBytes(value)));
        Assert.Single(saved);
        using var fixture = new EngineArchiveFixture();
        var command = EngineArchiveFixture.HostCommand;
        var response = await fixture.Runner.RunAsync(new(command[0], command.Skip(1).Append("component").ToArray(), fixture.Root,
            JsonSerializer.Serialize(new { protocolVersion = 1, operation = "glyph_plan", texts = text, allowedSlots = slots,
                preservedTexts = new[] { "UI\ue003" }, map = saved }, Json)), default);
        Assert.True(response.ExitCode == 0, response.StandardError);
        using var parsed = JsonDocument.Parse(response.StandardOutput);
        var actual = parsed.RootElement.GetProperty("result").GetProperty("map").Deserialize<Dictionary<int, int>>(Json)!;
        Assert.Equal(first.SlotToUnicode.OrderBy(pair => pair.Key), actual.OrderBy(pair => pair.Key));
    }

    [Fact]
    public void MappingRejectsReservedSlotConflictsExhaustionAndUnprovenExistingSlots()
    {
        var saved = new Dictionary<int, int> { [0xe000] = '译' };
        Assert.Throws<InvalidDataException>(() => Cp932GlyphEncoding.Plan(["译\ue000"], [0xe000], [], saved));
        Assert.Throws<InvalidDataException>(() => Cp932GlyphEncoding.Plan(["译"], [0xe000], ["\ue000"], saved));
        Assert.Throws<InvalidDataException>(() => Cp932GlyphEncoding.Plan(["译"], [0xe001], [], saved));
        Assert.Throws<InvalidDataException>(() => Cp932GlyphEncoding.Plan(["译简"], [0xe000], []));
        Assert.Throws<InvalidDataException>(() => Cp932GlyphEncoding.Plan(["译"], ['A'], []));
    }

    [Fact]
    public void EncodingDoesNotSilentlyReplaceUnplannedCharactersOrMalformedUtf16()
    {
        var codec = Cp932GlyphEncoding.Plan(["译〜"], [0xe000, 0xe001, 0xe002], []);
        Assert.Equal("译〜", codec.GetString(codec.GetBytes("译〜")));
        Assert.Throws<EncoderFallbackException>(() => codec.GetBytes("𠮷"));
        Assert.Throws<EncoderFallbackException>(() => codec.GetBytes("\ud800"));
        Assert.Throws<EncoderFallbackException>(() => Cp932GlyphEncoding.Plan(["\ud800"], [0xe000], []));
        Assert.Throws<EncoderFallbackException>(() => codec.GetBytes(char.ConvertFromUtf32(codec.SlotToUnicode.Keys.First())));
        Assert.Throws<InvalidDataException>(() => new Cp932GlyphEncoding(new Dictionary<int, int>
            { [0xe000] = '译', [0xe001] = '译' }));
        Assert.Throws<InvalidDataException>(() => new Cp932GlyphEncoding(new Dictionary<int, int>
            { [0xe000] = 0xe001, [0xe001] = '译' }));
    }

    [Fact]
    public async Task CopyIsRepeatableAndConflictsLeaveEveryExistingFileUntouched()
    {
        var assets = new AdapterComponentAssets();
        var workspace = Path.Combine(root, "workspace");
        var release = new List<AdapterReleaseFile>();
        await assets.CopyAsync(["cp932-glyph-map"], workspace, releaseFiles: release);
        Assert.Equal("src/Components/Cp932GlyphEncoding.cs", Assert.Single(release).Path);
        Assert.Equal("source", release[0].Role);
        var repeat = JsonSerializer.SerializeToElement(await assets.CopyAsync(["cp932-glyph-map"], workspace), Json);
        Assert.True(repeat.GetProperty("files")[0].GetProperty("reused").GetBoolean());
        var path = Path.Combine(workspace, "src", "Components", "Cp932GlyphEncoding.cs");
        await File.AppendAllTextAsync(path, "\n// User modification\n");
        var error = await Assert.ThrowsAsync<IOException>(() => assets.CopyAsync(["qlie-sfnt-font", "cp932-glyph-map"], workspace));
        Assert.Contains("未写入文件", error.Message);
        Assert.Single(Directory.GetFiles(workspace, "*", SearchOption.AllDirectories));
        Assert.EndsWith("// User modification\n", await File.ReadAllTextAsync(path));
        Assert.False(Directory.Exists(Path.Combine(workspace, "backups")));
        Assert.Throws<InvalidDataException>(() => assets.ReadSource("cp932-glyph-map", "../QlieFont.cs"));
    }

    [Fact]
    public async Task MissingLauncherBinaryFailsBeforeCopyingAnySelectedComponents()
    {
        var assets = new AdapterComponentAssets(Path.Combine(root, "missing-assets"));
        var description = JsonSerializer.SerializeToElement(assets.Describe("qlie-japanese-font-launcher"), Json);
        Assert.False(description.GetProperty("files").EnumerateArray().Single(file => file.GetProperty("binary").GetBoolean())
            .GetProperty("available").GetBoolean());
        var workspace = Path.Combine(root, "workspace");
        await Assert.ThrowsAsync<FileNotFoundException>(() => assets.CopyAsync(["cp932-glyph-map", "qlie-japanese-font-launcher"], workspace));
        Assert.False(Directory.Exists(workspace));
    }

    [Fact]
    public async Task CopiedComponentsCompileIndependentlyAndLauncherIsDeterministicFrameworkExecutable()
    {
        var repository = Repository();
        var runner = new AdapterProcessRunner();
        var assetsDirectory = Path.Combine(root, "assets");
        var powershell = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ProgramFiles), "PowerShell", "7", "pwsh.exe");
        async Task PackageAsync()
        {
            // Packaging uses the developer build environment, not the adapter worker's restricted environment variables.
            var start = new ProcessStartInfo(powershell) { UseShellExecute = false, CreateNoWindow = true,
                WorkingDirectory = repository, RedirectStandardOutput = true, RedirectStandardError = true };
            foreach (var argument in new[] { "-NoProfile", "-NonInteractive", "-File",
                Path.Combine(repository, "scripts/package-adapter-components.ps1"), "-OutputDirectory", assetsDirectory })
                start.ArgumentList.Add(argument);
            using var process = Process.Start(start)!;
            var stdout = process.StandardOutput.ReadToEndAsync();
            var stderr = process.StandardError.ReadToEndAsync();
            try { await process.WaitForExitAsync().WaitAsync(TimeSpan.FromSeconds(30)); }
            finally { if (!process.HasExited) process.Kill(entireProcessTree: true); }
            var diagnostic = await stdout + await stderr;
            Assert.True(process.ExitCode == 0, diagnostic);
            Assert.True(File.Exists(Path.Combine(assetsDirectory, "QlieLauncher.exe")), diagnostic);
        }
        await PackageAsync();
        var launcher = Path.Combine(assetsDirectory, "QlieLauncher.exe");
        var initial = await File.ReadAllBytesAsync(launcher);
        await PackageAsync();
        Assert.Equal(initial, await File.ReadAllBytesAsync(launcher));
        using (var stream = File.OpenRead(launcher))
        using (var pe = new PEReader(stream))
        {
            Assert.Equal(Machine.I386, pe.PEHeaders.CoffHeader.Machine);
            Assert.Equal(Subsystem.WindowsGui, pe.PEHeaders.PEHeader!.Subsystem);
            var metadata = pe.GetMetadataReader();
            var references = metadata.AssemblyReferences.Select(handle => metadata.GetAssemblyReference(handle)).ToArray();
            Assert.Contains(references, reference => metadata.GetString(reference.Name) == "mscorlib" && reference.Version.Major == 4);
            Assert.DoesNotContain(references, reference => metadata.GetString(reference.Name).StartsWith("GameL10n", StringComparison.Ordinal));
            Assert.DoesNotContain(references, reference => metadata.GetString(reference.Name) == "System.Runtime");
        }
        var assets = new AdapterComponentAssets(assetsDirectory);
        var workspace = Path.Combine(root, "workspace");
        await assets.CopyAsync(["cp932-glyph-map", "qlie-sfnt-font", "system4-fnl-glyph", "qlie-japanese-font-launcher"], workspace);
        await File.WriteAllTextAsync(Path.Combine(workspace, "src/Program.cs"), """
            using System;
            using System.Collections.Generic;
            using GameL10n.AdapterComponents;
            var codec = Cp932GlyphEncoding.Plan(new[] { "译文", "日文\ue000" }, new[] { 0xe000, 0xe001, 0xe002 }, Array.Empty<string>());
            if (codec.GetString(codec.GetBytes("译文")) != "译文" || codec.SlotToUnicode.ContainsKey(0xe000)) return 1;
            Console.Write("component-ok");
            return 0;
            """, new UTF8Encoding(false));
        var toolchain = new PinnedDotNetToolchain(Path.Combine(repository, ".tools/dotnet"), "10.0.402", "10.0.13");
        var package = await new GeneratedAdapterCompiler(toolchain, runner).CompileWorkspaceAsync(
            Path.Combine(workspace, "src"), Path.Combine(root, "compiled"), ["*.txt"], releaseFiles:
            [new("src/Program.cs", "source", "Entry"), new("src/Components/Cp932GlyphEncoding.cs", "source", "Codec"),
             new("src/QlieFont.cs", "source", "Font"), new("src/GlyphRasterizer.cs", "source", "Glyphs"),
             new("tools/QlieLauncher.cs", "asset", "Launcher source"), new("tools/QlieLauncher.exe", "asset", "Launcher")]);
        Assert.Contains("tools/QlieLauncher.exe", package.PackageFiles!.Keys);
        Assert.Contains("tools/QlieLauncher.cs", package.PackageFiles.Keys);
        Assert.Contains("src/Components/Cp932GlyphEncoding.cs", package.PackageFiles.Keys);
        var run = await runner.RunAsync(new(toolchain.ExecutablePath, [Path.Combine(package.DirectoryPath, "adapter.dll")], package.DirectoryPath), default);
        Assert.Equal(0, run.ExitCode);
        Assert.Equal("component-ok", run.StandardOutput);
    }

    private static string Repository()
    {
        var directory = new DirectoryInfo(AppContext.BaseDirectory);
        while (directory is not null && !File.Exists(Path.Combine(directory.FullName, "global.json"))) directory = directory.Parent;
        return directory?.FullName ?? throw new InvalidOperationException("Repository fixture not found.");
    }

    public void Dispose() { if (Directory.Exists(root)) Directory.Delete(root, true); }
}
