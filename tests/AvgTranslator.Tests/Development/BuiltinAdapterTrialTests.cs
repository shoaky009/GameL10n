using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using AvgTranslator.Core.Development;

namespace AvgTranslator.Tests.Development;

public sealed class BuiltinAdapterTrialTests : IDisposable
{
    private readonly string root = Path.Combine(Path.GetTempPath(), "avg-builtin-trial-tests-" + Guid.NewGuid().ToString("N"));

    [Fact]
    public async Task RealRenpyTrialReturnsBoundedSamplesAndVerifiesOriginalBytesWithoutLeavingOutput()
    {
        var source = Directory.CreateDirectory(Path.Combine(root, "resources")).FullName;
        var script = "label start:\n" + string.Join("\n", Enumerable.Range(0, 8)
            .Select(i => "    \"" + i + new string('あ', 330) + "。\"")) + "\n";
        var original = new UTF8Encoding(false).GetBytes(script);
        await File.WriteAllBytesAsync(Path.Combine(source, "script.rpy"), original);
        var runner = new RecordingRunner(new AdapterProcessRunner());
        var trial = CreateTrial(runner);

        var result = await trial.RunAsync("Renpy", root, "resources", ["script.rpy"]);

        Assert.Equal("passed", result.Status);
        Assert.Equal("passed", result.RoundtripStatus);
        Assert.Equal("not-tested", result.RuntimeStatus);
        Assert.Equal(8, result.UnitCount);
        Assert.Equal(5, result.Samples.Count);
        Assert.All(result.Samples, sample => Assert.InRange(sample.Text.Length, 1, 300));
        Assert.Equal(Convert.ToHexStringLower(SHA256.HashData(original)), result.SourceHashes["script.rpy"]);
        Assert.Equal(original, await File.ReadAllBytesAsync(Path.Combine(source, "script.rpy")));
        Assert.Equal(new[] { "extract", "build" }, runner.Requests.Select(Operation));
        Assert.All(runner.Requests, request => Assert.False(Directory.Exists(request.WorkspacePath)));
        using var buildRequest = JsonDocument.Parse(runner.Requests[1].StandardInput);
        Assert.False(Directory.Exists(buildRequest.RootElement.GetProperty("outputDirectory").GetString()));
        Assert.Single(Directory.EnumerateFileSystemEntries(root));
    }

    [Fact]
    public async Task DamagedParserReportsResourceAndUnderlyingExceptionInsteadOfUnsupported()
    {
        var source = Directory.CreateDirectory(Path.Combine(root, "resources")).FullName;
        await File.WriteAllBytesAsync(Path.Combine(source, "broken.cst"), new byte[32]);

        var result = await CreateTrial(new AdapterProcessRunner()).RunAsync("CatSystem", root, "resources", ["broken.cst"]);

        Assert.Equal("failed", result.Status);
        Assert.Equal("not-run", result.RoundtripStatus);
        Assert.Equal("broken.cst", result.Diagnostic!.ResourcePath);
        Assert.NotNull(result.Diagnostic.Inner);
        Assert.Equal("System.IO.InvalidDataException", result.Diagnostic.Inner.ErrorType);
        Assert.Contains("CatSystemScript", result.Diagnostic.Inner.StackTrace);
        Assert.NotEmpty(result.Diagnostic.Inner.Message);
        Assert.DoesNotContain(" at ", result.Error);
        Assert.Single(result.SourceHashes);
    }

    [Theory]
    [InlineData("resources/../work", "script.rpy")]
    [InlineData("src", "script.rpy")]
    [InlineData("resources", "../script.rpy")]
    public async Task RejectsPathsOutsideExplicitWorkspaceSampleScope(string directory, string file)
    {
        Directory.CreateDirectory(Path.Combine(root, "resources"));
        var runner = new NeverRunner();

        var result = await new BuiltinAdapterTrial(runner).RunAsync("Renpy", root, directory, [file]);

        Assert.Equal("rejected", result.Status);
        Assert.Equal(0, runner.Calls);
        Assert.Empty(result.SourceHashes);
    }

    [Fact]
    public async Task RejectsAbsoluteDirectoriesAndMissingCompanionsBeforeStartingAParser()
    {
        var source = Directory.CreateDirectory(Path.Combine(root, "resources")).FullName;
        await File.WriteAllBytesAsync(Path.Combine(source, "scene.src"), [1, 2, 3]);
        var runner = new NeverRunner();
        var trial = new BuiltinAdapterTrial(runner);

        Assert.Equal("rejected", (await trial.RunAsync("Softpal", root, source, ["scene.src"])).Status);
        var missing = await trial.RunAsync("Softpal", root, "resources", ["scene.src"]);

        Assert.Equal("rejected", missing.Status);
        Assert.Contains("TEXT.DAT", missing.Error);
        Assert.Equal("TEXT.DAT", missing.Diagnostic!.ResourcePath);
        Assert.Equal(0, runner.Calls);
    }

