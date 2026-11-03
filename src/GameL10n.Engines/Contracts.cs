namespace GameL10n.Core;

public sealed record GameProbe(
    string EngineId, string AdapterId, string AdapterVersion, string Description,
    IReadOnlyList<string> RequiredFiles, IReadOnlyList<string> Warnings,
    string? OriginalExecutable = null);

public sealed record ExtractionResult(
    IReadOnlyList<TranslationUnit> Units, IReadOnlyList<string> Warnings);

public sealed record AdapterBuildResult(
    IReadOnlyList<string> Files, IReadOnlyList<string> Warnings,
    string? LaunchExecutable = null);

public interface IGameAdapter
{
    string Id { get; }
    string Version { get; }
    Adapters.AdapterCatalogEntry? CatalogEntry => null;
    Task<GameProbe?> ProbeAsync(string gameDirectory, CancellationToken cancellationToken = default);
    Task<ExtractionResult> ExtractAsync(string sourceDirectory, CancellationToken cancellationToken = default);
    Task<AdapterBuildResult> BuildPatchAsync(string sourceDirectory,
        IReadOnlyDictionary<string, string> translations, string patchDirectory,
        CancellationToken cancellationToken = default);
    Task<ValidationResult> ValidatePatchAsync(string sourceDirectory, string patchDirectory,
        CancellationToken cancellationToken = default);
    IReadOnlyList<ValidationIssue> ValidateTranslation(TranslationUnit unit, string text) => [];
}
