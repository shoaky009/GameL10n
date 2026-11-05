using System.Text;
using System.Text.Encodings.Web;
using System.Text.Json;
using System.Text.Json.Serialization;
using GameL10n.Core.Adapters;
using GameL10n.Core.Isolation;
using GameL10n.Core.Storage;
using GameL10n.Core.Serialization;

namespace GameL10n.Core.Development;

[JsonConverter(typeof(JsonStringEnumConverter<AdapterTestStage>))]
public enum AdapterTestStage
{
    [JsonStringEnumMemberName("compilation")] Compilation,
    [JsonStringEnumMemberName("discovery")] Discovery,
    [JsonStringEnumMemberName("verification")] Verification,
    [JsonStringEnumMemberName("finish-ready")] FinishReady
}

[JsonConverter(typeof(JsonStringEnumConverter<AdapterVerificationMilestone>))]
public enum AdapterVerificationMilestone
{
    None, Compiled, Discovery, OriginalRoundtrip, Complete
}

public sealed record AdapterTestResult(bool Passed, AdapterTestStage Stage, IReadOnlyList<string> Diagnostics,
    string? Id = null, string? Version = null, bool Reused = false,
    long CompileMs = 0, long DiscoveryMs = 0, long VerificationMs = 0)
{
    [JsonIgnore] public bool ReadyToPublish => Passed && Stage == AdapterTestStage.FinishReady;
}

