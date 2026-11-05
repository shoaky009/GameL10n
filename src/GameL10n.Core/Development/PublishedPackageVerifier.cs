using System.Text.Json;
using GameL10n.Core.Isolation;
using GameL10n.Core.Pipeline;
using GameL10n.Core.Storage;

namespace GameL10n.Core.Development;

public sealed record PublishedPackageVerification(
    string AdapterId, string AdapterVersion, DateTimeOffset CheckedAt, string Status,
    string? PackageFingerprint, string? SourceFingerprint, string? BaselineFingerprint,
    string Toolchain, int VerifierRevision, IReadOnlyList<string> Diagnostics,
    IReadOnlyList<string> DevelopmentPathReferences, bool RuntimeTested = false)
{
    public bool Passed => Status == "passed";
}

/// <summary>Rechecks a relocated release against a saved project baseline; never calls a model or activates a version.</summary>
public sealed class PublishedPackageVerifier(PinnedDotNetToolchain toolchain, IAdapterProcessRunner runner,
    EngineReferenceToolkit? toolkit = null)
{
    public async Task<PublishedPackageVerification> VerifyAsync(GeneratedAdapterPackage package, string projectDirectory,
        string workRoot, CancellationToken ct = default, Action<string, string>? progress = null)
    {
        projectDirectory = Path.GetFullPath(projectDirectory);
        if (!Directory.Exists(projectDirectory)) throw new DirectoryNotFoundException("游戏项目目录不存在。");
        // Share the pipeline's lock so source/extraction state cannot change during verification.
        await using var projectLock = new FileStream(Path.Combine(projectDirectory, ".task.lock"),
            FileMode.OpenOrCreate, FileAccess.ReadWrite, FileShare.None);
        var temporary = WorkspaceFiles.Resolve(workRoot, "verify-package-" + Guid.NewGuid().ToString("N"));
        string? packageFingerprint = null, sourceFingerprint = null, baselineFingerprint = null;
        var references = new List<string>();
        var diagnostics = new List<string>();
        var status = "failed";
        try
        {
            progress?.Invoke("检查发布包", "正在核对文件完整性、原文快照和已有提取记录。");
            ct.ThrowIfCancellationRequested();
            GeneratedAdapterResources.VerifyPackage(package);
            var files = package.ManifestVersion >= 2 ? package.PackageFiles!.Keys
                : new[] { "Program.cs", "adapter.dll", "adapter.runtimeconfig.json" }.AsEnumerable();
            var packageHashes = new SortedDictionary<string, string>(StringComparer.Ordinal);
            foreach (var file in files)
                packageHashes.Add(file, await LocalFiles.HashFileAsync(WorkspaceFiles.Resolve(package.DirectoryPath, file), ct));
            packageFingerprint = LocalFiles.HashText(JsonSerializer.Serialize(new
                { package = package with { DirectoryPath = "" }, files = packageHashes }));
            var snapshot = await LocalFiles.ReadJsonAsync<SourceSnapshot>(Path.Combine(projectDirectory, "source.json"), ct)
                ?? throw new InvalidDataException("项目缺少原文快照，无法验证；请先完成识别和提取。");
            if (snapshot.AdapterId != package.Id || snapshot.Files is not { Count: > 0 })
                throw new InvalidDataException("项目的原文快照不属于此引擎，或没有资源记录。");
            if (string.IsNullOrWhiteSpace(snapshot.Directory) || snapshot.Files.Any(file => file is null
                || string.IsNullOrWhiteSpace(file.RelativePath) || file.Sha256 is not { Length: 64 }
                || file.Sha256.Any(character => !Uri.IsHexDigit(character))))
                throw new InvalidDataException("原文快照记录的路径或校验值无效。");
            sourceFingerprint = LocalFiles.HashText(JsonSerializer.Serialize(snapshot.Files.OrderBy(file => file.RelativePath)));
            var source = LocalFiles.UnderRoot(projectDirectory, snapshot.Directory);
            if (Path.GetFileName(source) != sourceFingerprint)
                throw new InvalidDataException("原文快照路径与资源指纹不一致。");
            var baseline = await new ExtractionStore(projectDirectory).LoadAsync(sourceFingerprint,
                snapshot.AdapterId, snapshot.AdapterVersion, ct);
            if (baseline?.Units is not { Count: > 0 })
                throw new InvalidDataException("已有提取记录缺失或损坏，无法比较遗漏；本次不会重新生成基线。");
            baselineFingerprint = LocalFiles.HashText(JsonSerializer.Serialize(baseline.Units));

            var relocated = Path.Combine(temporary, "package");
            foreach (var (file, hash) in packageHashes)
            {
                ct.ThrowIfCancellationRequested();
                var original = WorkspaceFiles.Resolve(package.DirectoryPath, file);
                await GeneratedAdapterResources.CopyAsync(original, WorkspaceFiles.Resolve(relocated, file), ct);
                if (await LocalFiles.HashFileAsync(WorkspaceFiles.Resolve(relocated, file), ct) != hash)
                    throw new InvalidDataException("发布包在复制时发生变化：" + file);
                var extension = Path.GetExtension(file).ToLowerInvariant();
                if (extension is ".cs" or ".py" or ".ps1" or ".json" && new FileInfo(original).Length <= 1024 * 1024)
                {
                    var text = (await File.ReadAllTextAsync(original, ct)).Replace("\\\\", "\\").Replace('\\', '/');
                    if (new[] { Path.Combine(projectDirectory, "adaptation"), package.DirectoryPath }
                        .Any(path => text.Contains(Path.GetFullPath(path).Replace('\\', '/'), StringComparison.OrdinalIgnoreCase)))
                        references.Add(file);
                }
            }
            var candidate = package with { DirectoryPath = relocated };
            GeneratedAdapterResources.VerifyPackage(candidate);
            progress?.Invoke("准备验证输入", "正在复制并校验原文快照，不使用游戏安装目录中的补丁文件。");
            var input = Path.Combine(temporary, "source");
            var paths = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
            foreach (var file in snapshot.Files)
            {
                if (!paths.Add(file.RelativePath)) throw new InvalidDataException("原文快照中存在重复路径。");
                var target = WorkspaceFiles.Resolve(input, file.RelativePath);
                await GeneratedAdapterResources.CopyAsync(WorkspaceFiles.Resolve(source, file.RelativePath), target, ct);
                if (await LocalFiles.HashFileAsync(target, ct) != file.Sha256)
                    throw new InvalidDataException("原文快照内容已变化：" + file.RelativePath);
            }
            var adapter = new GeneratedGameAdapter(candidate, toolchain, runner, Path.Combine(temporary, "run"), toolkit, progress);
            var verification = await new GeneratedAdapterVerifier().VerifyAsync(adapter, input,
                Path.Combine(temporary, "checks"), ct, sourceIsSnapshot: true, baselineUnits: baseline.Units, requireExactBaseline: true);
            diagnostics.AddRange(verification.Diagnostics);
            status = verification.Passed ? "passed" : "failed";
            // Catch a package changing during a long run, including undeclared runtime writes.
            GeneratedAdapterResources.VerifyPackage(candidate);
            GeneratedAdapterResources.VerifyPackage(package);
            foreach (var (file, hash) in packageHashes)
                if (await LocalFiles.HashFileAsync(WorkspaceFiles.Resolve(relocated, file), ct) != hash
                    || await LocalFiles.HashFileAsync(WorkspaceFiles.Resolve(package.DirectoryPath, file), ct) != hash)
                    throw new InvalidDataException("发布包在验证过程中发生变化：" + file);
        }
        catch (OperationCanceledException) when (ct.IsCancellationRequested)
        { status = "cancelled"; diagnostics.Add("验证已取消，没有更新适配器或游戏文件。"); }
        catch (Exception error) when (error is IOException or InvalidDataException or GeneratedAdapterException or JsonException
            or UnauthorizedAccessException or AdapterProcessLimitException or AdapterProcessUnavailableException)
        { status = "failed"; diagnostics.Add(GeneratedGameAdapter.LimitDiagnostic(error.Message)); }
        finally { GeneratedGameAdapter.DeleteStagedTree(temporary); }

        var result = new PublishedPackageVerification(package.Id, package.Version, DateTimeOffset.UtcNow, status,
            packageFingerprint, sourceFingerprint, baselineFingerprint, $"SDK {toolchain.SdkVersion}; Runtime {toolchain.RuntimeVersion}",
            GeneratedAdapterVerifier.Revision, diagnostics, references);
        // One latest report per project. It is evidence for these exact inputs, never a reusable execution permission.
        await LocalFiles.WriteJsonAsync(Path.Combine(projectDirectory, "package-verification.json"), result, CancellationToken.None);
        return result;
    }
}
