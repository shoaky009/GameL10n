using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using AvgTranslator.Core;
using AvgTranslator.Core.Development;
using AvgTranslator.Core.Isolation;
using AvgTranslator.Core.Adapters;

namespace AvgTranslator.Tests.Development;

public sealed class AutomaticAdaptationTests : IDisposable
{
    private readonly string root = Path.Combine(Path.GetTempPath(), "avg-adaptation-test-" + Guid.NewGuid().ToString("N"));
    private static readonly JsonSerializerOptions Json = new(JsonSerializerDefaults.Web);

    public AutomaticAdaptationTests() => Directory.CreateDirectory(root);

    [Fact]
    public async Task ExistingAdapterIsReusedWithoutSpendingModelTokens()
    {
        var model = new FakeModel(_ => throw new InvalidOperationException("The model should not be called."));
        var adapter = new ExistingAdapter();
        var service = new AutomaticAdaptationService(model, Compiler(new ScriptedRunner()), new LocalAdapterPackageStore(Path.Combine(root, "store")));
        var result = await service.AdaptAsync(Request([adapter]));
        Assert.Equal(AdaptationStatus.Reused, result.Status);
        Assert.Same(adapter, result.Adapter);
        Assert.Equal(0, model.Calls);
        Assert.NotEqual(Path.Combine(root, "game"), adapter.ProbedDirectory);
    }

    [Fact]
    public async Task SandboxFailureIsExplicitAndDoesNotActivateAPackage()
    {
        var model = new FakeModel(_ => new AdaptationProposal(null, "public static class Program { public static void Main() {} }", "test", new TokenUsage(20, 20)));
        var service = new AutomaticAdaptationService(model, Compiler(new UnavailableRunner()), new LocalAdapterPackageStore(Path.Combine(root, "store")));
        var result = await service.AdaptAsync(Request());
        Assert.Equal(AdaptationStatus.Unavailable, result.Status);
        Assert.Null(result.Adapter);
        Assert.Equal(0, model.Calls);
        Assert.Contains(result.Diagnostics, diagnostic => diagnostic.Contains("test sandbox unavailable", StringComparison.Ordinal));
        Assert.True(File.Exists(Path.Combine(result.WorkspacePath, "result.json")));
    }

    [Fact]
    public async Task AttemptsAreBoundedAndDiagnosticsAreReturnedToTheModel()
    {
        var model = new FakeModel(context =>
        {
            if (context.Diagnostics.Count > 0) Assert.Contains(context.Diagnostics, diagnostic => diagnostic.Contains("missing", StringComparison.Ordinal));
            return new AdaptationProposal(null, null, "No source.", new TokenUsage(20, 20));
        });
        var service = new AutomaticAdaptationService(model, Compiler(new ScriptedRunner()), new LocalAdapterPackageStore(Path.Combine(root, "store")));
        var result = await service.AdaptAsync(Request());
        Assert.Equal(AdaptationStatus.Failed, result.Status);
        Assert.Equal(2, model.Calls);
        Assert.Equal(80, result.Usage.TotalTokens);
    }

    [Fact]
    public async Task ProviderUsageOverBudgetCannotActivateAndStopsRetries()
    {
        var model = new FakeModel(_ => new AdaptationProposal(null, "ignored", "over budget", new TokenUsage(100_000, 100_000)));
        var result = await new AutomaticAdaptationService(model, Compiler(new ScriptedRunner()), new LocalAdapterPackageStore(Path.Combine(root, "store"))).AdaptAsync(Request());
        Assert.Equal(AdaptationStatus.BudgetExhausted, result.Status);
        Assert.Equal(1, model.Calls);
    }

    [Fact]
    public async Task CancellationStopsAWaitingModelWithoutActivation()
    {
        var request = Request();
        using var cancellation = new CancellationTokenSource();
        var model = new WaitingModel(cancellation);
        var result = await new AutomaticAdaptationService(model, Compiler(new ScriptedRunner()), new LocalAdapterPackageStore(Path.Combine(root, "store")))
            .AdaptAsync(request, cancellation.Token);
        Assert.Equal(AdaptationStatus.Cancelled, result.Status);
        Assert.Null(result.Adapter);
    }

