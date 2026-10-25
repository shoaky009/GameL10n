using System.Text;
using AvgTranslator.Core.Adapters;
using AvgTranslator.VnTextPatch;
using VNTextPatch.Shared;

namespace AvgTranslator.Tests.Adapters;

public sealed class VnTextPatchAdapterTests : IDisposable
{
    private readonly string root = Path.Combine(Path.GetTempPath(), "avg-vntextpatch-tests-" + Guid.NewGuid().ToString("N"));

    [Fact]
    public void EveryCatalogEntryHasAnInstantiableBundledGameParser()
    {
        var formats = VnTextPatchFormatCatalog.All;
        Assert.Equal(29, formats.Count);
        Assert.Equal(Worker.SupportedFormats.Order(), formats.Select(format => format.Key).Order());
        Assert.Equal(formats.Count, formats.Select(format => format.Family.AdapterId).Distinct().Count());
        Assert.DoesNotContain(formats, format => format.Key is "Json" or "Jsonl" or "KirikiriScn");
        foreach (var format in formats)
        {
            format.Family.Validate();
            var parserType = typeof(Worker).Assembly.GetType(format.ParserType, throwOnError: true)!;
            var parser = Assert.IsAssignableFrom<IScript>(Activator.CreateInstance(parserType, nonPublic: true));
            if (parser.Extension is { } extension) Assert.Contains(extension, format.Extensions);
        }
    }

    [Fact]
    public async Task RenpyTrialUsesUtf8AndPreservesCodeCommentsResourcesAndSourceBytes()
    {
        var source = Directory.CreateDirectory(Path.Combine(root, "source")).FullName;
        var sourceFile = Path.Combine(source, "script.rpy");
        const string script = "# \"日本語のコメント\"\r\n"
            + "define e = Character(\"固定名\")\r\n"
            + "image bg room = \"images/背景.png\"\r\n"
            + "init python:\r\n"
            + "    \"Python の文字列\"\r\n"
            + "    config.name = \"設定\"\r\n"
            + "label start:\r\n"
            + "    scene bg room\r\n"
            + "    play music \"audio/音楽.ogg\"\r\n"
            + "    $ code = \"コード\"\r\n"
            + "    e \"こんにちは # 本文。\"\r\n"
            + "    \"地の文。\"\r\n"
            + "    menu:\r\n"
            + "        \"進む\":\r\n"
            + "            jump done\r\n"
            + "label done:\r\n"
            + "    return\r\n";
        var original = new UTF8Encoding(false).GetBytes(script);
        await File.WriteAllBytesAsync(sourceFile, original);
        var adapter = CreateAdapter("Renpy");

        var probe = await adapter.ProbeAsync(source);
        Assert.NotNull(probe);
        Assert.Equal(new[] { "script.rpy" }, probe.RequiredFiles);
        var extracted = await adapter.ExtractAsync(source);
        Assert.Equal(new[] { "こんにちは # 本文。", "地の文。", "進む" }, extracted.Units.Select(unit => unit.SourceText));
        Assert.DoesNotContain(extracted.Warnings, warning => warning.Contains("KaGuYa", StringComparison.Ordinal));

        foreach (var translations in new[]
                 {
                     new Dictionary<string, string>(),
                     extracted.Units.ToDictionary(unit => unit.Id, unit => unit.SourceText)
                 })
        {
            var output = Path.Combine(root, "identity-" + translations.Count);
            await adapter.BuildPatchAsync(source, translations, output);
            Assert.Equal(original, await File.ReadAllBytesAsync(Path.Combine(output, "script.rpy")));
        }

        var trial = Path.Combine(root, "trial");
        await adapter.BuildPatchAsync(source,
            new Dictionary<string, string> { [extracted.Units[0].Id] = "你好 # 世界。" }, trial);
        Assert.Equal(new UTF8Encoding(false).GetBytes(script.Replace("こんにちは # 本文。", "你好 # 世界。")),
            await File.ReadAllBytesAsync(Path.Combine(trial, "script.rpy")));
        Assert.Equal(original, await File.ReadAllBytesAsync(sourceFile));
        var validation = await adapter.ValidatePatchAsync(source, trial);
        Assert.True(validation.Passed);
        Assert.Equal("not-tested", validation.RuntimeStatus);

        await File.WriteAllBytesAsync(Path.Combine(trial, "script.rpy"), [0xFF, 0xFE, 0xFF]);
        Assert.False((await adapter.ValidatePatchAsync(source, trial)).Passed);
    }

    [Fact]
    public async Task KirikiriUnicodeTrialPreservesCommentsCommandsAndUntranslatedText()
    {
        var source = Directory.CreateDirectory(Path.Combine(root, "source")).FullName;
        const string script = "; 先頭コメント\r\n*start\r\n@bg storage=背景\r\n; 中間コメント\r\nこんにちは。[p]\r\n";
        var file = Path.Combine(source, "scene.ks");
        await File.WriteAllTextAsync(file, script, new UnicodeEncoding(false, true));
        var original = await File.ReadAllBytesAsync(file);
        var adapter = CreateAdapter("KirikiriKs");
        Assert.NotNull(await adapter.ProbeAsync(source));
        var extracted = await adapter.ExtractAsync(source);
        var unit = Assert.Single(extracted.Units);
        Assert.Equal("こんにちは。", unit.SourceText);

        var identity = Path.Combine(root, "identity");
        await adapter.BuildPatchAsync(source, new Dictionary<string, string>(), identity);
        Assert.Equal(original, await File.ReadAllBytesAsync(Path.Combine(identity, "scene.ks")));

        var trial = Path.Combine(root, "trial");
        await adapter.BuildPatchAsync(source, new Dictionary<string, string> { [unit.Id] = "你好，世界。" }, trial);
        Assert.Equal(script.Replace("こんにちは。", "你好，世界。"),
            await File.ReadAllTextAsync(Path.Combine(trial, "scene.ks"), Encoding.Unicode));
        Assert.True((await adapter.ValidatePatchAsync(source, trial)).Passed);
        Assert.Equal(original, await File.ReadAllBytesAsync(file));
    }

