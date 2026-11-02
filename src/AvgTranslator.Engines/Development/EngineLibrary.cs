using AvgTranslator.Core.Adapters;

namespace AvgTranslator.Core.Development;

public enum EngineLibrarySource { BuiltIn, External, History }
public enum EngineLibraryReferenceKind { Active, Previous, Project, Source, Extraction, Job, Running }

public sealed record EngineLibraryReference(string AdapterId, string Version, string? GameId, string? GameTitle,
    EngineLibraryReferenceKind Kind, string? Path);

public sealed record EngineLibraryVersion(string AdapterId, string Version, EngineLibrarySource Source, bool Exists,
    string? DirectoryPath, DateTimeOffset? PublishedAt, bool Enabled, bool IsCandidate,
    IReadOnlyList<GameCompatibility> Games, IReadOnlyList<EngineLibraryReference> References, bool ReferencesKnown = false);

public sealed record EngineLibraryFamily(AdapterFamily Family, bool Enabled, IReadOnlyList<EngineLibraryVersion> Versions,
    IReadOnlyList<GameCompatibility> Games, IReadOnlyList<EngineLibraryReference> References);

/// <summary>A metadata snapshot. Package contents are verified when loaded for execution, not while browsing.</summary>
public sealed record EngineLibrarySnapshot(IReadOnlyList<EngineLibraryFamily> Families, IReadOnlyList<string> Diagnostics)
{
    public IReadOnlyList<EngineKnowledgeSummary> Knowledge { get; init; } = [];
}

public sealed record EngineLibraryDeleteResult(bool Deleted, IReadOnlyList<EngineLibraryReference> References,
    IReadOnlyList<string> Diagnostics);
