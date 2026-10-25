using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Text.RegularExpressions;
using AvgTranslator.Core.Isolation;
using AvgTranslator.Core.Adapters;

namespace AvgTranslator.Core.Development;

/// <summary>Compiles a single source file; generated project files, packages, build tasks and analyzers are never accepted.</summary>
public sealed partial class GeneratedAdapterCompiler(PinnedDotNetToolchain toolchain, IIsolatedProcessRunner runner)
{
    public PinnedDotNetToolchain Toolchain { get; } = toolchain;
    public IIsolatedProcessRunner Runner { get; } = runner;

    public async Task CheckAvailabilityAsync(string workspace, CancellationToken cancellationToken = default)
    {
        if (!File.Exists(Toolchain.ExecutablePath))
            throw new IsolationUnavailableException("The pinned .NET toolchain is unavailable; no model request was sent.");
        WindowsAppContainerProcessRunner.CreateLowIntegrityDirectory(workspace);
        var result = await Runner.RunAsync(new IsolatedProcessRequest(Toolchain.ExecutablePath,
            ["--info"], workspace, [Path.GetFullPath(Toolchain.RootDirectory)], Timeout: TimeSpan.FromSeconds(20)), cancellationToken);
        if (result.ExitCode != 0)
            throw new IsolationUnavailableException("The isolated .NET toolchain could not start; no model request was sent.");
    }

    public async Task<GeneratedAdapterPackage> CompileAsync(string sourceCode, string workspace,
        IReadOnlyList<string> resourceFiles, CancellationToken cancellationToken = default, AdapterFamily? family = null)
    {
        family?.Validate();
        if (!VersionPattern().IsMatch(Toolchain.SdkVersion) || !VersionPattern().IsMatch(Toolchain.RuntimeVersion)
            || !Toolchain.RuntimeVersion.StartsWith("10.", StringComparison.Ordinal))
            throw new IsolationUnavailableException("A pinned .NET 10 SDK and runtime are required for generated adapters.");
        var root = Path.GetFullPath(Toolchain.RootDirectory);
        var compiler = Path.Combine(root, "sdk", Toolchain.SdkVersion, "Roslyn", "bincore", "csc.dll");
        var referencePath = Path.Combine(root, "packs", "Microsoft.NETCore.App.Ref", Toolchain.RuntimeVersion, "ref", "net10.0");
        if (!File.Exists(Toolchain.ExecutablePath) || !File.Exists(compiler) || !Directory.Exists(referencePath))
            throw new IsolationUnavailableException("The pinned .NET toolchain is unavailable; automatic adaptation cannot compile safely.");
        foreach (var path in new[] { root, compiler, referencePath }) WorkspaceFiles.RejectReparsePoints(path);
        WindowsAppContainerProcessRunner.CreateLowIntegrityDirectory(workspace);
        var build = WorkspaceFiles.Resolve(workspace, "build");
        Directory.CreateDirectory(build);
        await File.WriteAllTextAsync(Path.Combine(build, "Program.cs"), sourceCode, new UTF8Encoding(false), cancellationToken);
        var references = Directory.GetFiles(referencePath, "*.dll").Order(StringComparer.Ordinal).ToArray();
        if (references.Length == 0) throw new IsolationUnavailableException("The pinned framework reference pack is empty.");
        var arguments = new List<string>
        {
            "/noconfig", "/nologo", "/nostdlib+", "/target:exe", "/langversion:latest", "/nullable:enable",
            "/optimize+", "/deterministic+", "/unsafe-", "/out:\"" + Path.Combine(build, "adapter.dll") + "\""
        };
        arguments.AddRange(references.Select(path => "/reference:\"" + path + "\""));
        arguments.Add("\"" + Path.Combine(build, "Program.cs") + "\"");
        var responseFile = Path.Combine(build, "compiler.rsp");
        await File.WriteAllLinesAsync(responseFile, arguments, new UTF8Encoding(false), cancellationToken);
        var result = await Runner.RunAsync(new IsolatedProcessRequest(Toolchain.ExecutablePath,
            [compiler, "/noconfig", "@" + responseFile], workspace, [root], Timeout: TimeSpan.FromSeconds(45)), cancellationToken);
        if (result.ExitCode != 0)
            throw new GeneratedAdapterException("Compilation failed: " + LimitDiagnostic(result.StandardOutput + "\n" + result.StandardError));
        var assembly = WorkspaceFiles.ReadBounded(build, "adapter.dll", 8 * 1024 * 1024);
        await File.WriteAllTextAsync(Path.Combine(build, "adapter.runtimeconfig.json"), JsonSerializer.Serialize(new
        {
            runtimeOptions = new { tfm = "net10.0", rollForward = "Disable", framework = new { name = "Microsoft.NETCore.App", version = Toolchain.RuntimeVersion } }
        }), cancellationToken);
        var sourceHash = Convert.ToHexStringLower(SHA256.HashData(Encoding.UTF8.GetBytes(sourceCode)));
        var versionHash = Convert.ToHexStringLower(SHA256.HashData(Encoding.UTF8.GetBytes(sourceHash + "\n" + Toolchain.RuntimeVersion + "\n"
            + string.Join('\n', resourceFiles.Order(StringComparer.Ordinal)))));
        return new GeneratedAdapterPackage(family?.AdapterId ?? "unpublished", versionHash[..16], build,
            resourceFiles.ToArray(), sourceHash, Convert.ToHexStringLower(SHA256.HashData(assembly)), family);
    }

    internal static string LimitDiagnostic(string value) => value.Length > 8_192 ? value[..8_192] + "\n[diagnostic truncated]" : value;

    [GeneratedRegex("^[0-9]+\\.[0-9]+\\.[0-9]+(?:-[A-Za-z0-9.-]+)?$")]
    private static partial Regex VersionPattern();
}
