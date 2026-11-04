using System.Diagnostics;
using System.Text.Json;
using GameL10n.Core.Adapters;
using GameL10n.Core.Isolation;
using GameL10n.Core.Storage;

namespace GameL10n.Core.Development;

public enum AdapterDiscoveryMode { Analysis, Reuse }

public sealed record AdapterDiscoveryRequest(string GameId, string GameDirectory, IReadOnlyList<IGameAdapter> ExistingAdapters,
    AdapterDiscoveryMode Mode = AdapterDiscoveryMode.Reuse, string? PinnedAdapterId = null, string? PinnedAdapterVersion = null,
    IReadOnlyList<GeneratedAdapterReuseFailure>? PreviousFailures = null);

public sealed record AdapterDiscoveryResult(IGameAdapter? Adapter, GameProbe? Probe,
    IReadOnlyList<BuiltinAdapterProbeObservation> BuiltinProbes, IReadOnlyList<GeneratedAdapterReuseFailure> Failures,
    IReadOnlyList<string> Diagnostics);

public sealed record GeneratedAdapterReuseFailure(string Id, string Version, string FailedStage,
    IReadOnlyList<string> Evidence, string? SourceDirectory, bool OriginalRoundtripPassed = false, string? InputFingerprint = null);

/// <summary>Candidate selection, resource acceptance and reuse evidence. Project snapshots belong to the caller.</summary>
public sealed class AdapterDiscoveryService(LocalAdapterPackageStore store, Func<PinnedDotNetToolchain> toolchain,
    IAdapterProcessRunner runner, EngineReferenceToolkit toolkit)
{
    public async Task<AdapterDiscoveryResult> FindAsync(AdapterDiscoveryRequest request, CancellationToken ct = default,
        Func<IGameAdapter, GameProbe, CancellationToken, Task<string>>? prepareSources = null,
        Action<string, string>? progress = null, Action<string>? log = null, Func<object, Task>? trace = null)
    {
        var watch = Stopwatch.StartNew();
        var diagnostics = new List<string>();
        var probes = new List<BuiltinAdapterProbeObservation>();
        var failures = new List<GeneratedAdapterReuseFailure>();
        void Diagnose(string message) { diagnostics.Add(message); log?.Invoke(message); }
        Task TraceAsync(object detail) => trace?.Invoke(detail) ?? Task.CompletedTask;
        var selectionTrace = new AdapterOperationTrace(trace, "discovery-phase");
        AdapterDiscoveryResult Result(IGameAdapter? adapter = null, GameProbe? probe = null) =>
            new(adapter, probe, probes.ToArray(), failures.ToArray(), diagnostics.ToArray());

        progress?.Invoke("扫描游戏目录", "正在收集资源路径和文件大小。");
        var inventory = await selectionTrace.MeasureAsync("scan", () => Task.FromResult(GameDiscoverySession.Scan(request.GameDirectory, ct)));
        var indexed = await selectionTrace.MeasureAsync("index", () => store.ReadDiscoveryCandidatesAsync(diagnostics, ct));
        var reusable = await selectionTrace.MeasureAsync("selection", () => AdapterDiscoveryPlanner.SelectAsync(inventory, indexed, ct));
        foreach (var item in inventory.Diagnostics) diagnostics.Add(item);
        foreach (var item in diagnostics) log?.Invoke(item);
        var disabled = store.ReadDisabledFamilyIds();
        var analysis = request.Mode == AdapterDiscoveryMode.Analysis;
        var active = analysis ? await selectionTrace.MeasureAsync("active-package", () => Task.FromResult(store.LoadActive(request.GameId))) : null;
        bool CanMatch(IGameAdapter adapter) => !disabled.Contains(adapter.CatalogEntry?.Family.AdapterId ?? adapter.Id)
            || analysis && request.PinnedAdapterId == adapter.Id && request.PinnedAdapterVersion == adapter.Version;
        var candidates = new List<Candidate>();
        var keys = new HashSet<(string, string)>();
        void Add(Candidate candidate) { if (keys.Add((candidate.Id, candidate.Version))) candidates.Add(candidate); }
        void AddAdapter(IGameAdapter adapter, string origin)
        {
            if (CanMatch(adapter)) Add(new(adapter.Id, adapter.Version, adapter.CatalogEntry?.Family.Name ?? adapter.Id,
                () => adapter, adapter is GeneratedGameAdapter, origin,
                adapter is GeneratedGameAdapter generated ? Descriptor(generated.Package) : null));
        }
        if (active is not null)
            Add(new(active.Id, active.Version, active.Family?.Name ?? active.Id,
                () => new GeneratedGameAdapter(active, toolchain(), runner, store.WorkDirectory, toolkit, progress, inventory), true,
                "project-active", Descriptor(active)));
        foreach (var adapter in request.ExistingAdapters.Where(adapter => adapter is not VnTextPatchAdapter)) AddAdapter(adapter, "registered");
        AddAdapter(new SiglusAdapter(toolkit, runner, progress), "builtin");
        foreach (var candidate in reusable)
            Add(new(candidate.Id, candidate.Version, candidate.Family?.Name ?? candidate.Id,
                () => new GeneratedGameAdapter(store.LoadVersion(candidate.Id, candidate.Version), toolchain(), runner,
                    store.WorkDirectory, toolkit, progress, inventory), true, "index", candidate.Descriptor, candidate.Match));
        // Loose scripts are useful for normal extraction; they must not finish an archive/runtime development task.
        if (analysis)
            foreach (var adapter in VnTextPatchAdapters.CreateAll(discoverySession: inventory)) AddAdapter(adapter, "builtin-format");
        progress?.Invoke("筛选引擎候选", $"已扫描 {inventory.FileCount:N0} 个文件，外置适配器由 {indexed.Count:N0} 个版本筛选为 {reusable.Count:N0} 个候选。");
        await TraceAsync(new { operation = "discovery-selection", stage = "selection", elapsedMs = watch.ElapsedMilliseconds, files = inventory.FileCount,
            indexedVersions = indexed.Count, candidates = reusable.Count, totalCandidates = candidates.Count, complete = inventory.Complete,
            signatureReads = inventory.RangeReads, signatureBytes = inventory.BytesRead });

        foreach (var candidate in candidates)
        {
            ct.ThrowIfCancellationRequested();
            progress?.Invoke("匹配游戏引擎", "正在检查：" + candidate.Name);
            var timer = Stopwatch.StartNew();
            var candidateTrace = new AdapterOperationTrace(trace, "candidate-phase", candidate.Id, candidate.Version);
            IGameAdapter? adapter = null;
            string phase = "load";
            string? source = null;
            var recognized = false;
            var accepted = false;
            var reusedFailure = false;
            var outcome = "not-matched";
            try
            {
                if (trace is not null)
                    await TraceAsync(new { operation = "candidate-start", adapterId = candidate.Id, version = candidate.Version,
                        name = candidate.Name, selection = SelectionDetail(candidate, inventory, ct) });
                adapter = await candidateTrace.MeasureAsync(phase, () => Task.FromResult(candidate.Load()));
                if (adapter is GeneratedGameAdapter loaded)
                    source = Directory.Exists(Path.Combine(loaded.Package.DirectoryPath, "src"))
                        ? Path.Combine(loaded.Package.DirectoryPath, "src") : loaded.Package.DirectoryPath;
                phase = "probe";
                var rangeReads = inventory.RangeReads;
                var bytesRead = inventory.BytesRead;
                var probe = await candidateTrace.MeasureAsync(phase, () => adapter is VnTextPatchAdapter loose
                    ? loose.ProbeAsync(request.GameDirectory, trace is null ? null : (format, resources) => TraceAsync(new
                    {
                        operation = "probe-resources", adapterId = candidate.Id, version = candidate.Version, format = loose.FormatKey,
                        reason = "filename-extension-or-companion", extensions = format.Extensions, companionFileNames = format.CompanionFileNames,
                        resources = ResourceSummary(inventory, resources)
                    }), ct)
                    : adapter.ProbeAsync(request.GameDirectory, ct));
                if (trace is not null)
                    await TraceAsync(new { operation = "probe-result", adapterId = candidate.Id, version = candidate.Version,
                        matched = probe is not null, diagnostic = (adapter as VnTextPatchAdapter)?.LastProbeDiagnostic,
                        resources = probe is null ? null : ResourceSummary(inventory, probe.RequiredFiles),
                        signatureReads = inventory.RangeReads - rangeReads, signatureBytes = inventory.BytesRead - bytesRead });
                if (adapter is VnTextPatchAdapter builtin && (probe is not null || builtin.LastProbeDiagnostic is not null))
                    probes.Add(new(builtin.FormatKey, probe is null ? "failed" : "script-probe-passed",
                        probe?.RequiredFiles.Take(8).ToArray() ?? [], builtin.LastProbeDiagnostic, DateTimeOffset.UtcNow));
                if (probe is null) continue;
                recognized = true;
                var sourceDirectory = request.GameDirectory;
                if (prepareSources is not null)
                {
                    phase = "prepare-sources";
                    sourceDirectory = await candidateTrace.MeasureAsync(phase, () => prepareSources(adapter, probe, ct));
                }
                if (adapter is GeneratedGameAdapter generated && (active?.Id != generated.Id || active.Version != generated.Version))
                {
                    phase = "fingerprint";
                    var fingerprint = await candidateTrace.MeasureAsync(phase, () => FingerprintAsync(generated, sourceDirectory, probe, ct, candidateTrace));
                    var previous = request.PreviousFailures?.FirstOrDefault(item => item.Id == generated.Id
                        && item.Version == generated.Version && fingerprint is not null && item.InputFingerprint == fingerprint);
                    if (previous is not null)
                    {
                        reusedFailure = true;
                        outcome = "previous-failure";
                        if (failures.Count < 6) failures.Add(previous with { SourceDirectory = source });
                        foreach (var item in previous.Evidence) Diagnose(item);
                        continue;
                    }
                    progress?.Invoke("验证资源兼容性", "正在检查原文回填、中文修改和部分试译。");
                    phase = "verify";
                    var verificationDirectory = WorkspaceFiles.Resolve(store.WorkDirectory, "reuse-check");
                    GeneratedGameAdapter.DeleteStagedTree(verificationDirectory);
                    var verification = await candidateTrace.MeasureAsync(phase, () => new GeneratedAdapterVerifier()
                        .VerifyAsync(generated, sourceDirectory, verificationDirectory, ct, trace: trace));
                    await TraceAsync(new { operation = "verification-result", adapterId = candidate.Id, version = candidate.Version,
                        passed = verification.Passed, originalRoundtripPassed = verification.OriginalRoundtripPassed,
                        diagnostic = verification.Passed ? null : verification.Diagnostics.LastOrDefault() });
                    if (!verification.Passed)
                    {
                        foreach (var item in verification.Diagnostics) Diagnose(item);
                        var last = verification.Diagnostics.LastOrDefault();
                        var end = last?.IndexOf(']') ?? -1;
                        var failedStage = last?.StartsWith('[') == true && end is > 1 and < 80 ? last[1..end] : phase;
                        outcome = "verification-failed";
                        if (!verification.Cacheable || fingerprint != await candidateTrace.MeasureAsync("fingerprint-recheck",
                            () => FingerprintAsync(generated, sourceDirectory, probe, ct, candidateTrace))) fingerprint = null;
                        if (failures.Count < 6) failures.Add(new(generated.Id, generated.Version, failedStage,
                            verification.Diagnostics.TakeLast(3).Select(Limit).ToArray(), source, verification.OriginalRoundtripPassed, fingerprint));
                        continue;
                    }
                    phase = "publish";
                    await candidateTrace.MeasureAsync(phase, async () =>
                    {
                        await store.RecordCompatibilityAsync(generated.Package.Family!, new(request.GameId,
                            Path.GetFileName(Path.TrimEndingDirectorySeparator(request.GameDirectory)), probe.Description, generated.Version,
                            "resource-roundtrip-passed"), ct);
                        await store.PublishAndActivateAsync(request.GameId, generated.Package, verification.Diagnostics, ct);
                    });
                }
                else if (prepareSources is null && adapter is not GeneratedGameAdapter)
                {
                    phase = "extract";
                    if ((await candidateTrace.MeasureAsync(phase, () => adapter.ExtractAsync(sourceDirectory, ct))).Units.Count == 0) continue;
                }
                accepted = true;
                outcome = "accepted";
                return Result(adapter, probe);
            }
            catch (Exception error) when (error is not OperationCanceledException
                && (!analysis || adapter is null or GeneratedGameAdapter or VnTextPatchAdapter or SiglusAdapter))
            {
                outcome = "failed";
                Diagnose($"适配器 {candidate.Id} 未匹配：{error.Message}");
                if (candidate.Generated && (recognized || candidate.RecognitionEvidence > 0) && failures.Count < 6)
                    failures.Add(new(candidate.Id, candidate.Version, phase, [Limit(error.Message)], source));
                var format = adapter is VnTextPatchAdapter builtin ? builtin.FormatKey : adapter is SiglusAdapter ? "Siglus" : null;
                if (format is not null)
                {
                    probes.RemoveAll(item => item.Format == format);
                    probes.Add(new(format, "failed", [], Limit(error.Message), DateTimeOffset.UtcNow));
                }
                if (adapter is VnTextPatchAdapter && error is AdapterProcessUnavailableException) break;
            }
            catch (Exception error)
            {
                outcome = error is OperationCanceledException ? "cancelled" : "failed";
                throw;
            }
            finally
            {
                await TraceAsync(new { operation = "reuse-candidate", stage = "candidate", adapterId = candidate.Id, version = candidate.Version,
                    phase, matched = recognized, accepted, reusedFailure, outcome, elapsedMs = timer.ElapsedMilliseconds });
            }
        }
        return Result();
    }

    private async Task<string?> FingerprintAsync(GeneratedGameAdapter adapter, string sourceDirectory, GameProbe probe, CancellationToken ct,
        AdapterOperationTrace timing)
    {
        try
        {
            var inventory = GameDiscoverySession.Scan(sourceDirectory, ct);
            if (!inventory.Complete) return null;
            var package = adapter.Package;
            // Revalidate pinned shared dependencies before reusing an observed failure.
            await toolkit.ResolvePackageAsync(package.DirectoryPath, ct);
            var resources = inventory.Resources(new(package.ResourceFiles, package.ResourcePatterns, package.Recognition), ct);
            var hashes = new Dictionary<string, string>(StringComparer.Ordinal);
            foreach (var file in resources.Concat(probe.RequiredFiles).Distinct(StringComparer.Ordinal).Order(StringComparer.Ordinal))
            {
                var path = WorkspaceFiles.Resolve(sourceDirectory, file);
                hashes[file] = await timing.FileAsync("fingerprint-file-hash", file, path, () => LocalFiles.HashFileAsync(path, ct));
            }
            return LocalFiles.HashText(JsonSerializer.Serialize(new
            {
                host = typeof(AdapterDiscoveryService).Module.ModuleVersionId, GeneratedAdapterVerifier.Revision,
                toolchain = toolchain(), package.Id, package.Version, package.AssemblySha256, package.PackageFiles,
                package.ResourcePatterns, package.Recognition, probe, hashes
            }));
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or InvalidDataException or GeneratedAdapterException)
        { return null; }
    }

    private static string Limit(string text) => text[..Math.Min(text.Length, 1500)];
    private static AdapterDiscoveryDescriptor Descriptor(GeneratedAdapterPackage package) =>
        new(package.ResourceFiles, package.ResourcePatterns, package.Recognition);

    private static object SelectionDetail(Candidate candidate, GameDiscoverySession inventory, CancellationToken ct)
    {
        try
        {
            return new { origin = candidate.Origin, recognitionStatus = candidate.Match?.Status.ToString(),
                evidence = candidate.RecognitionEvidence, reason = candidate.Origin == "index"
                    ? candidate.Match?.Status == RecognitionStatus.Match ? "declared-rules-matched" : "unknown-kept-for-probe"
                    : candidate.Origin == "project-active" ? "project-pinned-version" : "adapter-self-check",
                resourcePatterns = candidate.Descriptor?.ResourcePatterns?.Take(16).ToArray(),
                resourceFiles = candidate.Descriptor?.ResourcePatterns is not { Count: > 0 }
                    ? candidate.Descriptor?.ResourceFiles?.Take(16).ToArray() : null,
                requiredRules = candidate.Descriptor?.Recognition?.Required?.Count ?? 0,
                hintRules = candidate.Descriptor?.Recognition?.Hints?.Count ?? 0,
                resources = candidate.Descriptor is null ? null : ResourceSummary(inventory, inventory.Resources(candidate.Descriptor, ct)) };
        }
        catch (Exception error) when (error is InvalidDataException or ArgumentException or GeneratedAdapterException
            or System.Text.RegularExpressions.RegexMatchTimeoutException)
        { return new { origin = candidate.Origin, diagnostic = Limit(error.Message) }; }
    }

    private static object ResourceSummary(GameDiscoverySession inventory, IEnumerable<string> paths)
    {
        var resources = paths.Distinct(StringComparer.OrdinalIgnoreCase)
            .Select(path => new { path, bytes = inventory.FileLength(path) }).ToArray();
        return new { files = resources.Length, totalBytes = resources.Sum(file => file.bytes ?? 0),
            complete = inventory.Complete && resources.All(file => file.bytes.HasValue),
            largestFiles = resources.Where(file => file.bytes.HasValue).OrderByDescending(file => file.bytes).Take(5).ToArray(),
            missingFiles = resources.Where(file => !file.bytes.HasValue).Take(5).Select(file => file.path).ToArray() };
    }

    private sealed record Candidate(string Id, string Version, string Name, Func<IGameAdapter> Load,
        bool Generated, string Origin, AdapterDiscoveryDescriptor? Descriptor = null, RecognitionResult? Match = null)
    {
        public int RecognitionEvidence => Match?.Evidence ?? 0;
    }
}
