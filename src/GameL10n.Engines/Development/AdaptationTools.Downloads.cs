using System.Collections.Concurrent;
using System.Security.Cryptography;
using System.Text.Json;
using GameL10n.Core.Isolation;
using GameL10n.Core.Storage;

namespace GameL10n.Core.Development;

public sealed partial class AdaptationTools
{
    private static readonly ConcurrentDictionary<string, SemaphoreSlim> DownloadLocks = new(StringComparer.OrdinalIgnoreCase);
    private static readonly TimeSpan ReferenceCacheLifetime = TimeSpan.FromDays(7);
    private sealed record DownloadReceipt(string Url, string Sha256, long Bytes, DateTimeOffset RetrievedAt, string? MediaType);
    private sealed record PublicDownload(string Path, DownloadReceipt Source, bool Reused, bool Stale = false);

    private async Task<object> DownloadAsync(JsonElement args, CancellationToken ct)
    {
        ct.ThrowIfCancellationRequested();
        var relative = Text(args, "path").Replace('\\', '/');
        if (!relative.StartsWith("src/tools/", StringComparison.Ordinal) && !relative.StartsWith("work/", StringComparison.Ordinal))
            throw new InvalidDataException("下载目标必须在 src/tools/ 或 work/。");
        var path = WritePath(relative);
        var release = DeclaredReleaseFile(args, relative);
        var url = Text(args, "url");
        var uri = new Uri(url);
        ValidatePublicUri(uri);
        var refresh = Boolean(args, "refresh", false);
        reportProgress?.Invoke("准备额外工具", $"{Path.GetFileName(relative)} · 来源 {uri.Host} · "
            + Limit(Text(args, "purpose", "分析资料，尚未登记为发布依赖"), 240) + $" · 保存到 {relative}");
        var manifest = WorkspaceFiles.Resolve(SourceDirectory, "tools/sources.json");
        var sources = relative.StartsWith("src/tools/", StringComparison.Ordinal)
            ? await LocalFiles.ReadJsonAsync<Dictionary<string, ToolSource>>(manifest, ct) ?? [] : null;
        ToolSource? pinned = null;
        sources?.TryGetValue(relative["src/tools/".Length..], out pinned);
        if (pinned?.Url != url) pinned = null;
        string sha256;
        bool reused;
        if (!refresh && pinned is not null && File.Exists(path))
        {
            sha256 = await LocalFiles.HashFileAsync(path, ct);
            if (sha256 != pinned.Sha256)
                throw new InvalidDataException("目标工具已有本地修改，未覆盖：" + relative + "。请选择新路径，或用 refresh=true 明确重新下载。");
            reused = true;
        }
        else
        {
            var download = await ReadPublicDownloadAsync(url, 256 * 1024 * 1024, refresh, null, ct,
                refresh ? null : pinned?.Sha256);
            var temporary = path + "." + Guid.NewGuid().ToString("N") + ".partial";
            Directory.CreateDirectory(Path.GetDirectoryName(path)!);
            try
            {
                await GeneratedAdapterResources.CopyAsync(download.Path, temporary, ct);
                ct.ThrowIfCancellationRequested();
                InvalidateSource(path);
                File.Move(temporary, path, true);
            }
            finally { if (File.Exists(temporary)) File.Delete(temporary); }
            sha256 = download.Source.Sha256;
            reused = download.Reused;
        }
        if (sources is not null)
        {
            sources[relative["src/tools/".Length..]] = new(url, sha256);
            await LocalFiles.WriteJsonAsync(manifest, sources, ct);
        }
        if (release is not null) RegisterReleaseFiles([release]);
        var bytes = new FileInfo(path).Length;
        reportProgress?.Invoke(reused ? "复用已有工具" : "额外工具已保存", $"{relative} · {bytes / 1048576.0:F2} MiB");
        return new { path = relative, sha256, bytes, reused,
            releaseRegistered = session.ReleaseFiles?.Any(file => file.Path == AdapterReleaseFiles.PackagePath(relative)) == true };
    }