    [Fact]
    public async Task HostVerificationRejectsUnitDropsAndPreservesPreviousActivation()
    {
        var runner = new ScriptedRunner { DropTranslatedUnit = true };
        var package = CreatePackage();
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        await store.PublishAndActivateAsync("game", package, ["known prior version"]);
        var adapter = new GeneratedGameAdapter(package, Toolchain(), runner, Path.Combine(root, "runs"));
        var request = Request();
        var result = await new GeneratedAdapterVerifier().VerifyAsync(adapter, request.GameDirectory, Path.Combine(root, "verification"));
        Assert.False(result.Passed);
        Assert.Contains(result.Diagnostics, diagnostic => diagnostic.Contains("dropped", StringComparison.Ordinal));
        Assert.Equal(package.Version, store.LoadActive("game")!.Version);
    }

    [Fact]
    public async Task HostVerificationChecksByteRoundtripAndDistinctUnicodeTranslations()
    {
        var package = CreatePackage();
        var runner = new ScriptedRunner();
        var adapter = new GeneratedGameAdapter(package, Toolchain(), runner, Path.Combine(root, "runs"));
        var request = Request();
        var result = await new GeneratedAdapterVerifier().VerifyAsync(adapter, request.GameDirectory, Path.Combine(root, "verification"));
        Assert.True(result.Passed, string.Join("\n", result.Diagnostics));
        Assert.True(runner.SawUnicodeTranslations);
        Assert.False(Directory.EnumerateDirectories(Path.Combine(root, "runs")).Any());
    }

    [Fact]
    public async Task PathEscapingBuildOutputIsRejectedBeforeAnythingIsCopied()
    {
        var package = CreatePackage();
        var runner = new ScriptedRunner { EscapeOutput = true };
        var adapter = new GeneratedGameAdapter(package, Toolchain(), runner, Path.Combine(root, "runs"));
        var request = Request();
        var output = Path.Combine(root, "patch");
        Directory.CreateDirectory(output);
        await Assert.ThrowsAsync<GeneratedAdapterException>(() => adapter.BuildPatchAsync(request.GameDirectory, new Dictionary<string, string>(), output));
        Assert.Empty(Directory.EnumerateFileSystemEntries(output));
    }

    [Fact]
    public async Task PublishedTemplateIsVerifiedAndReusedForAnotherGameWithoutModelCalls()
    {
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var package = CreatePackage();
        await store.PublishAndActivateAsync("first-game", package, ["passed"]);
        var model = new FakeModel(_ => throw new InvalidOperationException("No model call should be needed."));
        var result = await new AutomaticAdaptationService(model, Compiler(new ScriptedRunner()), store).AdaptAsync(Request());
        Assert.Equal(AdaptationStatus.Reused, result.Status);
        Assert.Equal(0, model.Calls);
        var reloaded = new LocalAdapterPackageStore(store.RootDirectory);
        Assert.Equal(package.Id, reloaded.LoadActive("game")!.Id);
        var catalog = Assert.Single(reloaded.ReadCatalog([], "game"));
        Assert.Equal("template.line-script", catalog.Family.AdapterId);
        var compatible = Assert.Single(catalog.Games);
        Assert.Equal("sample-roundtrip-passed", compatible.StructureStatus);
        Assert.Equal("not-tested", compatible.RuntimeStatus);
        Assert.Single(compatible.SourceHashes);
        Assert.Single(reloaded.ReadCatalog([], "Line script"));
    }

    [Fact]
    public async Task PackagesCanBeReactivatedFromDiskAndRolledBack()
    {
        var package = CreatePackage();
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var first = await store.PublishAndActivateAsync("game", package, ["passed"]);
        var second = await store.PublishAndActivateAsync("game", package with { Version = "second" }, ["passed"]);
        await store.PublishAndActivateAsync("game", second, ["repeat activation"]);
        Assert.Equal(second.Version, new LocalAdapterPackageStore(store.RootDirectory).LoadActive("game")!.Version);
        Assert.True(await store.RollbackAsync("game"));
        Assert.Equal(first.Version, store.LoadActive("game")!.Version);
        Assert.True(Directory.Exists(second.DirectoryPath));
        var reusable = Assert.Single(new LocalAdapterPackageStore(store.RootDirectory).ListReusablePackages());
        Assert.Equal(first.Version, reusable.Version);
        Assert.Equal("template.line-script", reusable.Id);
    }