    [Fact]
    public async Task SampleLimitsAreReportedAsLimitsWithoutClaimingUnsupported()
    {
        var source = Directory.CreateDirectory(Path.Combine(root, "work")).FullName;
        var runner = new NeverRunner();
        var trial = new BuiltinAdapterTrial(runner);
        var count = await trial.RunAsync("Renpy", root, "work", Enumerable.Range(0, 13).Select(i => i + ".rpy").ToArray());
        await using (var file = File.Create(Path.Combine(source, "large.rpy"))) file.SetLength(64L * 1024 * 1024 + 1);
        var bytes = await trial.RunAsync("Renpy", root, "work", ["large.rpy"]);

        Assert.Equal("limit-exceeded", count.Status);
        Assert.Contains("12", count.Error);
        Assert.Equal("limit-exceeded", bytes.Status);
        Assert.Contains("64 MiB", bytes.Error);
        Assert.Equal(0, runner.Calls);
    }

    [Fact]
    public async Task SourceChangesCannotBeHiddenBySuccessfulExtractionResponse()
    {
        var source = Directory.CreateDirectory(Path.Combine(root, "resources")).FullName;
        var original = "label start:\n    \"こんにちは。\"\n"u8.ToArray();
        var path = Path.Combine(source, "script.rpy");
        await File.WriteAllBytesAsync(path, original);
        var runner = new MutatingRunner(path);

        var result = await new BuiltinAdapterTrial(runner).RunAsync("Renpy", root, "resources", ["script.rpy"]);

        Assert.Equal("failed", result.Status);
        Assert.Contains("发生变化", result.Error);
        Assert.Equal("script.rpy", result.Diagnostic!.ResourcePath);
        Assert.Equal(Convert.ToHexStringLower(SHA256.HashData(original)), result.SourceHashes["script.rpy"]);
        Assert.Equal(1, runner.Calls);
    }

    private static BuiltinAdapterTrial CreateTrial(IAdapterProcessRunner runner)
    {
        var directory = new DirectoryInfo(AppContext.BaseDirectory);
        var configuration = directory.Parent!.Name;
        while (directory is not null && !File.Exists(Path.Combine(directory.FullName, "global.json"))) directory = directory.Parent;
        Assert.NotNull(directory);
        return new BuiltinAdapterTrial(runner, Path.Combine(directory.FullName, ".tools", "dotnet", "dotnet.exe"),
            Path.Combine(directory.FullName, "tools", "AvgTranslator.VnTextPatch", "bin", configuration, "net10.0", "AvgTranslator.VnTextPatch.dll"));
    }

    private static string Operation(AdapterProcessRequest request)
    {
        using var json = JsonDocument.Parse(request.StandardInput);
        return json.RootElement.GetProperty("operation").GetString()!;
    }

    private sealed class RecordingRunner(IAdapterProcessRunner inner) : IAdapterProcessRunner
    {
        public List<AdapterProcessRequest> Requests { get; } = [];
        public Task<AdapterProcessResult> RunAsync(AdapterProcessRequest request, CancellationToken cancellationToken)
        {
            Requests.Add(request);
            return inner.RunAsync(request, cancellationToken);
        }
    }

    private sealed class NeverRunner : IAdapterProcessRunner
    {
        public int Calls { get; private set; }
        public Task<AdapterProcessResult> RunAsync(AdapterProcessRequest request, CancellationToken cancellationToken)
        {
            Calls++;
            throw new InvalidOperationException("The sample must be rejected before invoking the worker.");
        }
    }

    private sealed class MutatingRunner(string sourceFile) : IAdapterProcessRunner
    {
        public int Calls { get; private set; }
        public async Task<AdapterProcessResult> RunAsync(AdapterProcessRequest request, CancellationToken cancellationToken)
        {
            Calls++;
            using var json = JsonDocument.Parse(request.StandardInput);
            await File.WriteAllTextAsync(sourceFile, "changed", cancellationToken);
            await File.WriteAllTextAsync(json.RootElement.GetProperty("responseFile").GetString()!,
                """{"protocolVersion":1,"success":true,"extraction":{"units":[],"warnings":[]}}""", cancellationToken);
            return new(0, "ok", "");
        }
    }

    public void Dispose()
    {
        if (Directory.Exists(root)) Directory.Delete(root, recursive: true);
    }
}
