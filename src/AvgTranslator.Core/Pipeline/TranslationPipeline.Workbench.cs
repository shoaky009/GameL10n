using System.Text.Json;
using AvgTranslator.Core.Patching;
using AvgTranslator.Core.Storage;
using AvgTranslator.Core.Translation;

namespace AvgTranslator.Core.Pipeline;

public sealed record PatchTranslationSnapshot(int SchemaVersion, IReadOnlyDictionary<string, string> Texts);

public sealed partial class TranslationPipeline
{
    public async Task<PipelineResult> BuildFromSavedAsync(GameSession session, TranslationPolicy policy,
        IReadOnlyList<string>? unitIds = null, bool reviewedOnly = false,
        IProgress<PipelineProgress>? progress = null, CancellationToken ct = default)
    {
        await using var projectLock = AcquireProjectLock(session.ProjectDirectory);
        var memory = new TranslationMemory(Path.Combine(session.ProjectDirectory, "memory"), Path.Combine(DataDirectory, "memory", session.Id));
        await memory.InitializeAsync(ct);
        var edits = await TranslationEdits.LoadAsync(session.ProjectDirectory, ct);
        var selection = unitIds?.ToHashSet(StringComparer.Ordinal);
        if (selection is not null && (selection.Count == 0 || !selection.IsSubsetOf(session.Extraction.Units.Select(unit => unit.Id))))
            throw new ArgumentException("补丁选区为空或已失效，请重新选择文本。", nameof(unitIds));
        var completed = new Dictionary<string, string>(StringComparer.Ordinal);
        foreach (var unit in session.Extraction.Units)
        {
            ct.ThrowIfCancellationRequested();
            if (selection is not null && !selection.Contains(unit.Id)) continue;
            var edit = edits.Find(unit, policy.TargetLanguage);
            if (reviewedOnly && edit?.Reviewed != true) continue;
            var text = edit?.Text ?? await memory.FindAsync(unit, policy, ct);
            if (text is not null) completed.Add(unit.Id, text);
        }
        if (completed.Count == 0) throw new InvalidOperationException("所选范围还没有可用于补丁的译文。");
        return await BuildSnapshotAsync(session, completed, 0, new(0, 0), progress, ct);
    }

    public async Task<string> CreateCandidateAsync(GameSession session, TranslationUnit unit,
        IReadOnlyList<ITranslationProvider> providers, TranslationPolicy policy,
        IProgress<PipelineProgress>? progress = null, CancellationToken ct = default)
    {
        string? candidate = null;
        await RunCoreAsync(session, providers, policy, new(1, TrialUnitIds: [unit.Id])
        {
            CandidateOnly = true,
            TranslationUpdates = new InlineTranslations(values => { if (values.TryGetValue(unit.Id, out var text)) candidate = text; })
        }, progress, false, ct);
        return candidate ?? throw new InvalidDataException("模型未返回候选译文。");
    }

    private sealed class InlineTranslations(Action<IReadOnlyDictionary<string, string>> report) : IProgress<IReadOnlyDictionary<string, string>>
    {
        public void Report(IReadOnlyDictionary<string, string> value) => report(value);
    }

