using AvgTranslator.Core.Isolation;
using AvgTranslator.Core.Adapters;
using AvgTranslator.Core.Validation;

namespace AvgTranslator.Core.Development;

public sealed record AdaptationLimits(int MaximumTurns = 60, long MaximumTokens = 1_000_000,
    int MaximumOutputTokens = 10_000);

public enum AdaptationObjective { Develop, Repair, ExpandCoverage }

public sealed record AdaptationRequest(string GameId, string GameDirectory,
    IReadOnlyList<IGameAdapter> ExistingAdapters,
    AdaptationLimits? Limits = null, string? UserInstruction = null,
    IReadOnlyList<BuiltinAdapterProbeObservation>? BuiltinProbes = null, bool ImproveExisting = false,
    AdaptationObjective? Objective = null, IReadOnlyList<TranslationUnit>? BaselineUnits = null);

public enum AdaptationStatus { Activated, Reused, Failed, Unavailable, BudgetExhausted, Cancelled, Incomplete, WaitingForUser, Unsupported }

public sealed record AdaptationBlocker(bool RequiresUserInput, string Reason, string NextAction, bool WorkRemaining = false);

public sealed record AdaptationResult(AdaptationStatus Status, IGameAdapter? Adapter,
    IReadOnlyList<string> Diagnostics, TokenUsage Usage, string Summary = "", string NextAction = "",
    AdaptationBlocker? Blocker = null, AdaptationProgressNote? ProgressNote = null,
    AdaptationProgressNote? LastBreakthrough = null, IReadOnlyList<string>? PendingCoverage = null,
    IReadOnlyList<string>? PendingRuntimeChecks = null);

public sealed record AdaptationProgress(string Stage, string Message, TokenUsage Usage,
    AdaptationProgressNote? ProgressNote = null, AdaptationProgressNote? LastBreakthrough = null);

public sealed record PinnedDotNetToolchain(string RootDirectory, string SdkVersion, string RuntimeVersion)
{
    public string ExecutablePath => Path.Combine(Path.GetFullPath(RootDirectory), "dotnet.exe");
}

public sealed record GeneratedAdapterPackage(string Id, string Version, string DirectoryPath,
    IReadOnlyList<string> ResourceFiles, string SourceSha256, string AssemblySha256, AdapterFamily? Family = null,
    IReadOnlyList<string>? ResourcePatterns = null,
    IReadOnlyDictionary<string, string>? PackageFiles = null,
    int ProtocolVersion = 1, int ManifestVersion = 1,
    IReadOnlyList<AdapterReleaseFile>? ReleaseFiles = null, AdapterRecognition? Recognition = null);

public sealed record GeneratedAdapterVerification(bool Passed, IReadOnlyList<string> Diagnostics,
    bool OriginalRoundtripPassed = false);

public sealed class GeneratedAdapterException(string message) : Exception(message);
