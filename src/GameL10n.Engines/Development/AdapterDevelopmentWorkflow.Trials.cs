using GameL10n.Core.Isolation;
using GameL10n.Core.Storage;
using GameL10n.Core.Adapters;

namespace GameL10n.Core.Development;

public sealed partial class AdapterDevelopmentWorkflow
{
    private IReadOnlyList<string> ValidateTrialFiles(IReadOnlyList<string> files)
    {
        var normalized = files.Select(GeneratedAdapterResources.Normalize).Order(StringComparer.Ordinal).ToArray();
        if (normalized.Length is < 1 or > 12 || normalized.Distinct(StringComparer.OrdinalIgnoreCase).Count() != normalized.Length)
            throw new InvalidDataException("trialFiles 需要 1～12 个不重复的完整资源路径，并包含所需伴随文件。");
        foreach (var file in normalized)
        {
            _ = WorkspaceFiles.Resolve(ResourceDirectory, file);
            if (!session.ResourceHashes.ContainsKey(file))
                throw new InvalidDataException("试验资源尚未 stage_resources：" + file);
        }
        return normalized;
    }

    private async Task<(GeneratedAdapterVerification Result, long Bytes)> VerifyTrialAsync(GeneratedGameAdapter adapter,
        IReadOnlyList<string> files, CancellationToken ct)
    {
        var temporary = WorkspaceFiles.Resolve(workspace, "work/.resource-trial-" + Guid.NewGuid().ToString("N"));
        var source = Path.Combine(temporary, "resources");
        var verification = Path.Combine(temporary, "verification");
        long bytes = 0;
        try
        {
            foreach (var file in files)
            {
                var staged = WorkspaceFiles.Resolve(ResourceDirectory, file);
                var copy = WorkspaceFiles.Resolve(source, file);
                await GeneratedAdapterResources.CopyAsync(staged, copy, ct);
                if (await LocalFiles.HashFileAsync(copy, ct) != session.ResourceHashes[file])
                    throw new IOException("试验副本与已暂存资源不一致：" + file);
                bytes += new FileInfo(copy).Length;
            }
            var (coverage, baseline) = VerificationEvidence(files);
            reportProgress?.Invoke("小范围资源试验", $"正在检查 {files.Count} 个完整资源；试验通过后仍需完整资源验收才能发布。");
            var result = await new GeneratedAdapterVerifier().VerifyAsync(adapter, source, verification, ct,
                sourceIsSnapshot: true, coverage: coverage, baselineUnits: baseline, trace: trace, scope: "trial");
            foreach (var file in files)
                if (await LocalFiles.HashFileAsync(WorkspaceFiles.Resolve(source, file), ct) != session.ResourceHashes[file])
                    throw new IOException("试验修改了资源副本：" + file);
            return (result, bytes);
        }
        finally { GeneratedGameAdapter.DeleteStagedTree(temporary); }
    }

    private (TextCoverage Coverage, IReadOnlyList<TranslationUnit>? Baseline) VerificationEvidence(IReadOnlyList<string>? files)
    {
        var baseline = session.Objective == AdaptationObjective.Repair ? request.BaselineUnits : null;
        if (files is null) return (session.TextCoverage, baseline);
        var selected = files.ToHashSet(StringComparer.OrdinalIgnoreCase);
        bool Included(string path) => selected.Contains(GeneratedAdapterResources.Normalize(path));
        return (session.TextCoverage with
        { Samples = session.TextCoverage.Samples.Where(sample => Included(sample.ResourcePath)).ToArray() },
            baseline?.Where(unit => Included(unit.ResourcePath)).ToArray());
    }
}
