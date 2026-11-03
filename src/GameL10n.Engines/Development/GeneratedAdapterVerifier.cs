using System.Text;
using System.Text.Encodings.Web;
using System.Text.Json;
using GameL10n.Core.Isolation;

namespace GameL10n.Core.Development;

public sealed class GeneratedAdapterVerifier
{
    public const int Revision = 4;
    private static readonly JsonSerializerOptions DiagnosticJson = new() { Encoder = JavaScriptEncoder.UnsafeRelaxedJsonEscaping };

    /// <summary>Checks host-observed bytes and extraction; an extension's validation claim is never sufficient.</summary>
    public async Task<GeneratedAdapterVerification> VerifyAsync(GeneratedGameAdapter adapter, string sampleDirectory,
        string verificationDirectory, CancellationToken cancellationToken = default, bool sourceIsSnapshot = false,
        TextCoverage? coverage = null, IReadOnlyList<TranslationUnit>? baselineUnits = null, bool requireExactBaseline = false)
    {
        if (Directory.Exists(verificationDirectory) && Directory.EnumerateFileSystemEntries(verificationDirectory).Any())
            throw new InvalidDataException("适配验证需要空的临时目录。");
        Directory.CreateDirectory(verificationDirectory);
        var diagnostics = new List<string>();
        var phase = "probe";
        var originalRoundtripPassed = false;
        try
        {
            var probe = await adapter.ProbeAsync(sampleDirectory, cancellationToken);
            if (probe is null) throw new GeneratedAdapterException("The generated adapter did not recognize its own resource samples.");
            foreach (var executable in probe.RequiredFiles.Where(path => path.EndsWith(".exe", StringComparison.OrdinalIgnoreCase)))
            {
                var profile = executable + ".le.config";
                if (File.Exists(WorkspaceFiles.Resolve(sampleDirectory, profile))
                    && !probe.RequiredFiles.Select(Normalize).Contains(Normalize(profile), StringComparer.OrdinalIgnoreCase))
                    throw new GeneratedAdapterException("原程序的转区配置未纳入适配器资源，请补全 resourcePatterns 和 probe.requiredFiles：" + profile);
            }
            // Reuse verification may begin at a game directory. Full operations only receive this private copy.
            phase = "source-snapshot";
            var source = sourceIsSnapshot ? Path.GetFullPath(sampleDirectory) : WorkspaceFiles.Resolve(verificationDirectory, "source");
            if (!sourceIsSnapshot)
            {
                Directory.CreateDirectory(source);
                foreach (var resource in probe.RequiredFiles)
                {
                    var original = WorkspaceFiles.Resolve(sampleDirectory, resource);
                    var staged = WorkspaceFiles.Resolve(source, resource);
                    await GeneratedAdapterResources.CopyAsync(original, staged, cancellationToken);
                    if (GeneratedAdapterResources.HashFile(original) != GeneratedAdapterResources.HashFile(staged))
                        throw new IOException("A resource changed while it was being copied for verification: " + resource);
                }
            }
            var resources = adapter.DiscoverResources(source);
            phase = "initial-extraction";
            var first = await adapter.ExtractAsync(source, cancellationToken);
            if (baselineUnits is { Count: > 0 })
            {
                phase = "existing-text-compatibility";
                if (requireExactBaseline && first.Units.Count != baselineUnits.Count)
                    throw new GeneratedAdapterException($"发布包提取数量与已有记录不同：已有 {baselineUnits.Count} 条，本次 {first.Units.Count} 条。");
                RequireCompatibleBaseline(baselineUnits, first);
                diagnostics.Add($"已核对 {baselineUnits.Count} 条已有文本的身份与缓存字段，未改变已有译文和校对记录的匹配依据。");
            }
            if (coverage is not null)
            {
                var missing = coverage.Check(first);
                if (missing.Count > 0) throw new GeneratedAdapterException(string.Join("\n", missing.Take(8)));
                diagnostics.Add($"已核对 {coverage.Samples.Count} 个独立原文样本；仍有 {coverage.Pending.Count} 项已知覆盖待办。样本通过不代表全游戏文字完整。");
            }
            phase = "repeated-extraction";
            var repeated = await adapter.ExtractAsync(source, cancellationToken);
            RequireSameUnits(first, repeated, first.Units.ToDictionary(unit => unit.Id, unit => unit.SourceText));
            phase = "original-byte-roundtrip";
            var originalTexts = first.Units.ToDictionary(unit => unit.Id, unit => unit.SourceText, StringComparer.Ordinal);
            var roundtrip = WorkspaceFiles.Resolve(verificationDirectory, "roundtrip");
            Directory.CreateDirectory(roundtrip);
            var built = await adapter.BuildPatchAsync(source, originalTexts, roundtrip, cancellationToken);
            var expectedFiles = resources.Select(Normalize).ToHashSet(StringComparer.OrdinalIgnoreCase);
            if (!expectedFiles.IsSubsetOf(built.Files.Select(Normalize)))
                throw new GeneratedAdapterException("Original-text roundtrip must reproduce every discovered source resource.");
            foreach (var resource in resources)
            {
                var before = GeneratedAdapterResources.HashFile(WorkspaceFiles.Resolve(source, resource));
                var after = GeneratedAdapterResources.HashFile(WorkspaceFiles.Resolve(roundtrip, resource));
                if (before != after) throw new GeneratedAdapterException("Original-text roundtrip changed resource bytes: " + resource);
            }
            diagnostics.Add("Original-text build reproduced every approved sample byte for byte.");
            originalRoundtripPassed = true;

            // Change visible text, including duplicate strings, without moving resource syntax.
            phase = "full-translation-build";
            var samples = first.Units.Select((unit, index) => (Unit: unit, Index: index,
                Text: VerificationTranslationText.Create(unit, index))).ToArray();
            var mutableSamples = samples.Where(sample => sample.Text != sample.Unit.SourceText).ToArray();
            if (mutableSamples.Length == 0)
                throw new GeneratedAdapterException("所有提取文本均为保护标记或空白，无法生成有效测试译文；请提取可翻译的可见文字。");
            var translatedTexts = samples.ToDictionary(sample => sample.Unit.Id, sample => sample.Text, StringComparer.Ordinal);
            var translated = WorkspaceFiles.Resolve(verificationDirectory, "translated");
            Directory.CreateDirectory(translated);
            var translatedBuild = await adapter.BuildPatchAsync(source, translatedTexts, translated, cancellationToken);
            if (!expectedFiles.IsSubsetOf(translatedBuild.Files.Select(Normalize)))
                throw new GeneratedAdapterException("Translated build dropped a discovered source resource.");
            foreach (var profile in resources.Where(path => path.EndsWith(".exe.le.config", StringComparison.OrdinalIgnoreCase)))
                if (GeneratedAdapterResources.HashFile(WorkspaceFiles.Resolve(source, profile))
                    != GeneratedAdapterResources.HashFile(WorkspaceFiles.Resolve(translated, profile)))
                    throw new GeneratedAdapterException("中文回填修改了原程序的转区配置，请保留原配置，并为新的启动入口单独处理转区：" + profile);
            phase = "full-translation-re-extraction";
            var extractedTranslation = await adapter.ExtractAsync(translated, cancellationToken);
            RequireSameUnits(first, extractedTranslation, translatedTexts);
            phase = "full-translation-validation";
            var validation = await adapter.ValidatePatchAsync(source, translated, cancellationToken);
            if (!validation.Passed) throw new GeneratedAdapterException("The extension rejected the independently checked translated sample: "
                + string.Join("; ", validation.Issues.Select(issue => issue.Message)));
            diagnostics.Add($"{mutableSamples.Length} distinct Unicode translations survived build and re-extraction with stable IDs, resource paths, and locators."
                + $" {samples.Length - mutableSamples.Length} units containing only protected syntax or spacing were checked unchanged.");

            // Trials supply only selected translations. Exercise that path and observe untouched units in the rebuilt resources.
            phase = "partial-translation-build";
            var partialTexts = new Dictionary<string, string>(StringComparer.Ordinal);
            var firstSample = mutableSamples[0];
            partialTexts[firstSample.Unit.Id] = VerificationTranslationText.Create(firstSample.Unit, firstSample.Index, trial: true);
            if (mutableSamples.Length > 3)
            {
                var middle = mutableSamples[mutableSamples.Length / 2];
                partialTexts[middle.Unit.Id] = VerificationTranslationText.Create(middle.Unit, middle.Index, trial: true);
            }
            var expectedPartial = new Dictionary<string, string>(originalTexts, StringComparer.Ordinal);
            foreach (var pair in partialTexts) expectedPartial[pair.Key] = pair.Value;
            var partialDirectory = WorkspaceFiles.Resolve(verificationDirectory, "partial");
            Directory.CreateDirectory(partialDirectory);
            var partialBuild = await adapter.BuildPatchAsync(source, partialTexts, partialDirectory, cancellationToken);
            if (!expectedFiles.IsSubsetOf(partialBuild.Files.Select(Normalize)))
                throw new GeneratedAdapterException("Partial translation build dropped a discovered source resource.");
            phase = "partial-translation-re-extraction";
            RequireSameUnits(first, await adapter.ExtractAsync(partialDirectory, cancellationToken), expectedPartial);
            phase = "partial-translation-validation";
            var partialValidation = await adapter.ValidatePatchAsync(source, partialDirectory, cancellationToken);
            if (!partialValidation.Passed)
                throw new GeneratedAdapterException("Partial translation validation failed: "
                    + string.Join("; ", partialValidation.Issues.Select(issue => issue.Message)));
            diagnostics.Add("Partial trial edits survived re-extraction; every untouched translation unit retained its original text and identity.");
            diagnostics.Add("Resource checks passed. Game runtime compatibility has not been tested.");
            return new GeneratedAdapterVerification(true, diagnostics, originalRoundtripPassed);
        }
        catch (Exception exception) when (exception is GeneratedAdapterException or InvalidDataException or IOException or System.Text.Json.JsonException)
        {
            diagnostics.Add(GeneratedAdapterCompiler.LimitDiagnostic($"[{phase}] {exception.Message}"));
            return new GeneratedAdapterVerification(false, diagnostics, originalRoundtripPassed,
                Cacheable: exception is not IOException || exception is InvalidDataException);
        }
        finally { GeneratedGameAdapter.DeleteStagedTree(verificationDirectory); }
    }

