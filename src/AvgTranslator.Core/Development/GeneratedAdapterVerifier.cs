using System.Security.Cryptography;
using AvgTranslator.Core.Isolation;

namespace AvgTranslator.Core.Development;

public sealed class GeneratedAdapterVerifier
{
    /// <summary>Checks host-observed bytes and extraction; an extension's validation claim is never sufficient.</summary>
    public async Task<GeneratedAdapterVerification> VerifyAsync(GeneratedGameAdapter adapter, string sampleDirectory,
        string verificationDirectory, CancellationToken cancellationToken = default)
    {
        Directory.CreateDirectory(verificationDirectory);
        var diagnostics = new List<string>();
        try
        {
            var probe = await adapter.ProbeAsync(sampleDirectory, cancellationToken);
            if (probe is null) throw new GeneratedAdapterException("The generated adapter did not recognize its own resource samples.");
            var first = await adapter.ExtractAsync(sampleDirectory, cancellationToken);
            var repeated = await adapter.ExtractAsync(sampleDirectory, cancellationToken);
            RequireSameUnits(first, repeated, first.Units.ToDictionary(unit => unit.Id, unit => unit.SourceText));
            var originalTexts = first.Units.ToDictionary(unit => unit.Id, unit => unit.SourceText, StringComparer.Ordinal);
            var roundtrip = WorkspaceFiles.Resolve(verificationDirectory, "roundtrip");
            Directory.CreateDirectory(roundtrip);
            var built = await adapter.BuildPatchAsync(sampleDirectory, originalTexts, roundtrip, cancellationToken);
            var expectedFiles = adapter.Package.ResourceFiles.Select(Normalize).ToHashSet(StringComparer.OrdinalIgnoreCase);
            if (!expectedFiles.SetEquals(built.Files.Select(Normalize)))
                throw new GeneratedAdapterException("Original-text roundtrip must reproduce every approved resource.");
            foreach (var resource in adapter.Package.ResourceFiles)
            {
                var before = SHA256.HashData(WorkspaceFiles.ReadBounded(sampleDirectory, resource, 32 * 1024 * 1024));
                var after = SHA256.HashData(WorkspaceFiles.ReadBounded(roundtrip, resource, 32 * 1024 * 1024));
                if (!before.SequenceEqual(after)) throw new GeneratedAdapterException("Original-text roundtrip changed resource bytes: " + resource);
            }
            diagnostics.Add("Original-text build reproduced every approved sample byte for byte.");

            // Change all units, including duplicate source strings, to distinct values. IDs and locations must survive resizing.
            var translatedTexts = first.Units.Select((unit, index) => (unit.Id, Text: "AVG_TRANSLATION_" + index + "_翻译 " + unit.SourceText))
                .ToDictionary(pair => pair.Id, pair => pair.Text, StringComparer.Ordinal);
            var translated = WorkspaceFiles.Resolve(verificationDirectory, "translated");
            Directory.CreateDirectory(translated);
            var translatedBuild = await adapter.BuildPatchAsync(sampleDirectory, translatedTexts, translated, cancellationToken);
            if (!expectedFiles.SetEquals(translatedBuild.Files.Select(Normalize)))
                throw new GeneratedAdapterException("Translated build dropped an approved resource.");
            var extractedTranslation = await adapter.ExtractAsync(translated, cancellationToken);
            RequireSameUnits(first, extractedTranslation, translatedTexts);
            var validation = await adapter.ValidatePatchAsync(sampleDirectory, translated, cancellationToken);
            if (!validation.Passed) throw new GeneratedAdapterException("The extension rejected the independently checked translated sample: "
                + string.Join("; ", validation.Issues.Select(issue => issue.Message)));
            diagnostics.Add("Distinct Unicode translations survived build and re-extraction with stable IDs, resource paths, and locators.");
            diagnostics.Add("Resource checks passed. Game runtime compatibility has not been tested.");
            return new GeneratedAdapterVerification(true, diagnostics);
        }
        catch (Exception exception) when (exception is GeneratedAdapterException or InvalidDataException or IOException or System.Text.Json.JsonException)
        {
            diagnostics.Add(GeneratedAdapterCompiler.LimitDiagnostic(exception.Message));
            return new GeneratedAdapterVerification(false, diagnostics);
        }
    }

    private static string Normalize(string path) => path.Replace('\\', '/');

    private static void RequireSameUnits(ExtractionResult original, ExtractionResult actual, IReadOnlyDictionary<string, string> expectedTexts)
    {
        if (original.Units.Count != actual.Units.Count) throw new GeneratedAdapterException("Re-extraction dropped or added translation units.");
        var byId = actual.Units.ToDictionary(unit => unit.Id, StringComparer.Ordinal);
        foreach (var unit in original.Units)
        {
            if (!byId.TryGetValue(unit.Id, out var changed) || changed.ResourcePath != unit.ResourcePath
                || changed.Locator != unit.Locator || changed.SourceText != expectedTexts[unit.Id]
                || changed.Kind != unit.Kind || changed.Speaker != unit.Speaker)
                throw new GeneratedAdapterException("A translation unit changed its identity, location, metadata, or expected text: " + unit.Id);
        }
    }
}
