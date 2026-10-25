using System.Security.Cryptography;
using System.Text.Json;
using AvgTranslator.Core.Isolation;
using AvgTranslator.Core.Adapters;

namespace AvgTranslator.Core.Development;

/// <summary>Stages only approved resources. Generated code never receives the original game directory.</summary>
public sealed class GeneratedGameAdapter(
    GeneratedAdapterPackage package,
    PinnedDotNetToolchain toolchain,
    IIsolatedProcessRunner runner,
    string workRoot) : IGameAdapter
{
    private const long MaximumResourceBytes = 32L * 1024 * 1024;
    private const long MaximumTotalResourceBytes = 128L * 1024 * 1024;
    private static readonly JsonSerializerOptions Json = new(JsonSerializerDefaults.Web) { MaxDepth = 32 };

    public string Id => package.Id;
    public string Version => package.Version;
    public GeneratedAdapterPackage Package => package;
    public AdapterCatalogEntry? CatalogEntry => package.Family is { } family ? new(family, Version, []) : null;

    public async Task<GameProbe?> ProbeAsync(string gameDirectory, CancellationToken cancellationToken = default)
    {
        // A published template can be tried on other games without treating absent resources as an error.
        if (package.ResourceFiles.Any(path => !File.Exists(WorkspaceFiles.Resolve(gameDirectory, path)))) return null;
        var response = await InvokeAsync("probe", gameDirectory, null, null, cancellationToken);
        if (response.Probe is null) return null;
        if (response.Probe.RequiredFiles is null || response.Probe.Warnings is null)
            throw new GeneratedAdapterException("The extension returned an invalid probe result.");
        foreach (var path in response.Probe.RequiredFiles) RequireResourcePath(path);
        return response.Probe with { EngineId = package.Family?.Key ?? response.Probe.EngineId,
            AdapterId = Id, AdapterVersion = Version, OriginalExecutable = null };
    }

    public async Task<ExtractionResult> ExtractAsync(string sourceDirectory, CancellationToken cancellationToken = default)
    {
        var result = (await InvokeAsync("extract", sourceDirectory, null, null, cancellationToken)).Extraction
            ?? throw new GeneratedAdapterException("The extension returned no extraction result.");
        if (result.Units is null || result.Warnings is null || result.Units.Count is < 1 or > 100_000) throw new GeneratedAdapterException("The extension returned an empty or oversized extraction.");
        if (result.Units.Any(unit => unit is null || string.IsNullOrWhiteSpace(unit.Id) || unit.Id.Length > 512
            || unit.SourceText is null || unit.Locator is null || unit.Context is null)
            || result.Units.Select(unit => unit.Id).Distinct(StringComparer.Ordinal).Count() != result.Units.Count)
            throw new GeneratedAdapterException("The extension returned duplicate or invalid translation units.");
        foreach (var unit in result.Units) RequireResourcePath(unit.ResourcePath);
        return result;
    }

    public async Task<AdapterBuildResult> BuildPatchAsync(string sourceDirectory,
        IReadOnlyDictionary<string, string> translations, string patchDirectory, CancellationToken cancellationToken = default)
    {
        return (await InvokeAsync("build", sourceDirectory, translations, patchDirectory, cancellationToken)).Build
            ?? throw new GeneratedAdapterException("The extension returned no build result.");
    }

    public async Task<ValidationResult> ValidatePatchAsync(string sourceDirectory, string patchDirectory, CancellationToken cancellationToken = default)
    {
        var validation = (await InvokeAsync("validate", sourceDirectory, null, patchDirectory, cancellationToken)).Validation
            ?? throw new GeneratedAdapterException("The extension returned no validation result.");
        if (validation.Issues is null || validation.Issues.Any(issue => issue is null || issue.Code is null || issue.Message is null))
            throw new GeneratedAdapterException("The extension returned an invalid validation result.");
        // A resource parser cannot establish a successful game runtime test.
        return validation with { RuntimeStatus = "not-tested" };
    }

    private async Task<CliResponse> InvokeAsync(string operation, string sourceDirectory,
        IReadOnlyDictionary<string, string>? translations, string? patchDirectory, CancellationToken cancellationToken)
    {
        VerifyPackage();
        Directory.CreateDirectory(workRoot);
        var workspace = WorkspaceFiles.Resolve(workRoot, "run-" + Guid.NewGuid().ToString("N"));
        var source = Path.Combine(workspace, "source");
        var output = Path.Combine(workspace, "output");
        WindowsAppContainerProcessRunner.CreateLowIntegrityDirectory(workspace);
        Directory.CreateDirectory(source);
        Directory.CreateDirectory(output);
        try
        {
            StageResources(sourceDirectory, source);
            var baseline = ResourceHashes(source);
            if (operation == "validate")
            {
                if (patchDirectory is null) throw new ArgumentException("Validation requires a staged patch.");
                StageResources(patchDirectory, output, allowMissing: true);
            }
            var request = JsonSerializer.Serialize(new
            {
                protocolVersion = 1, operation, sourceDirectory = source, outputDirectory = output, translations
            }, Json);
            var result = await runner.RunAsync(new IsolatedProcessRequest(toolchain.ExecutablePath,
                [Path.Combine(package.DirectoryPath, "adapter.dll")], workspace,
                [Path.GetFullPath(toolchain.RootDirectory), Path.GetFullPath(package.DirectoryPath)], request,
                Timeout: TimeSpan.FromSeconds(25), MaximumOutputBytes: 4 * 1024 * 1024), cancellationToken);
            if (result.ExitCode != 0)
                throw new GeneratedAdapterException("The extension process failed: " + GeneratedAdapterCompiler.LimitDiagnostic(result.StandardError));
            if (!HashesEqual(baseline, ResourceHashes(source)))
                throw new GeneratedAdapterException("The extension modified staged source resources.");
            CliResponse response;
            try { response = JsonSerializer.Deserialize<CliResponse>(result.StandardOutput, Json) ?? throw new JsonException("Empty JSON."); }
            catch (JsonException exception) { throw new GeneratedAdapterException("The extension returned invalid protocol JSON: " + exception.Message); }
            if (response.ProtocolVersion != 1 || !response.Success)
                throw new GeneratedAdapterException("The extension rejected the operation: " + GeneratedAdapterCompiler.LimitDiagnostic(response.Error ?? "Invalid protocol version or unsuccessful response."));
            if (operation == "build")
            {
                if (response.Build is null || response.Build.Files is null || response.Build.Warnings is null || patchDirectory is null) throw new GeneratedAdapterException("Missing or invalid build response.");
                if (response.Build.LaunchExecutable is not null)
                    throw new GeneratedAdapterException("Generated resource adapters cannot return a launch executable.");
                var files = response.Build.Files;
                if (files.Count == 0 || files.Distinct(StringComparer.OrdinalIgnoreCase).Count() != files.Count)
                    throw new GeneratedAdapterException("The extension returned no build files or duplicate paths.");
                var actual = EnumerateFiles(output).Select(file => Path.GetRelativePath(output, file).Replace('\\', '/')).ToHashSet(StringComparer.OrdinalIgnoreCase);
                var declared = files.Select(path => path.Replace('\\', '/')).ToHashSet(StringComparer.OrdinalIgnoreCase);
                if (!actual.SetEquals(declared)) throw new GeneratedAdapterException("Build output does not match its declared file list.");
                // Check every file before copying any output to the caller's patch directory.
                foreach (var file in files)
                {
                    RequireResourcePath(file);
                    _ = WorkspaceFiles.ReadBounded(output, file, MaximumResourceBytes);
                    _ = WorkspaceFiles.Resolve(patchDirectory, file);
                }
                foreach (var file in files)
                {
                    var destination = WorkspaceFiles.Resolve(patchDirectory, file);
                    Directory.CreateDirectory(Path.GetDirectoryName(destination)!);
                    File.Copy(WorkspaceFiles.Resolve(output, file), destination, false);
                }
            }
            return response;
        }
        finally
        {
            // Runs are private copies, never game directories. Refuse link traversal on cleanup.
            DeleteStagedTree(workspace);
        }
    }

    private void VerifyPackage()
    {
        var bytes = WorkspaceFiles.ReadBounded(package.DirectoryPath, "adapter.dll", 8 * 1024 * 1024);
        if (!string.Equals(Convert.ToHexStringLower(SHA256.HashData(bytes)), package.AssemblySha256, StringComparison.Ordinal))
            throw new GeneratedAdapterException("The activated extension assembly failed its integrity check.");
    }

    private void RequireResourcePath(string path)
    {
        _ = WorkspaceFiles.Resolve(workRoot, path);
        if (!package.ResourceFiles.Any(resource => string.Equals(resource.Replace('\\', '/'), path.Replace('\\', '/'), StringComparison.OrdinalIgnoreCase)))
            throw new GeneratedAdapterException("The extension referenced a resource outside the approved sample set.");
    }

    private void StageResources(string from, string destination, bool allowMissing = false)
    {
        long total = 0;
        foreach (var resource in package.ResourceFiles)
        {
            var original = WorkspaceFiles.Resolve(from, resource);
            if (allowMissing && !File.Exists(original)) continue;
            var bytes = WorkspaceFiles.ReadBounded(from, resource, MaximumResourceBytes);
            total += bytes.Length;
            if (total > MaximumTotalResourceBytes) throw new GeneratedAdapterException("The approved resources exceed the staging size limit.");
            var copy = WorkspaceFiles.Resolve(destination, resource);
            Directory.CreateDirectory(Path.GetDirectoryName(copy)!);
            File.WriteAllBytes(copy, bytes);
        }
    }

    private Dictionary<string, string> ResourceHashes(string root) => package.ResourceFiles.ToDictionary(file => file,
        file => Convert.ToHexStringLower(SHA256.HashData(WorkspaceFiles.ReadBounded(root, file, MaximumResourceBytes))), StringComparer.Ordinal);

    private static bool HashesEqual(Dictionary<string, string> left, Dictionary<string, string> right) => left.Count == right.Count && left.All(pair => right.TryGetValue(pair.Key, out var value) && pair.Value == value);

    internal static IEnumerable<string> EnumerateFiles(string directory)
    {
        WorkspaceFiles.RejectReparsePoints(directory);
        foreach (var child in Directory.EnumerateFileSystemEntries(directory))
        {
            WorkspaceFiles.RejectReparsePoints(child);
            if (Directory.Exists(child))
            {
                foreach (var file in EnumerateFiles(child)) yield return file;
            }
            else yield return child;
        }
    }

    private static void DeleteStagedTree(string directory)
    {
        if (!Directory.Exists(directory)) return;
        try
        {
            // Directory.Delete does not follow directory reparse points, but reject these explicitly too.
            _ = EnumerateFiles(directory).ToArray();
            Directory.Delete(directory, true);
        }
        catch (Exception exception) when (exception is IOException or UnauthorizedAccessException or InvalidDataException)
        {
            // A failed cleanup cannot replace the operation's diagnostic or escape its staging root.
        }
    }

    private sealed record CliResponse(int ProtocolVersion, bool Success, string? Error,
        GameProbe? Probe, ExtractionResult? Extraction, AdapterBuildResult? Build, ValidationResult? Validation);
}