    private async Task<PublicDownload> ReadPublicDownloadAsync(string url, int maximumBytes, bool refresh,
        TimeSpan? maximumAge, CancellationToken ct, string? expectedHash = null)
    {
        var uri = new Uri(url);
        ValidatePublicUri(uri);
        var canonicalUrl = uri.GetComponents(UriComponents.HttpRequestUrl, UriFormat.UriEscaped);
        if (expectedHash is not null && !IsDownloadHash(expectedHash))
            throw new InvalidDataException("已登记工具的 SHA256 无效，不能据此恢复固定版本。");
        var relative = "downloads/" + LocalFiles.HashText(canonicalUrl);
        var receiptPath = WorkspaceFiles.Resolve(store.RootDirectory, relative + "/source.json");
        var gate = DownloadLocks.GetOrAdd(receiptPath, _ => new(1, 1));
        await gate.WaitAsync(ct);
        try
        {
            var cached = refresh ? null : await ReadCachedDownloadAsync(relative, canonicalUrl, maximumBytes, ct, expectedHash);
            if (!refresh && cached is not null && (maximumAge is null
                || DateTimeOffset.UtcNow - cached.Source.RetrievedAt <= maximumAge)) return cached;
            try
            {
                using var response = await GetAsync(uri, ct);
                if (response.Content.Headers.ContentLength > maximumBytes) throw DownloadTooLarge(maximumBytes);
                reportProgress?.Invoke("下载公开工具或资料", $"{Path.GetFileName(uri.AbsolutePath)} · 来源 {uri.Host} · 正在保存共享缓存。");
                Directory.CreateDirectory(Path.GetDirectoryName(receiptPath)!);
                var temporary = WorkspaceFiles.Resolve(store.RootDirectory, relative + "/" + Guid.NewGuid().ToString("N") + ".partial");
                try
                {
                    using var hash = IncrementalHash.CreateHash(HashAlgorithmName.SHA256);
                    long bytes = 0;
                    await using (var output = File.Create(temporary))
                    await using (var input = await response.Content.ReadAsStreamAsync(ct))
                    {
                        var buffer = new byte[128 * 1024];
                        int count;
                        while ((count = await input.ReadAsync(buffer, ct)) != 0)
                        {
                            bytes += count;
                            if (bytes > maximumBytes) throw DownloadTooLarge(maximumBytes);
                            hash.AppendData(buffer, 0, count);
                            await output.WriteAsync(buffer.AsMemory(0, count), ct);
                        }
                    }
                    var receipt = new DownloadReceipt(canonicalUrl, Convert.ToHexStringLower(hash.GetHashAndReset()), bytes,
                        DateTimeOffset.UtcNow, response.Content.Headers.ContentType?.MediaType);
                    if (expectedHash is not null && receipt.Sha256 != expectedHash)
                        throw new InvalidDataException("上游文件与此项目已登记的工具版本不同，未替换固定依赖。请恢复原文件；确需更新时用 refresh=true。");
                    var destination = WorkspaceFiles.Resolve(store.RootDirectory, relative + "/" + receipt.Sha256 + ".bin");
                    ct.ThrowIfCancellationRequested();
                    // Publish the URL receipt last; cancellation leaves the previous version intact.
                    File.Move(temporary, destination, true);
                    await LocalFiles.WriteJsonAsync(WorkspaceFiles.Resolve(store.RootDirectory, relative + "/" + receipt.Sha256 + ".json"), receipt, ct);
                    await LocalFiles.WriteJsonAsync(receiptPath, receipt, ct);
                    return new(destination, receipt, false);
                }
                finally { if (File.Exists(temporary)) File.Delete(temporary); }
            }
            catch (Exception error) when (!refresh && cached is not null && (error is HttpRequestException
                || error is OperationCanceledException && !ct.IsCancellationRequested))
            {
                return cached with { Stale = true };
            }
        }
        finally { gate.Release(); }
    }

    private async Task<PublicDownload?> ReadCachedDownloadAsync(string relative, string url, int maximumBytes, CancellationToken ct,
        string? expectedHash)
    {
        DownloadReceipt? receipt;
        try
        {
            var contents = WorkspaceFiles.ReadBounded(store.RootDirectory, relative + "/" + (expectedHash ?? "source") + ".json", 32 * 1024);
            receipt = JsonSerializer.Deserialize<DownloadReceipt>(contents, Json);
        }
        catch (Exception error) when (error is IOException or JsonException or InvalidDataException) { return null; }
        if (receipt is null || receipt.Url != url || !IsDownloadHash(receipt.Sha256)
            || expectedHash is not null && receipt.Sha256 != expectedHash
            || receipt.Bytes < 0 || receipt.RetrievedAt <= DateTimeOffset.UnixEpoch
            || receipt.RetrievedAt > DateTimeOffset.UtcNow.AddMinutes(5)) return null;
        if (receipt.Bytes > maximumBytes) throw DownloadTooLarge(maximumBytes);
        var path = WorkspaceFiles.Resolve(store.RootDirectory, relative + "/" + receipt.Sha256 + ".bin");
        if (!File.Exists(path) || new FileInfo(path).Length != receipt.Bytes) return null;
        if (await LocalFiles.HashFileAsync(path, ct) != receipt.Sha256) return null;
        return new(path, receipt, true);
    }

    private static InvalidDataException DownloadTooLarge(int maximumBytes) =>
        new($"公开文件超过 {maximumBytes / 1024} KiB 读取上限，请选择必要的工具文件或具体资料；未将截断内容当作完整资料。");

    private static bool IsDownloadHash(string? value) => value is { Length: 64 } && value.All(Uri.IsHexDigit);

    private static void ValidatePublicUri(Uri uri)
    {
        if (!uri.IsAbsoluteUri || uri.Scheme != "https" || !string.IsNullOrEmpty(uri.UserInfo) || uri.IsLoopback
            || uri.HostNameType != UriHostNameType.Dns || !uri.Host.Contains('.')
            || uri.Host.EndsWith(".local", StringComparison.OrdinalIgnoreCase))
            throw new InvalidDataException("资料工具只允许无凭据的公共 HTTPS 地址。");
    }
}