    [Fact]
    public async Task KaguyaExtractionWarnsThatCp932ScriptSupportDoesNotVerifyChineseRuntime()
    {
        Encoding.RegisterProvider(CodePagesEncodingProvider.Instance);
        var source = Directory.CreateDirectory(Path.Combine(root, "source")).FullName;
        string path = Path.Combine(source, "message.dat");
        await using (var stream = File.Create(path))
        using (var writer = new BinaryWriter(stream, Encoding.UTF8, leaveOpen: true))
        {
            writer.Write(Encoding.ASCII.GetBytes("[SCR-MESSAGE]ver4.0"));
            writer.Write((ushort)0); // Encryption flag and key.
            writer.Write(0); // Names.
            writer.Write(1); // Choices.
            byte[] text = Encoding.GetEncoding(932).GetBytes("こんにちは。");
            writer.Write((short)text.Length);
            writer.Write(text);
            writer.Write(0); // Messages.
            writer.Write(0); // Message groups.
        }
        var adapter = CreateAdapter("Kaguya");
        var probe = await adapter.ProbeAsync(source);
        Assert.NotNull(probe);
        Assert.Contains(probe.Warnings, warning => warning.Contains("CP932", StringComparison.Ordinal) && warning.Contains("中文运行", StringComparison.Ordinal));
        var extracted = await adapter.ExtractAsync(source);
        var unit = Assert.Single(extracted.Units);
        Assert.Contains(extracted.Warnings, warning => warning.Contains("可提取不代表已支持中文运行", StringComparison.Ordinal));
        var error = await Assert.ThrowsAsync<VnTextPatchException>(() => adapter.BuildPatchAsync(source,
            new Dictionary<string, string> { [unit.Id] = "汉语简体" }, Path.Combine(root, "trial")));
        Assert.Contains("编码", error.Message);
    }

    [Fact]
    public async Task ArtemisReadsFinalDialogueAndRejectsUnrepresentableChineseWithoutChangingSource()
    {
        Encoding.RegisterProvider(CodePagesEncodingProvider.Instance);
        var sjis = Encoding.GetEncoding(932, EncoderFallback.ExceptionFallback, DecoderFallback.ExceptionFallback);
        var source = Directory.CreateDirectory(Path.Combine(root, "source")).FullName;
        var original = sjis.GetBytes("*start\r\n[name アキ]\r\nこんにちは。");
        var sourceFile = Path.Combine(source, "scene.art");
        await File.WriteAllBytesAsync(sourceFile, original);
        var adapter = CreateAdapter("ArtemisTxt");
        var extracted = await adapter.ExtractAsync(source);
        Assert.Equal(new[] { "アキ", "こんにちは。" }, extracted.Units.Select(unit => unit.SourceText));

        var output = Path.Combine(root, "trial");
        var error = await Assert.ThrowsAsync<VnTextPatchException>(() => adapter.BuildPatchAsync(source,
            new Dictionary<string, string> { [extracted.Units[1].Id] = "汉语简体" }, output));
        Assert.Contains("编码", error.Message);
        Assert.Equal(original, await File.ReadAllBytesAsync(sourceFile));
        Assert.False(File.Exists(Path.Combine(output, "sjis_ext.bin")));
    }

    [Theory]
    [InlineData("CatSystem", "fake.cst", "This is not a CST script.")]
    [InlineData("Renpy", "script.rpy", "image bg = \"日本語.png\"\n$ value = \"日本語\"\n")]
    [InlineData("Whale", "notes.txt", "これはゲームの台本ではありません。\n")]
    [InlineData("TmrHiroAdvSystemText", "table.dat", "not a length-prefixed script")]
    public async Task ProbeRejectsUnsupportedContentEvenWhenExtensionMatches(string format, string name, string text)
    {
        var source = Directory.CreateDirectory(Path.Combine(root, "source")).FullName;
        await File.WriteAllTextAsync(Path.Combine(source, name), text, new UTF8Encoding(false));
        Assert.Null(await CreateAdapter(format).ProbeAsync(source));
    }

    private static VnTextPatchAdapter CreateAdapter(string key)
    {
        var directory = new DirectoryInfo(AppContext.BaseDirectory);
        var configuration = directory.Parent!.Name;
        while (directory is not null && !File.Exists(Path.Combine(directory.FullName, "global.json"))) directory = directory.Parent;
        Assert.NotNull(directory);
        var executable = Path.Combine(directory.FullName, ".tools", "dotnet", "dotnet.exe");
        var assembly = Path.Combine(directory.FullName, "tools", "AvgTranslator.VnTextPatch", "bin", configuration,
            "net10.0", "AvgTranslator.VnTextPatch.dll");
        Assert.True(File.Exists(executable), "The fixture requires the repository's pinned .NET toolchain.");
        Assert.True(File.Exists(Path.ChangeExtension(assembly, ".runtimeconfig.json")), "Build the bundled worker before running integration tests.");
        return new VnTextPatchAdapter(VnTextPatchFormatCatalog.All.Single(format => format.Key == key),
            workerExecutablePath: executable, workerAssemblyPath: assembly);
    }

    public void Dispose()
    {
        if (Directory.Exists(root)) Directory.Delete(root, recursive: true);
    }
}