/// <summary>Builds, verifies and publishes a development workspace independently of the agent tool protocol.</summary>
public sealed partial class AdapterDevelopmentWorkflow(AdaptationRequest request, AdaptationSession session, string workspace,
    AdapterRuntime runtime, LocalAdapterPackageStore store, HttpClient? researchClient = null,
    BuiltinAdapterAssets? builtinAssets = null, Action<string, string>? reportProgress = null)
{
    private static readonly JsonSerializerOptions Json = new(EngineJson.Web)
        { Encoder = JavaScriptEncoder.UnsafeRelaxedJsonEscaping };
    internal EngineReferenceToolkit EngineToolkit { get; } = new(Path.Combine(store.RootDirectory, "engine-tools"), researchClient, reportProgress);
    internal BuiltinAdapterAssets BuiltinAssets { get; } = builtinAssets ?? new();
    public GeneratedGameAdapter? PublishedAdapter { get; private set; }
    private string ResourceDirectory => Path.Combine(workspace, "resources");
    private string SourceDirectory => Path.Combine(workspace, "src");
    private static string Limit(string text, int maximum) => text.Length <= maximum ? text : text[..maximum] + "\n[truncated]";

    private void ReportVerifiedProgress(string message)
    {
        if (session.UpdateProgress(new("verified", message, DateTimeOffset.UtcNow)))
            reportProgress?.Invoke("资源验证进展", message);
    }

    private void RequireLaunchResources()
    {
        foreach (var profile in session.LaunchInspection?.LocaleProfiles ?? [])
            if (profile.ExecutableExists && session.ResourceHashes.ContainsKey(profile.Executable)
                && !session.ResourceHashes.ContainsKey(profile.Path))
                throw new InvalidDataException("所选程序有转区配置尚未纳入验证，请 stage_resources 并声明资源发现规则：" + profile.Path);
    }

    public async Task<AdapterTestResult> TestAsync(AdapterFamily family, IReadOnlyList<string> patterns,
        bool discoveryOnly, CancellationToken ct)
    {
        session.Recognition?.Validate();
        ClearVerification();
        if (session.ResourceHashes.Count == 0) throw new InvalidDataException("请先 stage_resources 选择用于验证的真实资源。");
        RequireLaunchResources();
        family.Validate();
        GeneratedAdapterResources.ValidatePatterns(patterns);
        var python = await PreparePythonReleaseAsync(ct);
        var releaseFiles = AdapterReleaseFiles.Validate(session.ReleaseFiles);
        var fingerprint = await VerificationFingerprintAsync(family, patterns, releaseFiles, discoveryOnly, ct);
        if (await ReuseVerificationAsync(fingerprint, ct) is { } reused) return reused;
        session.VerificationCache = null;
        async Task<AdapterTestResult> CompleteTestAsync(AdapterTestResult result, GeneratedAdapterPackage? package = null, string? compilationError = null, bool cacheable = true)
        {
            // Tools and external edits may change inputs during a check. Such a result is never reusable.
            if (cacheable && fingerprint is not null && fingerprint == await VerificationFingerprintAsync(family, patterns, releaseFiles, discoveryOnly, ct))
                session.VerificationCache = new(fingerprint, GeneratedAdapterVerifier.Revision, package,
                    compilationError is null ? result : null, compilationError);
            return result;
        }
        var timer = System.Diagnostics.Stopwatch.StartNew();
        GeneratedAdapterPackage package;
        try
        {
            package = await new GeneratedAdapterBuilder(runtime.Runner).BuildWorkspaceAsync(SourceDirectory,
                Path.Combine(workspace, "build"), patterns, releaseFiles, python, ct, family, session.Recognition);
        }
        catch (GeneratedAdapterException error)
        {
            await CompleteTestAsync(new(false, AdapterTestStage.Compilation, []), compilationError: error.Message);
            throw;
        }
        CheckReleaseGameInputs(package);
        session.RecordVerificationProgress(AdapterVerificationMilestone.Compiled);
        var compileMs = timer.ElapsedMilliseconds;
        timer.Restart();
        var adapter = new GeneratedGameAdapter(package, runtime.Toolchain, runtime.Runner, store.WorkDirectory, EngineToolkit, reportProgress);
        try
        {
            await CheckDiscoveryAsync(adapter, ct);
        }
        catch (Exception error) when (error is GeneratedAdapterException or InvalidDataException or IOException)
        {
            session.VerificationDiagnostics = [error.Message];
            return await CompleteTestAsync(new(false, AdapterTestStage.Discovery, session.VerificationDiagnostics,
                CompileMs: compileMs, DiscoveryMs: timer.ElapsedMilliseconds), package,
                cacheable: (error is not IOException || error is InvalidDataException) && error.InnerException is not IOException);
        }
        session.RecordVerificationProgress(AdapterVerificationMilestone.Discovery);
        var discoveryMs = timer.ElapsedMilliseconds;
        if (discoveryOnly)
        {
            session.VerificationDiagnostics = ["真实游戏与样本的探测结果一致；尚未执行提取及回填验收，不能发布。"];
            ReportVerifiedProgress("已确认适配器能识别当前游戏资源；提取和中文回填尚未验收。");
            return await CompleteTestAsync(new(true, AdapterTestStage.Discovery, session.VerificationDiagnostics, CompileMs: compileMs, DiscoveryMs: discoveryMs), package);
        }
        var verificationDirectory = WorkspaceFiles.Resolve(workspace, "verification");
        GeneratedGameAdapter.DeleteStagedTree(verificationDirectory);
        timer.Restart();
        var verification = await new GeneratedAdapterVerifier().VerifyAsync(adapter, ResourceDirectory, verificationDirectory, ct,
            sourceIsSnapshot: true, coverage: session.TextCoverage,
            baselineUnits: session.Objective == AdaptationObjective.Repair ? request.BaselineUnits : null);
        session.VerificationDiagnostics = verification.Diagnostics.ToArray();
        if (verification.OriginalRoundtripPassed)
            session.RecordVerificationProgress(AdapterVerificationMilestone.OriginalRoundtrip);
        if (verification.Passed)
        {
            session.RecordVerificationProgress(AdapterVerificationMilestone.Complete);
            ReportVerifiedProgress("原文回填、中文回填和局部试译的资源验收已通过；游戏启动与显示仍需实际验证。");
        }
        else if (verification.OriginalRoundtripPassed)
            ReportVerifiedProgress("原文回填已通过逐字节验证；中文或局部试译验收尚未通过，正在修复。");
        if (verification.Passed)
            AcceptVerification(package);
        return await CompleteTestAsync(new(verification.Passed,
            verification.Passed ? AdapterTestStage.FinishReady : AdapterTestStage.Verification,
            verification.Diagnostics, package.Id, package.Version, CompileMs: compileMs, DiscoveryMs: discoveryMs,
            VerificationMs: timer.ElapsedMilliseconds), package, cacheable: verification.Cacheable);
    }

    private string? BaselineFingerprint() => request.BaselineUnits is null ? null
        : LocalFiles.HashText(JsonSerializer.Serialize(request.BaselineUnits, EngineJson.TypeInfo<IReadOnlyList<TranslationUnit>>(Json)));

    public async Task<GeneratedGameAdapter> PublishAsync(string summary, bool allowPartial, CancellationToken ct, Func<CancellationToken, Task>? beforePublish = null)
    {
        if (request.ImproveExisting && !(session.Objective == AdaptationObjective.Repair && request.BaselineUnits is { Count: > 0 })
            && session.TextCoverage.Samples.Count == 0)
            throw new InvalidDataException("继续完善已有适配器时，请在 test_adapter 提供独立定位的 coverageSamples，确认反馈涉及的原文和字段已正确提取。");
        var package = session.VerifiedPackage ?? throw new InvalidDataException("尚无已通过验证的适配器；代码或资源变更后需要重新 test_adapter。");
        if (session.VerificationRevision != GeneratedAdapterVerifier.Revision)
            throw new InvalidDataException("已保存的验证未覆盖当前验收规则，请在现有源码上重新 test_adapter，无需重新分析或开发。");
        if (session.Objective == AdaptationObjective.Repair
            && (session.VerifiedObjective != AdaptationObjective.Repair || session.VerifiedBaselineFingerprint != BaselineFingerprint()))
            throw new InvalidDataException("已保存的验证未核对当前修复任务的原有文本基线，请重新 test_adapter；无需重新开发或扩展覆盖。");
        if (session.TextCoverage.Samples.Count > 0 && session.VerifiedCoverageFingerprint != session.TextCoverage.Fingerprint())
            throw new InvalidDataException("文字覆盖样本已更新，请 test_adapter 核对实际提取结果。");
        var partial = session.TextCoverage.Pending.Count > 0;
        if (partial && !allowPartial)
            throw new InvalidDataException((session.Objective == AdaptationObjective.Repair
                ? "本次修复可保留其他范围的已知覆盖遗漏；请 allowPartial=true，并在 summary 说明这些剩余限制，无需自动扩展其他类别。已知遗漏："
                : "仍有已知文字覆盖待办，请继续处理。确实暂时无法完成时可 allowPartial=true 发布部分结果，并在 summary 说明原因。待办：")
                + string.Join("；", session.TextCoverage.Pending));
        summary = summary.Trim();
        if (summary.Length == 0) throw new InvalidDataException("请在 summary 说明本次完成内容及剩余限制。");
        // Local tools may edit sources or only inspect/report. Compare the actual compilation inputs
        // at publication, rather than making every probe invalidate a successful roundtrip.
        await CheckVerifiedSourcesAsync(package, ct);
        CheckReleaseGameInputs(package);
        RequireLaunchResources();
        await CheckResourcesAsync(ct);
        var probe = await CheckDiscoveryAsync(new GeneratedGameAdapter(package, runtime.Toolchain, runtime.Runner, store.WorkDirectory, EngineToolkit, reportProgress), ct);
        if (beforePublish is not null) await beforePublish(ct);
        var published = await store.PublishAndActivateAsync(request.GameId, package, session.VerificationDiagnostics, ct);
        session.VerifiedPackage = published;
        await store.RecordCompatibilityAsync(package.Family!, new(request.GameId,
            Path.GetFileName(Path.TrimEndingDirectorySeparator(request.GameDirectory)), probe.Description, package.Version,
            "resource-roundtrip-passed"), ct);
        session.Summary = (partial ? "已发布部分文字支持；仍有未覆盖内容。\n" : "") + Limit(summary, 2000)
            + (partial ? "\n已知未覆盖内容：" + string.Join("；", session.TextCoverage.Pending) : "")
            + (session.TextCoverage.RuntimeChecks.Count > 0 ? "\n运行效果待验证：" + string.Join("；", session.TextCoverage.RuntimeChecks) : "");
        session.Stage = partial ? "部分文字可试译" : "适配完成";
        PublishedAdapter = new GeneratedGameAdapter(published, runtime.Toolchain, runtime.Runner, store.WorkDirectory, EngineToolkit, reportProgress);
        return PublishedAdapter;
    }

    private async Task CheckVerifiedSourcesAsync(GeneratedAdapterPackage package, CancellationToken ct)
    {
        if (package.ManifestVersion < 3 || package.ReleaseFiles is null)
            throw new InvalidDataException("旧验证结果未声明正式包文件，请沿用现有源码并通过 releaseFiles 重新 test_adapter，无需重新分析引擎。");
        var releaseFiles = AdapterReleaseFiles.Validate(package.ReleaseFiles);
        var unchanged = true;
        foreach (var file in releaseFiles)
        {
            var input = AdapterReleaseFiles.SourcePath(SourceDirectory, file);
            if (package.PackageFiles is null || !package.PackageFiles.TryGetValue(file.Path, out var hash)
                || !File.Exists(input) || await LocalFiles.HashFileAsync(input, ct) != hash)
            { unchanged = false; break; }
        }
        if (!unchanged)
        {
            session.VerifiedPackage = null;
            throw new InvalidDataException("适配源码或依赖与已验证版本不同，请重新 test_adapter；分析报告和笔记不影响验证。");
        }
    }

    private async Task<GameProbe> CheckDiscoveryAsync(GeneratedGameAdapter adapter, CancellationToken ct)
    {
        if (adapter.Package.Recognition is not null)
        {
            var inventory = GameDiscoverySession.Scan(request.GameDirectory, ct);
            var recognition = await inventory.EvaluateAsync(new(adapter.Package.ResourceFiles,
                adapter.Package.ResourcePatterns, adapter.Package.Recognition), content: true, ct);
            if (recognition.Status != RecognitionStatus.Match)
                throw new InvalidDataException("识别声明未在真实游戏上确认，请修正必要条件或签名；不确定的文件特征应放入 hints。识别声明可省略。");
        }
        var gameCandidates = adapter.DiscoverResources(request.GameDirectory);
        var sampleCandidates = adapter.DiscoverResources(ResourceDirectory);
        var candidateDifferences = Differences(gameCandidates, sampleCandidates);
        try
        {
            var sample = await adapter.ProbeAsync(ResourceDirectory, ct)
                ?? throw new InvalidDataException("适配器未识别暂存样本。probe 只获得文件前 64KB，请检查头部识别条件；完整解析应在 extract/build 阶段执行。");
            var actual = await adapter.ProbeAsync(request.GameDirectory, ct)
                ?? throw new InvalidDataException("适配器未识别真实游戏目录。请检查其他 EXE、BIN 或封包是否影响候选选择；完整解析不能放在只提供前 64KB 的 probe 中。");
            var requiredDifferences = Differences(actual.RequiredFiles, sample.RequiredFiles);
            if (requiredDifferences.Length > 0)
                throw new InvalidDataException("真实游戏与样本的必需资源不一致：" + requiredDifferences
                    + "。请暂存缺少的必需资源，或修正资源规则及 probe 筛选；不要为通过校验直接包含无关文件。");
            if (!string.Equals(actual.OriginalExecutable?.Replace('\\', '/'), sample.OriginalExecutable?.Replace('\\', '/'), StringComparison.OrdinalIgnoreCase))
                throw new InvalidDataException($"启动入口不一致：游戏={actual.OriginalExecutable ?? "未指定"}，样本={sample.OriginalExecutable ?? "未指定"}。请按引擎结构区分原启动器和设置/卸载程序。");
            return sample;
        }
        catch (Exception error) when (error is GeneratedAdapterException or InvalidDataException or IOException)
        {
            throw new InvalidDataException(error.Message + (candidateDifferences.Length == 0 ? "" : "\n资源规则匹配的候选差异：" + candidateDifferences), error);
        }

        static string Differences(IEnumerable<string> actual, IEnumerable<string> sample)
        {
            var game = actual.Select(path => path.Replace('\\', '/')).ToHashSet(StringComparer.OrdinalIgnoreCase);
            var staged = sample.Select(path => path.Replace('\\', '/')).ToHashSet(StringComparer.OrdinalIgnoreCase);
            var missing = game.Except(staged, StringComparer.OrdinalIgnoreCase).Order(StringComparer.Ordinal).ToArray();
            var extra = staged.Except(game, StringComparer.OrdinalIgnoreCase).Order(StringComparer.Ordinal).ToArray();
            var parts = new List<string>();
            if (missing.Length > 0) parts.Add($"样本缺少 {missing.Length} 项：" + string.Join("、", missing.Take(12)));
            if (extra.Length > 0) parts.Add($"样本多出 {extra.Length} 项：" + string.Join("、", extra.Take(12)));
            return string.Join("；", parts);
        }
    }

    private async Task CheckResourcesAsync(CancellationToken ct)
    {
        foreach (var (relative, hash) in session.ResourceHashes)
        {
            if (await LocalFiles.HashFileAsync(WorkspaceFiles.Resolve(request.GameDirectory, relative), ct) != hash
                || await LocalFiles.HashFileAsync(WorkspaceFiles.Resolve(ResourceDirectory, relative), ct) != hash)
                throw new InvalidDataException("原资源或工作副本发生变化，请重新 stage_resources 并验证：" + relative);
        }
    }

}
