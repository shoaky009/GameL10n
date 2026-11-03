using System.Text.Json;
using System.Text.Json.Nodes;
using AvgTranslator.Core.Adapters;
using AvgTranslator.Core.Isolation;
using AvgTranslator.Core.Storage;

namespace AvgTranslator.Core.Development;

public sealed partial class AdaptationTools
{
    private void ClearVerification()
    {
        session.VerifiedPackage = null;
        session.VerifiedCoverageFingerprint = null;
        session.VerifiedObjective = null;
        session.VerifiedBaselineFingerprint = null;
    }

    private void AcceptVerification(GeneratedAdapterPackage package)
    {
        session.VerifiedPackage = package;
        session.VerificationRevision = GeneratedAdapterVerifier.Revision;
        session.VerifiedCoverageFingerprint = session.TextCoverage.Fingerprint();
        session.VerifiedObjective = session.Objective;
        session.VerifiedBaselineFingerprint = session.Objective == AdaptationObjective.Repair ? BaselineFingerprint() : null;
    }

    private async Task<string?> VerificationFingerprintAsync(AdapterFamily family, IReadOnlyList<string> patterns,
        IReadOnlyList<AdapterReleaseFile> releaseFiles, bool discoveryOnly, CancellationToken ct)
    {
        var game = GameDiscoverySession.Scan(request.GameDirectory, ct);
        var staged = GameDiscoverySession.Scan(ResourceDirectory, ct);
        // An incomplete inventory cannot establish that the discovery inputs are identical.
        if (!game.Complete || !staged.Complete)
        {
            await CheckResourcesAsync(ct);
            return null;
        }
        var recognitionPatterns = session.Recognition is { } recognition
            ? recognition.Required.Concat(recognition.Hints).SelectMany(rule => rule.Patterns) : [];
        var gameFiles = game.Select(patterns.Concat(recognitionPatterns).Append("**/*.exe.le.config").ToArray(), ct)
            .Concat(session.ResourceHashes.Keys).Distinct(StringComparer.Ordinal).Order(StringComparer.Ordinal);
        var stagedFiles = staged.Select(patterns, ct).Concat(session.ResourceHashes.Keys)
            .Distinct(StringComparer.Ordinal).Order(StringComparer.Ordinal);
        var sources = new Dictionary<string, string>(StringComparer.Ordinal);
        foreach (var file in releaseFiles)
            sources[file.Path] = await LocalFiles.HashFileAsync(AdapterReleaseFiles.SourcePath(SourceDirectory, file), ct);
        async Task<Dictionary<string, string>> HashFiles(string root, IEnumerable<string> files)
        {
            var hashes = new Dictionary<string, string>(StringComparer.Ordinal);
            foreach (var file in files) hashes[file] = await LocalFiles.HashFileAsync(WorkspaceFiles.Resolve(root, file), ct);
            return hashes;
        }
        var gameHashes = await HashFiles(request.GameDirectory, gameFiles);
        var stagedHashes = await HashFiles(ResourceDirectory, stagedFiles);
        foreach (var (relative, hash) in session.ResourceHashes)
            if (gameHashes[relative] != hash || stagedHashes[relative] != hash)
                throw new InvalidDataException("原资源或工作副本发生变化，请重新 stage_resources 并验证：" + relative);
        return LocalFiles.HashText(JsonSerializer.Serialize(new
        {
            cacheSchema = 1, verifierRevision = GeneratedAdapterVerifier.Revision,
            hostBuild = typeof(AdaptationTools).Module.ModuleVersionId,
            compiler.Toolchain, gameDirectory = Path.GetFullPath(request.GameDirectory),
            family, patterns = patterns.Select(GeneratedAdapterResources.Normalize).Distinct(StringComparer.OrdinalIgnoreCase).Order(StringComparer.Ordinal),
            releaseFiles, sources, discoveryOnly, session.Recognition, session.Objective, session.LaunchInspection,
            coverage = session.TextCoverage.Fingerprint(),
            baseline = session.Objective == AdaptationObjective.Repair ? BaselineFingerprint() : null,
            gameResources = gameHashes, stagedResources = stagedHashes,
            approvedResources = session.ResourceHashes.OrderBy(pair => pair.Key, StringComparer.Ordinal)
        }, Json));
    }

    private async Task<object?> ReuseVerificationAsync(string? fingerprint, CancellationToken ct)
    {
        var cached = session.VerificationCache;
        if (fingerprint is null || cached is null || cached.Fingerprint != fingerprint
            || cached.Revision != GeneratedAdapterVerifier.Revision) return null;
        if (cached.Package is { } package)
        {
            try
            {
                GeneratedAdapterResources.VerifyPackage(package);
                // Shared parser/Python dependencies are pinned separately from package files.
                // Resolve them again so removal or corruption cannot be hidden by a cache hit.
                await EngineToolkit.ResolvePackageAsync(package.DirectoryPath, ct);
            }
            catch (Exception error) when (error is IOException or GeneratedAdapterException or InvalidDataException)
            { return null; }
        }
        if (cached.CompilationError is { } errorMessage) throw new GeneratedAdapterException(errorMessage);
        if (cached.Result is not { ValueKind: JsonValueKind.Object } saved || cached.Package is null) return null;
        var result = JsonNode.Parse(saved.GetRawText())!.AsObject();
        session.VerificationDiagnostics = saved.GetProperty("diagnostics").Deserialize<string[]>(Json)!;
        if (saved.GetProperty("passed").GetBoolean() && saved.GetProperty("stage").GetString() == "finish-ready")
            AcceptVerification(cached.Package);
        result["reused"] = true;
        result["compileMs"] = 0;
        result["discoveryMs"] = 0;
        result["verificationMs"] = 0;
        result["pendingCoverage"] = JsonSerializer.SerializeToNode(session.TextCoverage.Pending, Json);
        result["runtimeChecks"] = JsonSerializer.SerializeToNode(session.TextCoverage.RuntimeChecks, Json);
        reportProgress?.Invoke("复用适配器验收", "验收输入未变化，沿用已完成的资源检查结果；游戏运行效果仍需实际验证。");
        return result;
    }
}

/// <summary>Only the last completed check is retained; interrupted operations never create an entry.</summary>
public sealed record AdapterVerificationCache(string Fingerprint, int Revision, GeneratedAdapterPackage? Package,
    JsonElement? Result = null, string? CompilationError = null);
