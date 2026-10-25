using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Text.RegularExpressions;
using AvgTranslator.Core.Isolation;
using AvgTranslator.Core.Adapters;

namespace AvgTranslator.Core.Development;

/// <summary>Compiles C# with pinned framework references; project files, build tasks and package restore are not executed.</summary>
public sealed partial class GeneratedAdapterCompiler(PinnedDotNetToolchain toolchain, IAdapterProcessRunner runner)
{
    public PinnedDotNetToolchain Toolchain { get; } = toolchain;
    public IAdapterProcessRunner Runner { get; } = runner;

    public async Task CheckAvailabilityAsync(string workspace, CancellationToken cancellationToken = default)
    {
        if (!File.Exists(Toolchain.ExecutablePath))
            throw new AdapterProcessUnavailableException("缺少 .NET 适配工具链，请使用包含 toolchain 的完整发行包。");
        Directory.CreateDirectory(workspace);
        var result = await Runner.RunAsync(new AdapterProcessRequest(Toolchain.ExecutablePath,
            ["--info"], workspace, Timeout: TimeSpan.FromSeconds(20)), cancellationToken);
        if (result.ExitCode != 0)
            throw new AdapterProcessUnavailableException($".NET 适配工具启动失败（退出码 {result.ExitCode}）：" + LimitDiagnostic(result.StandardError));
    }

    public async Task<GeneratedAdapterPackage> CompileWorkspaceAsync(string sourceDirectory, string destinationDirectory,
        IReadOnlyList<string> resourcePatterns, CancellationToken cancellationToken = default, AdapterFamily? family = null)
    {
        family?.Validate();
        GeneratedAdapterResources.ValidatePatterns(resourcePatterns);
        sourceDirectory = Path.GetFullPath(sourceDirectory);
        destinationDirectory = Path.GetFullPath(destinationDirectory);
        var comparison = OperatingSystem.IsWindows() ? StringComparison.OrdinalIgnoreCase : StringComparison.Ordinal;
        if (destinationDirectory.Equals(sourceDirectory, comparison)
            || destinationDirectory.StartsWith(sourceDirectory + Path.DirectorySeparatorChar, comparison)
            || sourceDirectory.StartsWith(destinationDirectory + Path.DirectorySeparatorChar, comparison))
            throw new ArgumentException("Source and build directories must be separate.");
        var inputs = SourceInputs(sourceDirectory);
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
                + JsonSerializer.Serialize(normalizedPatterns) + "\n" + JsonSerializer.Serialize(family));
            var package = new GeneratedAdapterPackage(family?.AdapterId ?? "unpublished", versionHash[..16], pending, [],
                sourceHash, files["adapter.dll"], family, normalizedPatterns, files, ManifestVersion: 2);
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

    internal static string[] SourceInputs(string sourceDirectory) => Directory.Exists(sourceDirectory)
        ? GeneratedGameAdapter.EnumerateFiles(sourceDirectory).Where(path => path.EndsWith(".cs", StringComparison.OrdinalIgnoreCase)
            || GeneratedAdapterResources.Normalize(Path.GetRelativePath(sourceDirectory, path)).StartsWith("tools/", StringComparison.OrdinalIgnoreCase)).ToArray()
        : [];

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