    [Fact]
    public async Task OversizedOrExecutableSamplesAreRejectedBeforeModelUse()
    {
        var model = new FakeModel(_ => throw new InvalidOperationException("Should not call model."));
        var request = Request() with { SampleFiles = ["game.exe"] };
        var result = await new AutomaticAdaptationService(model, Compiler(new ScriptedRunner()), new LocalAdapterPackageStore(Path.Combine(root, "store"))).AdaptAsync(request);
        Assert.Equal(AdaptationStatus.Failed, result.Status);
        Assert.Equal(0, model.Calls);
    }

    private AdaptationRequest Request(IReadOnlyList<IGameAdapter>? adapters = null)
    {
        var game = Path.Combine(root, "game");
        Directory.CreateDirectory(game);
        File.WriteAllText(Path.Combine(game, "script.txt"), "First line\nSecond line", new UTF8Encoding(false));
        return new AdaptationRequest("game", game, ["script.txt"], adapters ?? []);
    }

    private PinnedDotNetToolchain Toolchain()
    {
        var runtime = Path.Combine(root, "runtime");
        Directory.CreateDirectory(Path.Combine(runtime, "sdk", "10.0.402", "Roslyn", "bincore"));
        Directory.CreateDirectory(Path.Combine(runtime, "packs", "Microsoft.NETCore.App.Ref", "10.0.13", "ref", "net10.0"));
        File.WriteAllText(Path.Combine(runtime, "dotnet.exe"), "test-only-runtime");
        File.WriteAllText(Path.Combine(runtime, "sdk", "10.0.402", "Roslyn", "bincore", "csc.dll"), "test-only-compiler");
        File.WriteAllText(Path.Combine(runtime, "packs", "Microsoft.NETCore.App.Ref", "10.0.13", "ref", "net10.0", "System.Runtime.dll"), "test-only-reference");
        return new PinnedDotNetToolchain(runtime, "10.0.402", "10.0.13");
    }

    private GeneratedAdapterCompiler Compiler(IIsolatedProcessRunner runner) => new(Toolchain(), runner);

    private GeneratedAdapterPackage CreatePackage()
    {
        var path = Path.Combine(root, "package");
        Directory.CreateDirectory(path);
        File.WriteAllText(Path.Combine(path, "Program.cs"), "// protocol fixture, never executed", new UTF8Encoding(false));
        File.WriteAllBytes(Path.Combine(path, "adapter.dll"), [1, 2, 3]);
        File.WriteAllText(Path.Combine(path, "adapter.runtimeconfig.json"), "{}");
        return new GeneratedAdapterPackage("template.line-script", "one", path, ["script.txt"], Hash(Path.Combine(path, "Program.cs")),
            Hash(Path.Combine(path, "adapter.dll")), new AdapterFamily("template", "line-script", "Line script"));
    }

    private static string Hash(string path) => Convert.ToHexStringLower(SHA256.HashData(File.ReadAllBytes(path)));
    public void Dispose() { if (Directory.Exists(root)) Directory.Delete(root, true); }

    private sealed class FakeModel(Func<AdaptationModelContext, AdaptationProposal> generate) : IAdaptationModel
    {
        public int Calls { get; private set; }
        public Task<AdaptationProposal> GenerateAsync(AdaptationModelContext context, CancellationToken cancellationToken)
        {
            Calls++;
            return Task.FromResult(generate(context));
        }
    }

    private sealed class WaitingModel(CancellationTokenSource cancellation) : IAdaptationModel
    {
        public Task<AdaptationProposal> GenerateAsync(AdaptationModelContext context, CancellationToken cancellationToken)
        {
            cancellation.Cancel();
            return new TaskCompletionSource<AdaptationProposal>().Task;
        }
    }

