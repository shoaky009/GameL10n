using System.Security.Cryptography;
using System.Text.Json;
using AvgTranslator.Core.Adapters;
using AvgTranslator.Core.Isolation;
using AvgTranslator.Core.Storage;

namespace AvgTranslator.Core.Development;

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
        var trial = builtinTrial ?? new BuiltinAdapterTrial(compiler.Runner);
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
                        var group = SampleGroup(format, path);
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
                var hashes = await SampleHashesAsync(request.GameDirectory, lengths, token);
                var fingerprint = LocalFiles.HashText(JsonSerializer.Serialize(new
                {
                    revision = AutomaticTrialRevision, trialRevision = BuiltinAdapterTrial.Revision, format = format.Key,
                    parserFingerprint, files = hashes.OrderBy(pair => pair.Key, StringComparer.Ordinal)
                }, Json));
                var cached = session.BuiltinTrials.FirstOrDefault(item => item.Format == format.Key && item.Fingerprint == fingerprint);
                if (cached is null) session.BuiltinTrials.RemoveAll(item => item.Format == format.Key);
                await StageAutomaticSamplesAsync(lengths, hashes, token);
                if (parserFingerprint != await trial.FingerprintAsync(token)) throw new IOException("自动试用期间解析工具发生变化，结果未复用。");
                if (cached is not null) { reused++; continue; }
                var result = await trial.RunAsync(format.Key, workspace, "resources", files, checkRoundtrip: true, token);
                var after = await SampleHashesAsync(request.GameDirectory, lengths, token);
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

    private IReadOnlyList<string> SampleGroup(VnTextPatchFormat format, string path)
    {
        var script = path.Replace('\\', '/');
        var source = WorkspaceFiles.Resolve(request.GameDirectory, script);
        if (!File.Exists(source)) throw new FileNotFoundException("候选样本不存在：" + script, script);
        var required = format.Key == "Softpal";
        if (format.Key == "Yuris")
        {
            using var stream = File.OpenRead(source);
            Span<byte> header = stackalloc byte[4];
            required = stream.ReadAtLeast(header, 4, throwOnEndOfStream: false) == 4 && header.SequenceEqual("YSTB"u8);
            if (!required) return [script];
        }
        var group = new List<string> { script };
        foreach (var name in format.CompanionFileNames)
        {
            var companion = Path.Combine(Path.GetDirectoryName(script) ?? "", name).Replace('\\', '/');
            if (File.Exists(WorkspaceFiles.Resolve(request.GameDirectory, companion))) group.Add(companion);
            else if (required) throw new FileNotFoundException("候选脚本缺少必需伴随文件：" + companion, companion);
        }
        return group;
    }

    private static async Task<Dictionary<string, string>> SampleHashesAsync(string root, IReadOnlyDictionary<string, long> lengths, CancellationToken ct)
    {
        var result = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);
        foreach (var (file, expectedLength) in lengths)
        {
            await using var input = new FileStream(WorkspaceFiles.Resolve(root, file), FileMode.Open, FileAccess.Read, FileShare.Read,
                128 * 1024, FileOptions.Asynchronous | FileOptions.SequentialScan);
            if (input.Length != expectedLength) throw new IOException("自动试用样本大小发生变化：" + file);
            using var hash = IncrementalHash.CreateHash(HashAlgorithmName.SHA256);
            var buffer = new byte[128 * 1024];
            long read = 0;
            int count;
            while ((count = await input.ReadAsync(buffer, ct)) != 0)
            {
                if (count > expectedLength - read) throw new IOException("自动试用样本超出预算：" + file);
                read += count;
                hash.AppendData(buffer, 0, count);
            }
            if (read != expectedLength) throw new IOException("自动试用样本被截断：" + file);
            result[file] = Convert.ToHexStringLower(hash.GetHashAndReset());
        }
        return result;
    }

    private async Task StageAutomaticSamplesAsync(IReadOnlyDictionary<string, long> lengths, IReadOnlyDictionary<string, string> hashes, CancellationToken ct)
    {
        var pending = WorkspaceFiles.Resolve(workspace, "work/.auto-stage-" + Guid.NewGuid().ToString("N"));
        try
        {
            foreach (var (file, expectedLength) in lengths)
            {
                var staged = WorkspaceFiles.Resolve(ResourceDirectory, file);
                if (File.Exists(staged) && new FileInfo(staged).Length == expectedLength
                    && await LocalFiles.HashFileAsync(staged, ct) == hashes[file]) continue;
                var destination = WorkspaceFiles.Resolve(pending, file);
                Directory.CreateDirectory(Path.GetDirectoryName(destination)!);
                await using (var input = new FileStream(WorkspaceFiles.Resolve(request.GameDirectory, file), FileMode.Open, FileAccess.Read, FileShare.Read,
                    128 * 1024, FileOptions.Asynchronous | FileOptions.SequentialScan))
                await using (var output = new FileStream(destination, FileMode.CreateNew, FileAccess.Write, FileShare.None,
                    128 * 1024, FileOptions.Asynchronous))
                {
                    var buffer = new byte[128 * 1024];
                    long total = 0;
                    int count;
                    while ((count = await input.ReadAsync(buffer, ct)) != 0)
                    {
                        if (count > expectedLength - total) throw new IOException("自动试用复制超出样本预算：" + file);
                        await output.WriteAsync(buffer.AsMemory(0, count), ct);
                        total += count;
                    }
                }
                if (new FileInfo(destination).Length != expectedLength || await LocalFiles.HashFileAsync(destination, ct) != hashes[file])
                    throw new IOException("自动试用复制的样本与原始资源不一致：" + file);
            }
            var actual = await SampleHashesAsync(request.GameDirectory, lengths, ct);
            if (hashes.Any(pair => actual[pair.Key] != pair.Value)) throw new IOException("自动试用暂存期间原始资源发生变化。");
            foreach (var (file, hash) in hashes)
            {
                var destination = WorkspaceFiles.Resolve(ResourceDirectory, file);
                var copy = WorkspaceFiles.Resolve(pending, file);
                if (File.Exists(copy)) { Directory.CreateDirectory(Path.GetDirectoryName(destination)!); File.Move(copy, destination, true); }
                session.ResourceHashes[file] = hash;
            }
        }
        finally { GeneratedGameAdapter.DeleteStagedTree(pending); }
    }

    private static bool DeterministicParseFailure(VnTextPatchDiagnostic? diagnostic)
    {
        while (diagnostic?.Inner is { } inner) diagnostic = inner;
        return diagnostic?.ErrorType is "System.IO.InvalidDataException" or "System.IO.EndOfStreamException"
            or "System.Text.DecoderFallbackException" or "System.Text.EncoderFallbackException"
            or "System.ArgumentOutOfRangeException" or "System.IndexOutOfRangeException" or "System.OverflowException";
    }
}