    private static string Normalize(string path) => path.Replace('\\', '/');

    internal static void RequireCompatibleBaseline(IReadOnlyList<TranslationUnit> baseline, ExtractionResult actual)
    {
        // Additional text may already have been implemented before a repair was scoped. Keep it,
        // but never silently discard or rename existing units and invalidate committed translations.
        var byId = actual.Units.ToDictionary(unit => unit.Id, StringComparer.Ordinal);
        var incompatible = 0;
        var details = new List<string>(2);
        foreach (var before in baseline)
        {
            if (!byId.TryGetValue(before.Id, out var after))
            {
                incompatible++;
                if (details.Count < 2) details.Add("缺少已有文本：" + DescribeUnit(before));
                continue;
            }
            var fields = new List<string>();
            if (before.ResourcePath != after.ResourcePath) fields.Add("resourcePath");
            if (before.Locator != after.Locator) fields.Add("locator");
            if (before.SourceText != after.SourceText) fields.Add("sourceText");
            if (before.Context != after.Context) fields.Add("context");
            if (before.Speaker != after.Speaker) fields.Add("speaker");
            if (before.Kind != after.Kind) fields.Add("kind");
            if (!SameProtectedTokens(before, after)) fields.Add("protectedTokens");
            if (fields.Count == 0) continue;
            incompatible++;
            if (details.Count < 2) details.Add("已有文本字段改变（" + string.Join(", ", fields) + "）：" + DescribeUnit(before));
        }
        if (incompatible == 0) return;
        throw new GeneratedAdapterException(GeneratedAdapterCompiler.LimitDiagnostic(
            $"本次适配改变了 {incompatible} / {baseline.Count} 条已有文本的身份或缓存字段，不能直接复用已有译文与校对记录。"
            + "保留原 ID、位置、原文、上下文、说话人、类型及保护标记，在适配器内部维护新的编码/槽位映射。"
            + "使用 inspect_existing_text 分页读取原字段，不要靠原文相似度合并。\n" + string.Join("\n", details)));
    }

