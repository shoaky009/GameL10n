using AvgTranslator.Core.Isolation;

namespace AvgTranslator.Core.Development;

internal sealed record ProbeRead(string Path, long Offset, int Length);
internal sealed record ProbeRange(string Path, long Offset, int Length, string DataPath);

/// <summary>Supplies only explicitly requested candidate ranges; media payloads remain at the source.</summary>
internal sealed class GeneratedProbeReads(string sourceDirectory, IReadOnlyList<string> candidates, string workspace)
{
    internal static readonly object Limits = new { maximumRounds = 4, maximumRanges = 64,
        maximumRangeBytes = 1024 * 1024, maximumTotalBytes = 8 * 1024 * 1024 };
    private readonly HashSet<string> allowed = new(candidates.Select(GeneratedAdapterResources.Normalize), StringComparer.OrdinalIgnoreCase);
    private readonly Dictionary<string, (long Length, DateTime LastWrite)> versions = candidates.ToDictionary(
        GeneratedAdapterResources.Normalize, path => Version(WorkspaceFiles.Resolve(sourceDirectory, path)), StringComparer.OrdinalIgnoreCase);
    private readonly Dictionary<string, string> hashes = new(StringComparer.OrdinalIgnoreCase);
    private int rounds;
    private long totalBytes;
    public List<ProbeRange> Ranges { get; } = [];

    public async Task SupplyAsync(IReadOnlyList<ProbeRead> reads, CancellationToken ct)
    {
        if (++rounds > 4 || reads.Count == 0 || reads.Count + Ranges.Count > 64)
            throw new GeneratedAdapterException("探测范围读取超过上限：最多 4 轮、64 个片段，请缩小索引读取范围。");
        var keys = Ranges.Select(range => (range.Path.ToUpperInvariant(), range.Offset, range.Length)).ToHashSet();
        long bytes = totalBytes;
        // Validate the whole request before copying any data.
        foreach (var read in reads)
        {
            var path = GeneratedAdapterResources.Normalize(read.Path);
            if (!allowed.Contains(path)) throw new GeneratedAdapterException("探测只能读取本次候选文件：" + path);
            if (read.Length is < 1 or > 1024 * 1024 || read.Offset < 0 || read.Offset > versions[path].Length - read.Length)
                throw new GeneratedAdapterException("探测片段越界或超过 1 MiB：" + path);
            if (!keys.Add((path.ToUpperInvariant(), read.Offset, read.Length)))
                throw new GeneratedAdapterException("重复请求已提供的探测片段，请使用 probeRanges：" + path);
            bytes += read.Length;
        }
        if (bytes > 8 * 1024 * 1024) throw new GeneratedAdapterException("探测片段总量超过 8 MiB，请避免读取完整媒体资源。");

        var directory = WorkspaceFiles.Resolve(workspace, "probe-ranges");
        Directory.CreateDirectory(directory);
        foreach (var read in reads)
        {
            ct.ThrowIfCancellationRequested();
            var path = GeneratedAdapterResources.Normalize(read.Path);
            var original = WorkspaceFiles.Resolve(sourceDirectory, path);
            WorkspaceFiles.RejectLinkAncestors(original);
            if (Version(original) != versions[path]) throw new GeneratedAdapterException("探测期间原始文件已变化，请重试：" + path);
            var destination = WorkspaceFiles.Resolve(directory, Ranges.Count + ".bin");
            var buffer = new byte[read.Length];
            await using (var input = new FileStream(original, FileMode.Open, FileAccess.Read, FileShare.Read,
                4096, FileOptions.Asynchronous | FileOptions.RandomAccess))
            {
                input.Position = read.Offset;
                await input.ReadExactlyAsync(buffer, ct);
            }
            if (Version(original) != versions[path]) throw new GeneratedAdapterException("探测期间原始文件已变化，请重试：" + path);
            await using (var output = new FileStream(destination, FileMode.CreateNew, FileAccess.Write, FileShare.None,
                4096, FileOptions.Asynchronous))
                await output.WriteAsync(buffer, ct);
            hashes[destination] = GeneratedAdapterResources.HashFile(destination);
            Ranges.Add(new(path, read.Offset, read.Length, destination));
        }
        totalBytes = bytes;
    }

    public void VerifyUnchanged()
    {
        foreach (var pair in hashes)
        {
            WorkspaceFiles.RejectLinkAncestors(pair.Key);
            if (GeneratedAdapterResources.HashFile(pair.Key) != pair.Value)
                throw new GeneratedAdapterException("适配器修改了只读探测片段。");
        }
    }

    private static (long Length, DateTime LastWrite) Version(string path)
    {
        var file = new FileInfo(path);
        return (file.Length, file.LastWriteTimeUtc);
    }
}
