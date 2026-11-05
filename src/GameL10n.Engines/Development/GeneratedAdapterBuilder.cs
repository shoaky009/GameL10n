using System.Text;
using System.Text.Json;
using GameL10n.Core.Adapters;
using GameL10n.Core.Isolation;

namespace GameL10n.Core.Development;

/// <summary>Checks Python syntax and packages explicitly selected adapter inputs without compiling user code.</summary>
public sealed class GeneratedAdapterBuilder(IAdapterProcessRunner runner)
{
    public async Task<GeneratedAdapterPackage> BuildWorkspaceAsync(string sourceDirectory, string destinationDirectory,
        IReadOnlyList<string> resourcePatterns, IReadOnlyList<AdapterReleaseFile> releaseFiles, EnginePythonPaths python,
        CancellationToken ct = default, AdapterFamily? family = null, AdapterRecognition? recognition = null)
    {
        family?.Validate();
        recognition?.Validate();
        GeneratedAdapterResources.ValidatePatterns(resourcePatterns);
        var selection = AdapterReleaseFiles.Validate(releaseFiles);
        if (!selection.Any(file => file.Path == "src/adapter.py" && file.Role == "source")
            || selection.Any(file => file.Role == "source" && !file.Path.EndsWith(".py", StringComparison.OrdinalIgnoreCase)))
            throw new GeneratedAdapterException("新适配器必须使用 src/adapter.py 入口及 Python 源码；旧 C# 实现需迁移后再发布。");
        if (!selection.Any(file => file.Path == EngineReferenceToolkit.DependencyPath && file.Role == "runtime"))
            throw new GeneratedAdapterException("请先准备固定版本的共享 Python 依赖。");
        sourceDirectory = Path.GetFullPath(sourceDirectory);
        destinationDirectory = Path.GetFullPath(destinationDirectory);
        var comparison = OperatingSystem.IsWindows() ? StringComparison.OrdinalIgnoreCase : StringComparison.Ordinal;
        if (destinationDirectory.Equals(sourceDirectory, comparison)
            || destinationDirectory.StartsWith(sourceDirectory + Path.DirectorySeparatorChar, comparison)
            || sourceDirectory.StartsWith(destinationDirectory + Path.DirectorySeparatorChar, comparison))
            throw new ArgumentException("Source and build directories must be separate.");
        var pending = destinationDirectory + ".build-" + Guid.NewGuid().ToString("N");
        WorkspaceFiles.RejectLinkAncestors(pending);
        Directory.CreateDirectory(pending);
        try
        {
            foreach (var file in selection)
                await GeneratedAdapterResources.CopyAsync(AdapterReleaseFiles.SourcePath(sourceDirectory, file),
                    WorkspaceFiles.Resolve(pending, file.Path), ct);
            // Parsing checks all released Python files without importing them, running their code or writing pycache.
            var sources = selection.Where(file => file.Path.EndsWith(".py", StringComparison.OrdinalIgnoreCase))
                .Select(file => WorkspaceFiles.Resolve(pending, file.Path)).ToArray();
            const string syntax = "import ast,pathlib,sys; [ast.parse(pathlib.Path(p).read_text(encoding='utf-8-sig'),filename=p) for p in sys.argv[1:]]";
            var checkedSyntax = await runner.RunAsync(new(python.Python, ["-I", "-B", "-X", "utf8", "-c", syntax, .. sources],
                pending, Timeout: TimeSpan.FromSeconds(30), MaximumOutputBytes: 64 * 1024), ct);
            if (checkedSyntax.ExitCode != 0)
                throw new GeneratedAdapterException("Python 语法检查失败：" + GeneratedAdapterCompiler.LimitDiagnostic(checkedSyntax.StandardOutput + "\n" + checkedSyntax.StandardError));
            var files = GeneratedGameAdapter.EnumerateFiles(pending).ToDictionary(path =>
                GeneratedAdapterResources.Normalize(Path.GetRelativePath(pending, path)), GeneratedAdapterResources.HashFile, StringComparer.Ordinal);
            var sourceHash = GeneratedAdapterResources.HashEntries(files.Where(pair => pair.Key.StartsWith("src/", StringComparison.Ordinal)));
            var patterns = resourcePatterns.Select(GeneratedAdapterResources.Normalize).Distinct(StringComparer.OrdinalIgnoreCase).Order(StringComparer.Ordinal).ToArray();
            var version = LocalVersion(files, patterns, family, selection, recognition);
            var package = new GeneratedAdapterPackage(family?.AdapterId ?? "unpublished", version, pending, [], sourceHash,
                files["src/adapter.py"], family, patterns, files, ManifestVersion: 4, ReleaseFiles: selection, Recognition: recognition);
            GeneratedAdapterResources.VerifyPackage(package);
            ct.ThrowIfCancellationRequested();
            WorkspaceFiles.RejectLinkAncestors(destinationDirectory);
            if (Directory.Exists(destinationDirectory))
            {
                GeneratedGameAdapter.DeleteStagedTree(destinationDirectory);
                if (Directory.Exists(destinationDirectory)) throw new IOException("The previous adapter build could not be replaced.");
            }
            Directory.CreateDirectory(Path.GetDirectoryName(destinationDirectory)!);
            Directory.Move(pending, destinationDirectory);
            return package with { DirectoryPath = destinationDirectory };
        }
        finally { GeneratedGameAdapter.DeleteStagedTree(pending); }
    }

    private static string LocalVersion(IReadOnlyDictionary<string, string> files, IReadOnlyList<string> patterns,
        AdapterFamily? family, IReadOnlyList<AdapterReleaseFile> selection, AdapterRecognition? recognition) =>
        Convert.ToHexStringLower(System.Security.Cryptography.SHA256.HashData(Encoding.UTF8.GetBytes("python-v4\n"
            + GeneratedAdapterResources.HashEntries(files) + "\n" + JsonSerializer.Serialize(new { patterns, family, selection, recognition }))))[..16];
}