    private async Task<PipelineResult> BuildSnapshotAsync(GameSession session, Dictionary<string, string> completed,
        int cacheHits, TokenUsage usage, IProgress<PipelineProgress>? progress, CancellationToken ct)
    {
        var allUnits = session.Extraction.Units;
        var units = allUnits.Where(unit => completed.ContainsKey(unit.Id)).ToArray();
        foreach (var unit in units)
        {
            var issues = ValidateTranslation(session.Adapter, unit, completed[unit.Id]);
            if (issues.Count > 0) throw new InvalidDataException($"{unit.ResourcePath} · {unit.Locator}：{issues[0].Message} 请修正译文或从补丁范围中排除该文本。");
        }
        var isTrial = completed.Count != allUnits.Count;
        var trialIds = isTrial ? completed.Keys.ToHashSet(StringComparer.Ordinal) : null;
        Task ReportAsync(string stage, string message, CancellationToken token)
        {
            progress?.Report(new(stage, completed.Count, allUnits.Count, cacheHits, usage.TotalTokens, message));
            return Task.CompletedTask;
        }
        await VerifySourcesAsync(session.SourceDirectory, session.Sources, ct);
        // Original-text fallbacks belong only to the patch; they are never committed as translations.
        var buildTexts = isTrial
            ? allUnits.ToDictionary(u => u.Id, u => completed.GetValueOrDefault(u.Id, u.SourceText), StringComparer.Ordinal)
            : completed;
        var buildKey = LocalFiles.HashText(JsonSerializer.Serialize(new
        {
            SnapshotVersion = 2, session.Adapter.Id, session.Adapter.Version, session.Sources, isTrial,
            TrialUnitIds = trialIds?.Order(StringComparer.Ordinal).ToArray(),
            Translations = buildTexts.OrderBy(k => k.Key, StringComparer.Ordinal)
        }));
        var patchDirectory = Path.Combine(session.ProjectDirectory, "patches", buildKey);
        var reportPath = Path.Combine(patchDirectory, "translation-report.json");
        var existing = await LocalFiles.ReadRecoverableJsonAsync<PipelineResult>(reportPath, ct);
        if (existing is not null) existing = ResolveResult(session.ProjectDirectory, existing);
        if (existing is not null && await PatchIsIntactAsync(patchDirectory, ct))
        {
            var manifest = await LocalFiles.ReadJsonAsync<PatchManifest>(Path.Combine(patchDirectory, PatchInstaller.ManifestFilename), ct);
            var executableCheck = await ValidateExecutablesAsync(session, patchDirectory, manifest!.Files.Select(file => file.RelativePath).ToArray(), ct);
            if (!executableCheck.Passed)
                throw new InvalidDataException("已有补丁未通过程序加载检查：" + string.Join("；", executableCheck.Issues.Select(issue => issue.Message)));
            await RecordCompatibilityAsync(session, existing, ct);
            var reused = existing with { CacheHits = cacheHits, TranslationUsage = usage };
            await LocalFiles.WriteJsonAsync(Path.Combine(session.ProjectDirectory, "latest-result.json"), PersistedResult(session.ProjectDirectory, reused), ct);
            await ReportAsync(isTrial ? "试译完成" : "完成", isTrial
                ? "已复用试译补丁。可安装后进入所选片段检查显示效果；其余文本保留原文。"
                : "译文和补丁均未变化，直接复用已完成结果。", ct);
            return reused;
        }
        await ReportAsync("生成补丁", "正在回填译文并生成游戏目录所需文件。", ct);
        var staging = patchDirectory + ".pending-" + Guid.NewGuid().ToString("N");
        Directory.CreateDirectory(staging);
        var build = await session.Adapter.BuildPatchAsync(session.SourceDirectory, buildTexts, staging, ct);
        if (build.Files.Count == 0) throw new InvalidDataException("Adapter 未生成补丁文件。");
        if (build.Files.Distinct(StringComparer.OrdinalIgnoreCase).Count() != build.Files.Count)
            throw new InvalidDataException("Adapter 生成了重复的文件路径。");
        await ReportAsync("校验补丁", "正在校验资源结构、文本和文件哈希。", ct);
        var validationResult = await session.Adapter.ValidatePatchAsync(session.SourceDirectory, staging, ct);
        var executableValidation = await ValidateExecutablesAsync(session, staging, build.Files, ct);
        validationResult = validationResult with
        {
            Passed = validationResult.Passed && executableValidation.Passed,
            Issues = validationResult.Issues.Concat(executableValidation.Issues).ToArray()
        };
        if (!validationResult.Passed)
        {
            await LocalFiles.WriteJsonAsync(Path.Combine(staging, "validation-errors.json"), validationResult, ct);
            throw new InvalidDataException("补丁未通过结构校验：" + string.Join("；", validationResult.Issues.Select(i => i.Message).Take(5)));
        }
        var patchFiles = new List<PatchFile>();
        foreach (var relative in build.Files)
            patchFiles.Add(new(relative, await LocalFiles.HashFileAsync(LocalFiles.UnderRoot(staging, relative), ct),
                session.Sources.FirstOrDefault(f => f.RelativePath.Equals(relative, StringComparison.OrdinalIgnoreCase))?.Sha256));
        await LocalFiles.WriteJsonAsync(Path.Combine(staging, PatchInstaller.ManifestFilename), new PatchManifest(1, patchFiles), ct);
        var result = new PipelineResult(patchDirectory, build.LaunchExecutable ?? session.Probe.OriginalExecutable,
            allUnits.Count, cacheHits, usage, validationResult,
            session.Extraction.Warnings.Concat(build.Warnings).Distinct().ToArray(), Path.GetFileName(session.SourceDirectory),
            isTrial, completed.Count);
        await LocalFiles.WriteJsonAsync(Path.Combine(staging, "translation-snapshot.json"), new PatchTranslationSnapshot(1, completed), ct);
        await LocalFiles.WriteJsonAsync(Path.Combine(staging, "translation-report.json"), PersistedResult(session.ProjectDirectory, result), ct);
        if (Directory.Exists(patchDirectory))
            Directory.Move(patchDirectory, patchDirectory + ".damaged-" + Guid.NewGuid().ToString("N"));
        Directory.Move(staging, patchDirectory);
        await LocalFiles.WriteJsonAsync(Path.Combine(session.ProjectDirectory, "latest-result.json"), PersistedResult(session.ProjectDirectory, result), ct);
        await RecordCompatibilityAsync(session, result, ct);
        await ReportAsync(isTrial ? "试译完成" : "完成", isTrial
            ? $"已生成 {completed.Count} 条文本的试译补丁，其余文本保留原文。请在“补丁与验证”安装后进入所选片段验证。"
            : "汉化补丁已生成。结构校验通过；游戏运行效果尚未验证。", ct);
        return result;
    }
}
