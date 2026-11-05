using System.Security.Cryptography;
using System.Text.Json;
using GameL10n.Core.Isolation;
using GameL10n.Core.Storage;
using GameL10n.Core.Serialization;

namespace GameL10n.Core.Development;

public sealed partial class AdapterDevelopmentWorkflow
{
    private string? toolkitFailure;
    internal async Task<EngineReferencePin> ReferenceAsync(bool retry, CancellationToken ct)
    {
        if (toolkitFailure is not null && !retry)
            throw new InvalidDataException("共享引擎工具准备失败，当前会话不自动重复准备：" + toolkitFailure
                + "。可继续读取内置解析器资料或在线参考；恢复完整发行包的工具文件后 inspect_engine_reference(retryDownload=true)。");
        try
        {
            var dependency = await SourceDependencyAsync(ct);
            var selected = dependency?.Reference.Kind == "galtransl" ? dependency.Reference
                : session.EngineReference?.Kind == "galtransl" ? session.EngineReference : null;
            var pin = await EngineToolkit.EnsureReferenceAsync(selected, ct);
            session.EngineReference = pin;
            toolkitFailure = null;
            return pin;
        }
        catch (Exception error) when (error is HttpRequestException or IOException
            || error is OperationCanceledException && !ct.IsCancellationRequested)
        { toolkitFailure = Limit(error.Message, 500); throw; }
    }

    internal async Task<(EnginePythonDependency Dependency, EnginePythonPaths Paths)> PythonAsync(CancellationToken ct)
    {
        var dependency = await SourceDependencyAsync(ct);
        var pin = dependency?.Reference.Kind == "siglus"
            ? await EngineToolkit.EnsureReferenceAsync(dependency.Reference, ct) : await ReferenceAsync(false, ct);
        try { return await EngineToolkit.EnsurePythonAsync(pin, dependency, ct); }
        catch (Exception error) when (error is HttpRequestException or IOException
            || error is OperationCanceledException && !ct.IsCancellationRequested)
        { toolkitFailure = Limit(error.Message, 500); throw; }
    }

    private Task<EnginePythonDependency?> SourceDependencyAsync(CancellationToken ct) =>
        GameL10n.Core.Storage.LocalFiles.ReadJsonAsync<EnginePythonDependency>(
            WorkspaceFiles.Resolve(SourceDirectory, EngineReferenceToolkit.DependencyPath), EngineJson.TypeInfo<EnginePythonDependency>(EngineJson.Storage), ct);

    private async Task<EnginePythonPaths> PreparePythonReleaseAsync(CancellationToken ct)
    {
        var prepared = await PythonAsync(ct);
        var path = WorkspaceFiles.Resolve(SourceDirectory, EngineReferenceToolkit.DependencyPath);
        if (!File.Exists(path))
        {
            Directory.CreateDirectory(Path.GetDirectoryName(path)!);
            await LocalFiles.WriteJsonAsync(path, prepared.Dependency, EngineJson.TypeInfo<EnginePythonDependency>(EngineJson.Storage), ct);
        }
        RegisterReleaseFiles([new(EngineReferenceToolkit.DependencyPath, "runtime", "固定版本 Python 解释器及引擎库")]);
        return prepared.Paths;
    }

    internal void RegisterReleaseFiles(IEnumerable<AdapterReleaseFile> files)
    {
        var additions = files.ToArray();
        if (additions.Length == 0) return;
        AdapterReleaseFiles.Validate(additions, requireSource: false);
        var next = (session.ReleaseFiles ?? session.VerifiedPackage?.ReleaseFiles ?? [])
            .ToDictionary(file => file.Path, StringComparer.OrdinalIgnoreCase);
        foreach (var file in additions) next[file.Path] = file;
        AddDownloadProvenance(next);
        SetReleaseFiles(next.Values.ToArray());
    }

    internal void SetReleaseFiles(IReadOnlyList<AdapterReleaseFile> files)
    {
        var next = files.Count == 0 ? [] : AdapterReleaseFiles.Validate(files, requireSource: false);
        var current = session.ReleaseFiles ?? session.VerifiedPackage?.ReleaseFiles ?? [];
        if (!current.OrderBy(file => file.Path, StringComparer.Ordinal).SequenceEqual(next))
            session.VerifiedPackage = null;
        session.ReleaseFiles = next;
    }

    internal void AddDownloadProvenance(Dictionary<string, AdapterReleaseFile> files)
    {
        if (files.Keys.Any(path => path.StartsWith("tools/", StringComparison.Ordinal))
            && File.Exists(Path.Combine(SourceDirectory, "tools", "sources.json")))
            files.TryAdd("tools/sources.json", new("tools/sources.json", "license", "下载依赖的来源与校验记录"));
    }

    private void CheckReleaseGameInputs(GeneratedAdapterPackage package)
    {
        // Known staged originals can be identified even after renaming. This is not a
        // heuristic for executables: custom launchers remain valid release assets.
        var originals = session.ResourceHashes.Values.ToHashSet(StringComparer.OrdinalIgnoreCase);
        var copies = package.ReleaseFiles!.Where(file => originals.Contains(package.PackageFiles![file.Path]))
            .Select(file => file.Path).ToArray();
        if (copies.Length > 0)
            throw new InvalidDataException("正式引擎包包含当前游戏的原文件副本：" + string.Join("、", copies.Take(8))
                + "。从发布清单移除，改由适配器读取当前游戏资源；修改后的 EXE/资源写入补丁输出，不作为引擎依赖携带。");
    }
    internal static async Task<Dictionary<string, string>> SampleHashesAsync(string root, IReadOnlyDictionary<string, long> lengths, CancellationToken ct)
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

    internal async Task StageAutomaticSamplesAsync(IReadOnlyDictionary<string, long> lengths, IReadOnlyDictionary<string, string> hashes, CancellationToken ct)
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

}