    private sealed class UnavailableRunner : IIsolatedProcessRunner
    {
        public Task<IsolatedProcessResult> RunAsync(IsolatedProcessRequest request, CancellationToken cancellationToken) =>
            throw new IsolationUnavailableException("test sandbox unavailable");
    }

    private sealed class ExistingAdapter : IGameAdapter
    {
        public string Id => "existing";
        public string Version => "1";
        public string? ProbedDirectory { get; private set; }
        public Task<GameProbe?> ProbeAsync(string gameDirectory, CancellationToken cancellationToken = default)
        {
            ProbedDirectory = gameDirectory;
            return Task.FromResult<GameProbe?>(new GameProbe("test", Id, Version, "test", ["script.txt"], []));
        }
        public Task<ExtractionResult> ExtractAsync(string sourceDirectory, CancellationToken cancellationToken = default) => throw new NotSupportedException();
        public Task<AdapterBuildResult> BuildPatchAsync(string sourceDirectory, IReadOnlyDictionary<string, string> translations, string patchDirectory, CancellationToken cancellationToken = default) => throw new NotSupportedException();
        public Task<ValidationResult> ValidatePatchAsync(string sourceDirectory, string patchDirectory, CancellationToken cancellationToken = default) => throw new NotSupportedException();
    }

    // Protocol fixtures test the host's independent checks, not OS containment or actual C# execution.
    private sealed class ScriptedRunner : IIsolatedProcessRunner
    {
        public bool DropTranslatedUnit { get; init; }
        public bool EscapeOutput { get; init; }
        public bool SawUnicodeTranslations { get; private set; }
        public Task<IsolatedProcessResult> RunAsync(IsolatedProcessRequest request, CancellationToken cancellationToken)
        {
            if (request.Arguments[0] == "--info") return Task.FromResult(new IsolatedProcessResult(0, "trusted toolchain", ""));
            if (request.Arguments[0].EndsWith("csc.dll", StringComparison.OrdinalIgnoreCase))
            {
                File.WriteAllBytes(Path.Combine(request.WorkspacePath, "build", "adapter.dll"), [1, 2, 3]);
                return Task.FromResult(new IsolatedProcessResult(0, "", ""));
            }
            using var document = JsonDocument.Parse(request.StandardInput);
            var input = document.RootElement;
            var source = input.GetProperty("sourceDirectory").GetString()!;
            var output = input.GetProperty("outputDirectory").GetString()!;
            var lines = File.ReadAllText(Path.Combine(source, "script.txt")).Split('\n');
            var operation = input.GetProperty("operation").GetString();
            object response;
            if (operation == "probe") response = new { protocolVersion = 1, success = true, probe = new GameProbe("fixture", "ignored", "ignored", "fixture", ["script.txt"], []) };
            else if (operation == "extract")
            {
                var units = lines.Select((line, index) => new TranslationUnit("line-" + index, "script.txt", "line:" + index, line, "fixture")).ToArray();
                if (DropTranslatedUnit && lines[0].Contains("AVG_TRANSLATION_", StringComparison.Ordinal)) units = units.Take(1).ToArray();
                response = new { protocolVersion = 1, success = true, extraction = new ExtractionResult(units, []) };
            }
            else if (operation == "build")
            {
                var translations = input.GetProperty("translations");
                for (var index = 0; index < lines.Length; index++)
                    if (translations.TryGetProperty("line-" + index, out var value)) lines[index] = value.GetString()!;
                SawUnicodeTranslations |= lines.Any(line => line.Contains("翻译", StringComparison.Ordinal));
                File.WriteAllText(Path.Combine(output, "script.txt"), string.Join('\n', lines), new UTF8Encoding(false));
                response = new { protocolVersion = 1, success = true, build = new AdapterBuildResult(EscapeOutput ? ["../escape.txt"] : ["script.txt"], []) };
            }
            else response = new { protocolVersion = 1, success = true, validation = new ValidationResult(true, [], "passed") };
            return Task.FromResult(new IsolatedProcessResult(0, JsonSerializer.Serialize(response, Json), ""));
        }
    }
}
