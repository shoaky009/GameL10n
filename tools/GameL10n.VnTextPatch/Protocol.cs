using System.Text.Json.Serialization;
using VNTextPatch.Shared.Scripts.Yuris;

namespace GameL10n.VnTextPatch;

internal sealed record Request(int ProtocolVersion, string Operation, string SourceDirectory,
    string? OutputDirectory, string[]? ResourceFiles, string? ResponseFile, string? TranslationsFile,
    Dictionary<string, string>? Translations, string Format, bool IncludeYurisSources = false,
    YurisEncodingOptions? YurisEncoding = null);

internal sealed record Response(int ProtocolVersion = 1, bool Success = true, string? Error = null,
    Probe? Probe = null, Extraction? Extraction = null, Build? Build = null, Validation? Validation = null,
    Diagnostic? Diagnostic = null);
internal sealed record Diagnostic(string ErrorType, string Message, string? ResourcePath = null,
    long? Offset = null, string? StackTrace = null, Diagnostic? Inner = null);
internal sealed record Probe(string EngineId, string AdapterId, string AdapterVersion, string Description,
    IReadOnlyList<string> RequiredFiles, IReadOnlyList<string> Warnings, string? OriginalExecutable = null);
internal sealed record Unit(string Id, string ResourcePath, string Locator, string SourceText, string Context,
    string? Speaker = null, string Kind = "dialogue",
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] YurisTextSource? YurisSource = null);
internal sealed record Extraction(IReadOnlyList<Unit> Units, IReadOnlyList<string> Warnings);
internal sealed record Build(IReadOnlyList<string> Files, IReadOnlyList<string> Warnings, string? LaunchExecutable = null);
internal sealed record Issue(string Code, string Message, string? UnitId = null);
internal sealed record Validation(bool Passed, IReadOnlyList<Issue> Issues, string RuntimeStatus = "not-tested");