    private static void RequireSameUnits(ExtractionResult original, ExtractionResult actual, IReadOnlyDictionary<string, string> expectedTexts)
    {
        const int sampleLimit = 2;
        var byId = actual.Units.ToDictionary(unit => unit.Id, StringComparer.Ordinal);
        var missing = new List<TranslationUnit>(sampleLimit);
        var changedSamples = new List<(TranslationUnit Before, TranslationUnit After)>(sampleLimit);
        var missingCount = 0;
        var changedCount = 0;
        foreach (var unit in original.Units)
        {
            if (!byId.Remove(unit.Id, out var changed))
            {
                missingCount++;
                if (missing.Count < sampleLimit) missing.Add(unit);
                continue;
            }
            if (changed.ResourcePath != unit.ResourcePath
                || changed.Locator != unit.Locator || changed.SourceText != expectedTexts[unit.Id]
                || changed.Kind != unit.Kind || changed.Speaker != unit.Speaker
                || !SameProtectedTokens(unit, changed))
            {
                changedCount++;
                if (changedSamples.Count < sampleLimit) changedSamples.Add((unit, changed));
            }
        }
        if (missingCount == 0 && changedCount == 0 && byId.Count == 0) return;

        var message = new StringBuilder($"Re-extraction mismatch: expected={original.Units.Count}, actual={actual.Units.Count}; "
            + $"missing={missingCount}, added={byId.Count}, changed={changedCount}. Showing up to {sampleLimit} samples per category.\n");
        // Shared IDs can shift before the added/dropped tail. Report those differences first.
        foreach (var (before, after) in changedSamples)
        {
            message.AppendLine("Changed " + DescribeUnit(before));
            message.AppendLine($"  suppliedTranslation={Sample(expectedTexts[before.Id])}, extractedText={Sample(after.SourceText)}");
            foreach (var (field, expected, observed) in new[]
            {
                ("sourceText", expectedTexts[before.Id], after.SourceText),
                ("resourcePath", before.ResourcePath, after.ResourcePath),
                ("locator", before.Locator, after.Locator),
                ("kind", before.Kind, after.Kind),
                ("speaker", before.Speaker, after.Speaker)
            })
                if (expected != observed)
                    message.AppendLine($"  {field}: expected={Sample(expected)}, actual={Sample(observed)}");
            if (!SameProtectedTokens(before, after))
            {
                var removed = (before.ProtectedTokens ?? []).Except(after.ProtectedTokens ?? [], StringComparer.Ordinal);
                var added = (after.ProtectedTokens ?? []).Except(before.ProtectedTokens ?? [], StringComparer.Ordinal);
                message.AppendLine($"  protectedTokens: missing=[{string.Join(", ", removed.Take(4).Select(Sample))}], "
                    + $"added=[{string.Join(", ", added.Take(4).Select(Sample))}] (up to 4 each)");
            }
        }
        foreach (var unit in missing)
        {
            message.AppendLine("Missing " + DescribeUnit(unit));
            message.AppendLine("  suppliedTranslation=" + Sample(expectedTexts[unit.Id]));
        }
        foreach (var unit in byId.Values.Take(sampleLimit)) message.AppendLine("Added " + DescribeUnit(unit));
        throw new GeneratedAdapterException(GeneratedAdapterCompiler.LimitDiagnostic(message.ToString()));
    }

    private static bool SameProtectedTokens(TranslationUnit before, TranslationUnit after) =>
        (before.ProtectedTokens ?? []).Distinct(StringComparer.Ordinal).Order(StringComparer.Ordinal)
            .SequenceEqual((after.ProtectedTokens ?? []).Distinct(StringComparer.Ordinal).Order(StringComparer.Ordinal));

    private static string DescribeUnit(TranslationUnit unit) =>
        $"id={Sample(unit.Id)}, resourcePath={Sample(unit.ResourcePath)}, locator={Sample(unit.Locator)}, "
        + $"sourceText={Sample(unit.SourceText)}, context={Sample(unit.Context)}";

    private static string Sample(string? value) =>
        JsonSerializer.Serialize(value is { Length: > 160 } ? value[..160] + "…" : value, DiagnosticJson);
}
