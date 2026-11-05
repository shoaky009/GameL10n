using System.Text.Json;
using GameL10n.Core.Adapters;
using GameL10n.Core.Isolation;
using GameL10n.Core.Storage;
using System.Text.Json.Nodes;
using GameL10n.Core.Serialization;

namespace GameL10n.Core.Development;

public sealed record AutomaticBuiltinTrialReport(int Revision, string Status, int Reused,
    IReadOnlyList<string> Skipped, string? Error = null);

public sealed partial class AdaptationTools
{
    private const int AutomaticTrialRevision = 1;
    private const long AutomaticTrialBytes = 64L * 1024 * 1024;

    /// <summary>Bounded host work before the first model turn; never replaces an existing implementation.</summary>
    internal async Task RunAutomaticBuiltinTrialsAsync(IReadOnlySet<string> disabledFamilies, Func<Task> save,
        CancellationToken ct)
    {
        if (session.Objective != AdaptationObjective.Develop || request.ImproveExisting || request.BaselineUnits is { Count: > 0 }
            || session.TotalTurns != 0 || session.PendingCalls.Count > 0 || session.VerifiedPackage is not null
            || session.ReleaseFiles is { Count: > 0 } || session.ReuseFailures.Count > 0
            || Directory.Exists(SourceDirectory) && Directory.EnumerateFileSystemEntries(SourceDirectory).Any()) return;
        var candidates = session.BuiltinDiscovery?.Candidates ?? [];
        if (candidates.Count == 0) return;
        using var budget = CancellationTokenSource.CreateLinkedTokenSource(ct);
        budget.CancelAfter(TimeSpan.FromSeconds(60));
        var skipped = new List<string>();
        var reused = 0;
        var attempted = 0;
        long selectedBytes = 0;
        var trial = builtinTrial ?? new BuiltinAdapterTrial(runtime.Runner);
        string? parserFingerprint = null;
        session.AutomaticTrials = new(AutomaticTrialRevision, "running", 0, []);
        await save();
        try
        {
            foreach (var candidate in candidates.OrderBy(candidate => candidate.EvidenceKind == "script-signature" ? 0 : 1))
            {
                if (attempted >= 3) break;
                budget.Token.ThrowIfCancellationRequested();
                var format = VnTextPatchFormatCatalog.All.FirstOrDefault(format => format.Key == candidate.Format);
                if (format is null || disabledFamilies.Contains(format.Family.AdapterId)) continue;
                var files = new List<string>();
                var lengths = new Dictionary<string, long>(StringComparer.OrdinalIgnoreCase);
                var scripts = 0;
                foreach (var path in candidate.SampleFiles.Take(4))
                {
                    if (scripts >= 2) break;
                    if (!format.Extensions.Contains(Path.GetExtension(path), StringComparer.OrdinalIgnoreCase)
                        || format.CompanionFileNames.Contains(Path.GetFileName(path), StringComparer.OrdinalIgnoreCase)) continue;
                    try
                    {
                        var group = BuiltinAdapterTrial.SampleGroup(format, request.GameDirectory, path);
                        var extra = group.Where(file => !lengths.ContainsKey(file)).Distinct(StringComparer.OrdinalIgnoreCase)
                            .ToDictionary(file => file, file => new FileInfo(WorkspaceFiles.Resolve(request.GameDirectory, file)).Length,
                                StringComparer.OrdinalIgnoreCase);
                        if (files.Count + extra.Count > 12 || extra.Values.Any(length => length < 0 || length > AutomaticTrialBytes)
                            || extra.Values.Sum() > AutomaticTrialBytes - selectedBytes)
                        { Skip(candidate.Format + "：" + Limit(path, 80) + " 及伴随文件超过自动试用的 64 MiB／12 文件预算。"); continue; }
                        foreach (var (file, length) in extra) { files.Add(file); lengths.Add(file, length); selectedBytes += length; }
                        scripts++;
                    }
                    catch (Exception error) when (error is IOException or UnauthorizedAccessException or InvalidDataException)
                    { Skip(candidate.Format + "：" + Limit(error.Message, 240)); }
                }
                if (scripts == 0)
                { Skip(candidate.Format + "：没有可直接试用的裸脚本样本；封包、编译脚本和私有过滤器需要另行确认。"); continue; }
                if (parserFingerprint is null)
                {
                    try { parserFingerprint = await trial.FingerprintAsync(budget.Token); }
                    catch (Exception error) when (error is IOException or UnauthorizedAccessException)
                    {
                        session.AutomaticTrials = new(AutomaticTrialRevision, "unavailable", reused, skipped,
                            "本地内置解析工具不可用，未执行自动试用；这是工具资产问题，不是格式不兼容：" + Limit(error.Message, 300));
                        await save();
                        return;
                    }
                }
                attempted++;
                reportProgress?.Invoke("自动试用内置解析器", $"正在用 {files.Count} 个工作副本检查 {format.Key} 的提取与原文回填。");
                using var candidateBudget = CancellationTokenSource.CreateLinkedTokenSource(budget.Token);
                candidateBudget.CancelAfter(TimeSpan.FromSeconds(20));
                var token = candidateBudget.Token;
                var hashes = await AdapterDevelopmentWorkflow.SampleHashesAsync(request.GameDirectory, lengths, token);
                var fingerprint = LocalFiles.HashText(JsonSerializer.Serialize(new JsonObject
                {
                    ["revision"] = AutomaticTrialRevision,
                    ["trialRevision"] = BuiltinAdapterTrial.Revision,
                    ["format"] = format.Key,
                    ["parserFingerprint"] = parserFingerprint,
                    ["files"] = EngineJson.Node((hashes.OrderBy(pair => pair.Key, StringComparer.Ordinal)).ToArray(), Json)
                }, EngineJson.TypeInfo<JsonObject>(Json)));
                var cached = session.BuiltinTrials.FirstOrDefault(item => item.Format == format.Key && item.Fingerprint == fingerprint);
                if (cached is null) session.BuiltinTrials.RemoveAll(item => item.Format == format.Key);
                await Development.StageAutomaticSamplesAsync(lengths, hashes, token);
                if (parserFingerprint != await trial.FingerprintAsync(token)) throw new IOException("自动试用期间解析工具发生变化，结果未复用。");
                if (cached is not null) { reused++; continue; }
                var result = await trial.RunAsync(format.Key, workspace, "resources", files, checkRoundtrip: true, token);
                var after = await AdapterDevelopmentWorkflow.SampleHashesAsync(request.GameDirectory, lengths, token);
                if (hashes.Any(pair => !after.TryGetValue(pair.Key, out var value) || value != pair.Value)
                    || result.SourceHashes.Any(pair => !hashes.TryGetValue(pair.Key, out var value) || value != pair.Value))
                    throw new IOException("自动试用期间原始资源或样本发生变化，结果未缓存，请重新确认资源。");
                if (parserFingerprint != await trial.FingerprintAsync(token)) throw new IOException("自动试用期间解析工具发生变化，结果未缓存。");
                // Do not reuse transient process/I/O failures. Keep their bounded diagnostics for the model.
                var cacheable = result.Status is "passed" or "no-text" || DeterministicParseFailure(result.Diagnostic);
                RecordBuiltinTrial(result, "resources", files, cacheable ? fingerprint : null,
                    result.Samples.Take(2).Select(sample => sample with { Text = Limit(sample.Text, 120) }).ToArray());
                await save();
            }
            session.AutomaticTrials = new(AutomaticTrialRevision, "completed", reused, skipped);
        }
        catch (OperationCanceledException) when (!ct.IsCancellationRequested)
        { session.AutomaticTrials = new(AutomaticTrialRevision, "timed-out", reused, skipped, "自动试用达到时间预算，未完成的结果未缓存；可继续定向分析，不代表格式不受支持。"); }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or InvalidDataException or AdapterProcessUnavailableException)
        { session.AutomaticTrials = new(AutomaticTrialRevision, "incomplete", reused, skipped, Limit(error.Message, 500)); }
        await save();

        void Skip(string reason) { if (skipped.Count < 6 && !skipped.Contains(reason)) skipped.Add(reason); }
    }

    private static bool DeterministicParseFailure(VnTextPatchDiagnostic? diagnostic)
    {
        while (diagnostic?.Inner is { } inner) diagnostic = inner;
        return diagnostic?.ErrorType is "System.IO.InvalidDataException" or "System.IO.EndOfStreamException"
            or "System.Text.DecoderFallbackException" or "System.Text.EncoderFallbackException"
            or "System.ArgumentOutOfRangeException" or "System.IndexOutOfRangeException" or "System.OverflowException";
    }
}
