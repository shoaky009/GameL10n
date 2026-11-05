using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Text.RegularExpressions;
using GameL10n.Core.Isolation;
using GameL10n.Core.Adapters;

namespace GameL10n.Core.Development;

/// <summary>Developer SDK compiler for legacy packages. Automatic adaptation uses the Python builder.</summary>
public sealed partial class GeneratedAdapterCompiler(PinnedDotNetToolchain toolchain, IAdapterProcessRunner runner)
{
    public PinnedDotNetToolchain Toolchain { get; } = toolchain;
    public IAdapterProcessRunner Runner { get; } = runner;

    public async Task CheckAvailabilityAsync(string workspace, CancellationToken cancellationToken = default)
    {
        if (!File.Exists(Toolchain.ExecutablePath))
            throw new AdapterProcessUnavailableException("缺少开发用 .NET SDK，请配置固定版本的开发工具链。");
        Directory.CreateDirectory(workspace);
        var result = await Runner.RunAsync(new AdapterProcessRequest(Toolchain.ExecutablePath,
            ["--info"], workspace, Timeout: TimeSpan.FromSeconds(20)), cancellationToken);
        if (result.ExitCode != 0)
            throw new AdapterProcessUnavailableException($".NET 适配工具启动失败（退出码 {result.ExitCode}）：" + LimitDiagnostic(result.StandardError));
    }