    public async Task<(bool Compiled, AdapterProcessResult Result)> RunCSharpAsync(string workspace,
        IReadOnlyList<string> sources, IReadOnlyList<string> references, IReadOnlyList<string> arguments,
        string workingDirectory, string standardInput, TimeSpan timeout, CancellationToken cancellationToken = default)
    {
        var build = WorkspaceFiles.Resolve(workspace, "work/.run-csharp-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(build);
        using var lifetime = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        lifetime.CancelAfter(timeout);
        try
        {
            // Put explicitly supplied dependencies beside the executable so runtime resolution matches compilation.
            var names = new HashSet<string>(StringComparer.OrdinalIgnoreCase) { "analysis.dll" };
            var localReferences = new List<string>();
            foreach (var reference in references)
            {
                var name = Path.GetFileName(reference);
                if (!names.Add(name)) throw new InvalidDataException("C# 引用 DLL 文件名重复或与 analysis.dll 冲突：" + name);
                var target = WorkspaceFiles.Resolve(build, name);
                await GeneratedAdapterResources.CopyAsync(reference, target, lifetime.Token);
                localReferences.Add(target);
            }
            // Redirected console handles still use the Windows code page unless the child selects UTF-8.
            var consoleSetup = Path.Combine(build, "HostConsole.cs");
            await File.WriteAllTextAsync(consoleSetup, """
                namespace AvgTranslator.AnalysisHost
                {
                    internal static class ConsoleSetup
                    {
                        [System.Runtime.CompilerServices.ModuleInitializer]
                        internal static void Initialize()
                        {
                            System.Console.InputEncoding = new System.Text.UTF8Encoding(false);
                            System.Console.OutputEncoding = new System.Text.UTF8Encoding(false);
                        }
                    }
                }
                """, new UTF8Encoding(false), lifetime.Token);
            var compilation = await CompileSourcesAsync([.. sources, consoleSetup], build, workingDirectory, lifetime.Token, localReferences, "analysis");
            if (compilation.ExitCode != 0) return (false, compilation);
            var result = await Runner.RunAsync(new(Toolchain.ExecutablePath,
                new[] { Path.Combine(build, "analysis.dll") }.Concat(arguments).ToArray(), workingDirectory,
                standardInput, timeout, 1024 * 1024), lifetime.Token);
            return (true, result);
        }
        catch (OperationCanceledException) when (!cancellationToken.IsCancellationRequested)
        { throw new AdapterProcessLimitException("C# 编译或运行超时，进程已停止。"); }
        finally { GeneratedGameAdapter.DeleteStagedTree(build); }
    }

    private async Task<AdapterProcessResult> CompileSourcesAsync(IReadOnlyList<string> sources, string build, string workingDirectory,
        CancellationToken cancellationToken, IReadOnlyList<string>? additionalReferences = null, string assemblyName = "adapter")
    {
        if (!VersionPattern().IsMatch(Toolchain.SdkVersion) || !VersionPattern().IsMatch(Toolchain.RuntimeVersion)
            || !Toolchain.RuntimeVersion.StartsWith("10.", StringComparison.Ordinal))
            throw new AdapterProcessUnavailableException("A pinned .NET 10 SDK and runtime are required for generated adapters.");
        var root = Path.GetFullPath(Toolchain.RootDirectory);
        var compiler = Path.Combine(root, "sdk", Toolchain.SdkVersion, "Roslyn", "bincore", "csc.dll");
        var referencePath = Path.Combine(root, "packs", "Microsoft.NETCore.App.Ref", Toolchain.RuntimeVersion, "ref", "net10.0");
        if (!File.Exists(Toolchain.ExecutablePath) || !File.Exists(compiler) || !Directory.Exists(referencePath))
            throw new AdapterProcessUnavailableException("缺少 .NET 编译器或引用程序集，请使用包含 toolchain 的完整发行包。");
        foreach (var path in new[] { root, compiler, referencePath }) WorkspaceFiles.RejectLinkAncestors(path);
        var references = Directory.GetFiles(referencePath, "*.dll").Order(StringComparer.Ordinal).ToArray();
        if (references.Length == 0) throw new AdapterProcessUnavailableException("The pinned framework reference pack is empty.");
        var arguments = new List<string>
        {
            "/nologo", "/nostdlib+", "/target:exe", "/langversion:latest", "/nullable:enable", "/codepage:65001", "/utf8output",
            // Embedded symbols give failed probes source lines without keeping separate PDB files.
            "/debug:embedded", assemblyName == "analysis" ? "/optimize-" : "/optimize+",
            "/deterministic+", "/unsafe-", "/out:\"" + Path.Combine(build, assemblyName + ".dll") + "\"",
            "/pathmap:\"" + build + "=/adapter\""
        };
        arguments.AddRange(references.Select(path => "/reference:\"" + path + "\""));
        if (additionalReferences is not null) arguments.AddRange(additionalReferences.Select(path => "/reference:\"" + path + "\""));
        arguments.AddRange(sources.Select(path => "\"" + path + "\""));
        var responseFile = Path.Combine(build, "compiler.rsp");
        await File.WriteAllLinesAsync(responseFile, arguments, new UTF8Encoding(false), cancellationToken);
        AdapterProcessResult result;
        try
        {
            result = await Runner.RunAsync(new AdapterProcessRequest(Toolchain.ExecutablePath,
                [compiler, "/noconfig", "@" + responseFile], workingDirectory, Timeout: TimeSpan.FromMinutes(2)), cancellationToken);
            if (result.ExitCode != 0) return result;
            if (!File.Exists(Path.Combine(build, assemblyName + ".dll"))) throw new GeneratedAdapterException("The compiler produced no executable assembly.");
        }
        finally { File.Delete(responseFile); }
        await File.WriteAllTextAsync(Path.Combine(build, assemblyName + ".runtimeconfig.json"), JsonSerializer.Serialize(new
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
    internal static string LimitDiagnostic(string value) => value.Length > 8_192 ? value[..8_192] + "\n[diagnostic truncated]" : value;

    [GeneratedRegex("^[0-9]+\\.[0-9]+\\.[0-9]+(?:-[A-Za-z0-9.-]+)?$")]
    private static partial Regex VersionPattern();
}
