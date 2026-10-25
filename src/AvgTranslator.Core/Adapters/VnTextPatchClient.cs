using System.Text.Json;
using AvgTranslator.Core.Development;
using AvgTranslator.Core.Isolation;

namespace AvgTranslator.Core.Adapters;

public sealed record VnTextPatchDiagnostic(string ErrorType, string Message, string? ResourcePath = null,
    long? Offset = null, string? StackTrace = null, VnTextPatchDiagnostic? Inner = null);

public sealed class VnTextPatchException(string message, VnTextPatchDiagnostic? diagnostic = null) : Exception(message)
{
    public VnTextPatchDiagnostic? Diagnostic { get; } = diagnostic;
}

internal sealed class VnTextPatchClient(IAdapterProcessRunner runner, string? workerExecutablePath, string? workerAssemblyPath)
{
    private static readonly JsonSerializerOptions Json = new(JsonSerializerDefaults.Web) { MaxDepth = 64 };
    private readonly string executable = Path.GetFullPath(workerExecutablePath
        ?? Path.Combine(AppContext.BaseDirectory, "tools", "vntextpatch", "AvgTranslator.VnTextPatch.exe"));
    private readonly IReadOnlyList<string> arguments = workerAssemblyPath is null ? [] : [Path.GetFullPath(workerAssemblyPath)];

    internal async Task<Response> InvokeAsync(string operation, string format, string sourceDirectory,
        IReadOnlyList<string> resources, string? outputDirectory, IReadOnlyDictionary<string, string>? translations,
        CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();
        var source = Path.GetFullPath(sourceDirectory);
        var output = outputDirectory is null ? null : Path.GetFullPath(outputDirectory);
        if (output is not null) RequireSeparateDirectories(source, output);
        var workspace = Path.Combine(Path.GetTempPath(), "avg-translator-vntextpatch-" + Guid.NewGuid().ToString("N"));
        RequireSeparateDirectories(source, workspace);
        WorkspaceFiles.RejectLinkAncestors(workspace);
        Directory.CreateDirectory(workspace);
        try
        {
            var responsePath = Path.Combine(workspace, "response.json");
            string? translationsPath = null;
            if (translations is not null)
            {
                translationsPath = Path.Combine(workspace, "translations.json");
                await using var stream = new FileStream(translationsPath, FileMode.CreateNew, FileAccess.Write, FileShare.None);
                await JsonSerializer.SerializeAsync(stream, translations, Json, cancellationToken);
            }
            var request = JsonSerializer.Serialize(new
            {
                protocolVersion = 1, operation, format, sourceDirectory = source, outputDirectory = output,
                resourceFiles = resources, responseFile = responsePath, translationsFile = translationsPath
            }, Json);
            var result = await runner.RunAsync(new AdapterProcessRequest(executable, arguments, workspace, request,
                Timeout: operation == "probe" ? TimeSpan.FromMinutes(2) : TimeSpan.FromMinutes(10),
                MaximumOutputBytes: 1024 * 1024), cancellationToken);
            if (!File.Exists(responsePath))
                throw new VnTextPatchException("内置脚本工具未返回处理结果。",
                    new("WorkerProcessError", Diagnostic(result.StandardError)));
            WorkspaceFiles.RejectReparsePoints(responsePath);
            Response response;
            try
            {
                await using var stream = new FileStream(responsePath, FileMode.Open, FileAccess.Read, FileShare.Read,
                    128 * 1024, FileOptions.Asynchronous | FileOptions.SequentialScan);
                response = await JsonSerializer.DeserializeAsync<Response>(stream, Json, cancellationToken)
                    ?? throw new JsonException("Empty response.");
            }
            catch (JsonException error)
            {
                throw new VnTextPatchException("内置脚本工具返回了无效结果。",
                    new(error.GetType().FullName!, Diagnostic(error.Message)));
            }
            if (response.ProtocolVersion != 1 || !response.Success || result.ExitCode != 0)
                throw new VnTextPatchException("内置脚本处理失败：" + Diagnostic(response.Error ?? "工具未成功完成操作。"),
                    response.Diagnostic ?? new("WorkerProcessError", Diagnostic(result.StandardError)));
            return response;
        }
        finally
        {
            // Only this invocation's temporary IPC directory is removed; game and patch paths are never cleanup targets.
            GeneratedGameAdapter.DeleteStagedTree(workspace);
        }
    }

    internal static string Diagnostic(string message) => message.Length <= 2000 ? message : message[..2000] + "…";

    private static void RequireSeparateDirectories(string source, string destination)
    {
        var comparison = OperatingSystem.IsWindows() ? StringComparison.OrdinalIgnoreCase : StringComparison.Ordinal;
        var left = Path.GetFullPath(source).TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar);
        var right = Path.GetFullPath(destination).TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar);
        if (left.Equals(right, comparison) || right.StartsWith(left + Path.DirectorySeparatorChar, comparison)
            || left.StartsWith(right + Path.DirectorySeparatorChar, comparison))
            throw new InvalidDataException("原始资源、补丁和临时目录必须相互独立，不能在原始资源目录中写入文件。");
    }

    internal sealed record Response(int ProtocolVersion, bool Success, string? Error,
        GameProbe? Probe, ExtractionResult? Extraction, AdapterBuildResult? Build, ValidationResult? Validation,
        VnTextPatchDiagnostic? Diagnostic = null);
}