    public async Task<GeneratedAdapterPackage> CompileWorkspaceAsync(string sourceDirectory, string destinationDirectory,
        IReadOnlyList<string> resourcePatterns, IReadOnlyList<AdapterReleaseFile> releaseFiles,
        CancellationToken cancellationToken = default, AdapterFamily? family = null, AdapterRecognition? recognition = null)
    {
        family?.Validate();
        recognition?.Validate();
        GeneratedAdapterResources.ValidatePatterns(resourcePatterns);
        sourceDirectory = Path.GetFullPath(sourceDirectory);
        destinationDirectory = Path.GetFullPath(destinationDirectory);
        var comparison = OperatingSystem.IsWindows() ? StringComparison.OrdinalIgnoreCase : StringComparison.Ordinal;
        if (destinationDirectory.Equals(sourceDirectory, comparison)
            || destinationDirectory.StartsWith(sourceDirectory + Path.DirectorySeparatorChar, comparison)
            || sourceDirectory.StartsWith(destinationDirectory + Path.DirectorySeparatorChar, comparison))
            throw new ArgumentException("Source and build directories must be separate.");
        var selection = AdapterReleaseFiles.Validate(releaseFiles);
        var inputs = selection.Select(file => AdapterReleaseFiles.SourcePath(sourceDirectory, file)).ToArray();
        var sources = inputs.Where(path => path.EndsWith(".cs", StringComparison.OrdinalIgnoreCase)
            && !GeneratedAdapterResources.Normalize(Path.GetRelativePath(sourceDirectory, path)).StartsWith("tools/", StringComparison.OrdinalIgnoreCase)).ToArray();
        if (sources.Length == 0) throw new GeneratedAdapterException("The adapter workspace contains no C# source files.");
        var pending = destinationDirectory + ".compile-" + Guid.NewGuid().ToString("N");
        WorkspaceFiles.RejectLinkAncestors(pending);
        Directory.CreateDirectory(pending);
        try
        {
            foreach (var file in inputs)
            {
                var relative = GeneratedAdapterResources.Normalize(Path.GetRelativePath(sourceDirectory, file));
                var target = relative.StartsWith("tools/", StringComparison.OrdinalIgnoreCase) ? relative : "src/" + relative;
                await GeneratedAdapterResources.CopyAsync(file, WorkspaceFiles.Resolve(pending, target), cancellationToken);
            }
            var compiledSources = sources.Select(path => WorkspaceFiles.Resolve(pending,
                "src/" + GeneratedAdapterResources.Normalize(Path.GetRelativePath(sourceDirectory, path)))).Order(StringComparer.Ordinal).ToArray();
            RequireCompilation(await CompileSourcesAsync(compiledSources, pending, pending, cancellationToken));
            var files = GeneratedGameAdapter.EnumerateFiles(pending).ToDictionary(
                path => GeneratedAdapterResources.Normalize(Path.GetRelativePath(pending, path)), GeneratedAdapterResources.HashFile, StringComparer.Ordinal);
            var sourceHash = GeneratedAdapterResources.HashEntries(files.Where(pair => pair.Key.StartsWith("src/", StringComparison.Ordinal)
                && pair.Key.EndsWith(".cs", StringComparison.OrdinalIgnoreCase)));
            var normalizedPatterns = resourcePatterns.Select(GeneratedAdapterResources.Normalize).Distinct(StringComparer.OrdinalIgnoreCase).Order(StringComparer.Ordinal).ToArray();
            var versionHash = HashText(GeneratedAdapterResources.HashEntries(files) + "\n" + Toolchain.RuntimeVersion + "\n"
                + JsonSerializer.Serialize(normalizedPatterns) + "\n" + JsonSerializer.Serialize(family)
                + "\n" + JsonSerializer.Serialize(selection)
                + (recognition is null ? "" : "\n" + JsonSerializer.Serialize(recognition)));
            var package = new GeneratedAdapterPackage(family?.AdapterId ?? "unpublished", versionHash[..16], pending, [],
                sourceHash, files["adapter.dll"], family, normalizedPatterns, files,
                ManifestVersion: 3, ReleaseFiles: selection, Recognition: recognition);
            GeneratedAdapterResources.VerifyPackage(package);
            cancellationToken.ThrowIfCancellationRequested();
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

    // Keep the original single-file package contract available to existing callers.
    public async Task<GeneratedAdapterPackage> CompileAsync(string sourceCode, string workspace,
        IReadOnlyList<string> resourceFiles, CancellationToken cancellationToken = default, AdapterFamily? family = null)
    {
        family?.Validate();
        Directory.CreateDirectory(workspace);
        var build = WorkspaceFiles.Resolve(workspace, "build");
        Directory.CreateDirectory(build);
        await File.WriteAllTextAsync(Path.Combine(build, "Program.cs"), sourceCode, new UTF8Encoding(false), cancellationToken);
        RequireCompilation(await CompileSourcesAsync([Path.Combine(build, "Program.cs")], build, workspace, cancellationToken));
        var sourceHash = HashText(sourceCode);
        var versionHash = HashText(sourceHash + "\n" + Toolchain.RuntimeVersion + "\n" + string.Join('\n', resourceFiles.Order(StringComparer.Ordinal)));
        return new GeneratedAdapterPackage(family?.AdapterId ?? "unpublished", versionHash[..16], build,
            resourceFiles.ToArray(), sourceHash, GeneratedAdapterResources.HashFile(Path.Combine(build, "adapter.dll")), family);
    }

    private async Task<AdapterProcessResult> CompileSourcesAsync(IReadOnlyList<string> sources, string build, string workingDirectory,
        CancellationToken cancellationToken)
    {
        if (!VersionPattern().IsMatch(Toolchain.SdkVersion) || !VersionPattern().IsMatch(Toolchain.RuntimeVersion)
            || !Toolchain.RuntimeVersion.StartsWith("10.", StringComparison.Ordinal))
            throw new AdapterProcessUnavailableException("A pinned development .NET 10 SDK is required for legacy adapter compilation.");
        var root = Path.GetFullPath(Toolchain.RootDirectory);
        var compiler = Path.Combine(root, "sdk", Toolchain.SdkVersion, "Roslyn", "bincore", "csc.dll");
        var referencePath = Path.Combine(root, "packs", "Microsoft.NETCore.App.Ref", Toolchain.RuntimeVersion, "ref", "net10.0");
        if (!File.Exists(Toolchain.ExecutablePath) || !File.Exists(compiler) || !Directory.Exists(referencePath))
            throw new AdapterProcessUnavailableException("缺少开发用 .NET 编译器或引用程序集；发行包仅支持 Python 适配开发，旧格式编译需开发机 SDK。");
        foreach (var path in new[] { root, compiler, referencePath }) WorkspaceFiles.RejectLinkAncestors(path);
        var references = Directory.GetFiles(referencePath, "*.dll").Order(StringComparer.Ordinal).ToArray();
        if (references.Length == 0) throw new AdapterProcessUnavailableException("The pinned framework reference pack is empty.");
        var arguments = new List<string>
        {
            "/nologo", "/nostdlib+", "/target:exe", "/langversion:latest", "/nullable:enable", "/codepage:65001", "/utf8output",
            // Embedded symbols give adapter failures source lines without keeping separate PDB files.
            "/debug:embedded", "/optimize+",
            "/deterministic+", "/unsafe-", "/out:\"" + Path.Combine(build, "adapter.dll") + "\"",
            "/pathmap:\"" + build + "=/adapter\""
        };
        arguments.AddRange(references.Select(path => "/reference:\"" + path + "\""));
        arguments.AddRange(sources.Select(path => "\"" + path + "\""));
        var responseFile = Path.Combine(build, "compiler.rsp");
        await File.WriteAllLinesAsync(responseFile, arguments, new UTF8Encoding(false), cancellationToken);
        AdapterProcessResult result;
        try
        {
            result = await Runner.RunAsync(new AdapterProcessRequest(Toolchain.ExecutablePath,
                [compiler, "/noconfig", "@" + responseFile], workingDirectory, Timeout: TimeSpan.FromMinutes(2)), cancellationToken);
            if (result.ExitCode != 0) return result;
            if (!File.Exists(Path.Combine(build, "adapter.dll"))) throw new GeneratedAdapterException("The compiler produced no executable assembly.");
        }
        finally { File.Delete(responseFile); }
        await File.WriteAllTextAsync(Path.Combine(build, "adapter.runtimeconfig.json"), JsonSerializer.Serialize(new
        {
            runtimeOptions = new { tfm = "net10.0", rollForward = "Disable", framework = new { name = "Microsoft.NETCore.App", version = Toolchain.RuntimeVersion } }
        }), cancellationToken);
        return result;
    }

    private static void RequireCompilation(AdapterProcessResult result)
    {
        if (result.ExitCode != 0)
            throw new GeneratedAdapterException("Compilation failed: " + LimitDiagnostic(result.StandardOutput + "\n" + result.StandardError));
    }

    private static string HashText(string value) => Convert.ToHexStringLower(SHA256.HashData(Encoding.UTF8.GetBytes(value)));
    internal static string LimitDiagnostic(string value) => GeneratedGameAdapter.LimitDiagnostic(value);

    [GeneratedRegex("^[0-9]+\\.[0-9]+\\.[0-9]+(?:-[A-Za-z0-9.-]+)?$")]
    private static partial Regex VersionPattern();
}
