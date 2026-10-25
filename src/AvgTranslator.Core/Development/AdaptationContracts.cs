using AvgTranslator.Core.Isolation;
using AvgTranslator.Core.Adapters;
using AvgTranslator.Core.Validation;

namespace AvgTranslator.Core.Development;

public sealed record AdaptationSample(string RelativePath, string ContentBase64);
public sealed record AdaptationAdapterOption(string Id, string Version, string Description);

public sealed record AdaptationModelContext(
    string GameId,
    IReadOnlyList<AdaptationSample> Samples,
    IReadOnlyList<AdaptationAdapterOption> ExistingAdapters,
    IReadOnlyList<string> Diagnostics,
    string ProtocolDescription,
    int MaximumOutputTokens,
    long RemainingTotalTokens,
    IReadOnlyList<ExecutableAnalysis>? Executables = null);

public sealed record AdaptationProposal(string? ReuseAdapterId, string? SourceCode, string Summary, TokenUsage Usage,
    AdapterFamily? Family = null);

public interface IAdaptationModel
{
    Task<AdaptationProposal> GenerateAsync(AdaptationModelContext context, CancellationToken cancellationToken);
}

public sealed record AdaptationLimits(
    int MaximumAttempts = 2,
    long MaximumTokens = 16_000,
    int MaximumOutputTokens = 6_000,
    int MaximumSampleFiles = 4,
    int MaximumSampleBytes = 64 * 1024,
    int MaximumTotalSampleBytes = 128 * 1024,
    int MaximumSourceBytes = 96 * 1024,
    TimeSpan? TotalTimeout = null);

public sealed record AdaptationRequest(string GameId, string GameDirectory,
    IReadOnlyList<string> SampleFiles, IReadOnlyList<IGameAdapter> ExistingAdapters,
    AdaptationLimits? Limits = null);

public enum AdaptationStatus { Activated, Reused, Failed, Unavailable, BudgetExhausted, Cancelled }

public sealed record AdaptationResult(AdaptationStatus Status, IGameAdapter? Adapter,
    IReadOnlyList<string> Diagnostics, string WorkspacePath, TokenUsage Usage);

public sealed record PinnedDotNetToolchain(string RootDirectory, string SdkVersion, string RuntimeVersion)
{
    public string ExecutablePath => Path.Combine(Path.GetFullPath(RootDirectory), "dotnet.exe");
}

public sealed record GeneratedAdapterPackage(string Id, string Version, string DirectoryPath,
    IReadOnlyList<string> ResourceFiles, string SourceSha256, string AssemblySha256, AdapterFamily? Family = null);

public sealed record GeneratedAdapterVerification(bool Passed, IReadOnlyList<string> Diagnostics);

public sealed class GeneratedAdapterException(string message) : Exception(message);
