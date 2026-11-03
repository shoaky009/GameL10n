using System.Net;
using System.Net.Sockets;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using GameL10n.Core.Adapters;
using GameL10n.Core.Development;
using GameL10n.Core.Isolation;
using GameL10n.Core.Storage;

if (args.Length == 3 && args[0] == "prepare-engine-tools")
    return await EngineToolsPackage.RunAsync(args[1], args[2]);

if (args.Length >= 3 && args[0] == "correct-translations")
    return await TranslationCorrections.RunAsync(args[1], args[2], args.Contains("--apply"));

if (args.Length >= 5 && args[0] == "update-adapter-coverage")
    return await AdapterCoverageUpdate.RunAsync(args[1], args[2], args[3], args[4], args.Contains("--apply"));

if (args.Length >= 5 && args[0] == "repair-system4")
    return await System4Repair.RunAsync(args[1], args[2], args[3], args[4], args.Contains("--install"));

if (args.Length >= 5 && args[0] == "repair-qlie")
    return await QlieRepair.RunAsync(args[1], args[2], args[3], args[4], args.Contains("--install"));

if (args.Length < 3 || args[0] is not ("verify-darkblue" or "toolchain"))
{
    Console.Error.WriteLine("Usage: verify-darkblue <game-directory> <output-directory> | toolchain <portable-dotnet-root> <output-directory> | repair-qlie <project-directory> <work-directory> <adapter-source> <portable-dotnet-root> [--install]");
    return 1;
}
var output = Path.GetFullPath(args[2]);
var input = Path.GetFullPath(args[1]);
if (output.StartsWith(input.TrimEnd(Path.DirectorySeparatorChar) + Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase) || output.Equals(input, StringComparison.OrdinalIgnoreCase))
    throw new ArgumentException("Diagnostics output must be outside the input directory.");
Directory.CreateDirectory(output);
if (args[0] == "verify-darkblue")
{
    var adapter = new Sfa16AosV2Adapter();
    var initialHash = await LocalFiles.HashFileAsync(Path.Combine(input, "scr.aos"));
    var probe = await adapter.ProbeAsync(input) ?? throw new InvalidDataException("Unsupported game executable or archive.");
    var extracted = await adapter.ExtractAsync(input);
    var original = extracted.Units.ToDictionary(u => u.Id, u => u.SourceText);
    var roundtrip = Path.Combine(output, "roundtrip-" + Guid.NewGuid().ToString("N"));
    await adapter.BuildPatchAsync(input, original, roundtrip);
    var unchanged = initialHash == await LocalFiles.HashFileAsync(Path.Combine(roundtrip, "scr.aos"));
    if (!unchanged) throw new InvalidDataException("No-op build changed bytes.");
    Encoding.RegisterProvider(CodePagesEncodingProvider.Instance);
    var chinese = Encoding.GetEncoding(936, EncoderFallback.ExceptionFallback, DecoderFallback.ExceptionFallback);
    var translations = original.ToDictionary();
    var substitutions = 0;
    foreach (var unit in extracted.Units)
    {
        try { _ = chinese.GetByteCount(unit.SourceText); }
        catch (EncoderFallbackException) { translations[unit.Id] = "测试"; substitutions++; }
    }
    var sample = extracted.Units.First(u => u.Kind == "dialogue" && !u.SourceText.Contains('\\') && !u.SourceText.Contains('@'));
    translations[sample.Id] = "这是中文显示测试。";
    var patch = Path.Combine(output, "changed-" + Guid.NewGuid().ToString("N"));
    await adapter.BuildPatchAsync(input, translations, patch);
    var validation = await adapter.ValidatePatchAsync(input, patch);
    var recovered = await adapter.ExtractAsync(patch);
    var textRecovered = recovered.Units.Single(u => u.ResourcePath == sample.ResourcePath && u.Locator == sample.Locator).SourceText == translations[sample.Id];
    var report = new
    {
        probe.EngineId, adapter.Version, units = extracted.Units.Count,
        unitKinds = extracted.Units.GroupBy(u => u.Kind).ToDictionary(g => g.Key, g => g.Count()),
        originalHash = initialHash, originalUnchanged = initialHash == await LocalFiles.HashFileAsync(Path.Combine(input, "scr.aos")),
        roundtripIdentical = unchanged, validation, textRecovered, cp936Substitutions = substitutions,
        note = "Synthetic Chinese test text only; this is not a translated game or a runtime display test."
    };
    await LocalFiles.WriteJsonAsync(Path.Combine(output, "darkblue-report.json"), report);
    Console.WriteLine(JsonSerializer.Serialize(report, LocalFiles.JsonOptions));
    return validation.Passed && textRecovered ? 0 : 1;
}

var toolchain = new PinnedDotNetToolchain(input, "10.0.402", "10.0.13");
var runner = new AdapterProcessRunner();
var compiler = new GeneratedAdapterCompiler(toolchain, runner);
try
{
    await compiler.CheckAvailabilityAsync(Path.Combine(output, "preflight"));
    const string source = "using System; Console.WriteLine(Console.ReadLine());";
    var package = await compiler.CompileAsync(source, Path.Combine(output, "compile"), ["sample.dat"]);
    var workspace = Path.Combine(output, "run");
    Directory.CreateDirectory(workspace);
    var execution = await runner.RunAsync(new AdapterProcessRequest(toolchain.ExecutablePath,
        [Path.Combine(package.DirectoryPath, "adapter.dll")], workspace, "中文 UTF-8 测试\n"), default);
    var passed = execution.ExitCode == 0 && execution.StandardOutput.Trim() == "中文 UTF-8 测试";
    Console.WriteLine(passed ? "Local compilation and UTF-8 process communication passed." : execution.StandardError);
    return passed ? 0 : 1;
}
catch (Exception error) when (error is AdapterProcessUnavailableException or AdapterProcessLimitException or GeneratedAdapterException)
{
    Console.Error.WriteLine(error.Message);
    return 2;
}
