using System.ComponentModel;
using System.Diagnostics;
using System.Text;

namespace GameL10n.Core.Development;

public sealed record AdapterProcessRequest(string ExecutablePath, IReadOnlyList<string> Arguments,
    string WorkspacePath, string StandardInput = "", TimeSpan? Timeout = null, long? MaximumOutputBytes = 1024 * 1024,
    string? VnTextPatchAssemblyPath = null, IReadOnlyList<string>? AdapterHostCommand = null);

public sealed record AdapterProcessResult(int ExitCode, string StandardOutput, string StandardError);

public interface IAdapterProcessRunner
{
    Task<AdapterProcessResult> RunAsync(AdapterProcessRequest request, CancellationToken cancellationToken);
}

public sealed class AdapterProcessUnavailableException(string message, Exception? inner = null) : Exception(message, inner);
public sealed class AdapterProcessLimitException(string message, string? phase = null, bool timedOut = false,
    string standardOutput = "", string standardError = "", Exception? inner = null) : Exception(message, inner)
{
    public string? Phase { get; } = phase;
    public bool TimedOut { get; } = timedOut;
    public string StandardOutput { get; } = LimitOutput(standardOutput);
    public string StandardError { get; } = LimitOutput(standardError);

    internal static string LimitOutput(string value)
    {
        const string suffix = "\n[diagnostic truncated]";
        return value.Length > 8192 ? value[..(8192 - suffix.Length)] + suffix : value;
    }
}

// A caller may own the overall deadline; preserve collected output without turning cancellation into a limit failure here.
internal sealed class AdapterProcessCanceledException(Exception inner, CancellationToken cancellationToken,
    string standardOutput, string standardError) : OperationCanceledException(inner.Message, inner, cancellationToken)
{
    public string StandardOutput { get; } = AdapterProcessLimitException.LimitOutput(standardOutput);
    public string StandardError { get; } = AdapterProcessLimitException.LimitOutput(standardError);
}

/// <summary>Runs local tools with bounded time and output. Uses the current user's permissions.</summary>
public sealed class AdapterProcessRunner : IAdapterProcessRunner
{
    public async Task<AdapterProcessResult> RunAsync(AdapterProcessRequest request, CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();
        if (!File.Exists(request.ExecutablePath) || !Directory.Exists(request.WorkspacePath))
            throw new AdapterProcessUnavailableException("适配工具或临时工作目录不存在。");
        if (request.MaximumOutputBytes is < 1 || request.Timeout is { } duration && duration != Timeout.InfiniteTimeSpan && duration <= TimeSpan.Zero)
            throw new ArgumentOutOfRangeException(nameof(request));
        var start = new ProcessStartInfo(Path.GetFullPath(request.ExecutablePath))
        {
            WorkingDirectory = Path.GetFullPath(request.WorkspacePath), UseShellExecute = false, CreateNoWindow = true,
            RedirectStandardInput = true, RedirectStandardOutput = true, RedirectStandardError = true,
            StandardInputEncoding = new UTF8Encoding(false), StandardOutputEncoding = Encoding.UTF8, StandardErrorEncoding = Encoding.UTF8
        };
        foreach (var argument in request.Arguments) start.ArgumentList.Add(argument);
        // Do not inherit API credentials or arbitrary runtime overrides from the host environment.
        start.Environment.Clear();
        start.Environment["SystemRoot"] = start.Environment["WINDIR"] = Environment.GetFolderPath(Environment.SpecialFolder.Windows);
        start.Environment["TEMP"] = start.Environment["TMP"] = start.WorkingDirectory;
        start.Environment["USERPROFILE"] = start.Environment["DOTNET_CLI_HOME"] = start.WorkingDirectory;
        start.Environment["DOTNET_ROOT"] = Path.GetDirectoryName(start.FileName);
        start.Environment["DOTNET_MULTILEVEL_LOOKUP"] = "0";
        start.Environment["DOTNET_EnableDiagnostics"] = "0";
        start.Environment["DOTNET_CLI_TELEMETRY_OPTOUT"] = "1";
        start.Environment["DOTNET_SKIP_FIRST_TIME_EXPERIENCE"] = "1";
        if (request.AdapterHostCommand is { } hostCommand)
            start.Environment["GAMEL10N_ADAPTER_HOST"] = System.Text.Json.JsonSerializer.Serialize(hostCommand);
        if (request.VnTextPatchAssemblyPath is { } parserPath)
        {
            start.Environment["GAMEL10N_VNTEXTPATCH"] = Path.GetFullPath(parserPath);
            // Existing published adapters still read this protocol variable.
            start.Environment["AVG_TRANSLATOR_VNTEXTPATCH"] = Path.GetFullPath(parserPath);
        }
        using var process = new Process { StartInfo = start };
        try
        {
            if (!process.Start()) throw new AdapterProcessUnavailableException("本地适配工具未能启动。");
        }
        catch (Win32Exception error)
        { throw new AdapterProcessUnavailableException($"本地适配工具未能启动（Windows {error.NativeErrorCode}）：{error.Message}", error); }
        using var timeout = new CancellationTokenSource(request.Timeout ?? TimeSpan.FromSeconds(30));
        using var lifetime = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken, timeout.Token);
        using var registration = lifetime.Token.Register(StopProcess);
        var standardOutput = new StringBuilder();
        var standardError = new StringBuilder();
        AdapterProcessLimitException? outputLimit = null;
        try
        {
            var output = ReadAsync(process.StandardOutput, standardOutput);
            var error = ReadAsync(process.StandardError, standardError);
            await Task.WhenAll(output, error, WriteInputAsync(), process.WaitForExitAsync(lifetime.Token));
            if (request.MaximumOutputBytes is { } maximum && (long)Encoding.UTF8.GetByteCount(output.Result) + Encoding.UTF8.GetByteCount(error.Result) > maximum)
                throw new AdapterProcessLimitException("适配工具输出超过大小限制。");
            return new(process.ExitCode, output.Result, error.Result);
        }
        catch (Exception error) when (cancellationToken.IsCancellationRequested)
        {
            throw new AdapterProcessCanceledException(error, cancellationToken, standardOutput.ToString(), standardError.ToString());
        }
        catch (Exception error) when (outputLimit is not null)
        {
            throw new AdapterProcessLimitException(outputLimit.Message, standardOutput: standardOutput.ToString(),
                standardError: standardError.ToString(), inner: error);
        }
        catch (Exception error) when (timeout.IsCancellationRequested)
        {
            throw new AdapterProcessLimitException("适配工具运行超时，进程已停止。", timedOut: true,
                standardOutput: standardOutput.ToString(), standardError: standardError.ToString(), inner: error);
        }
        catch (AdapterProcessLimitException error)
        {
            throw new AdapterProcessLimitException(error.Message, standardOutput: standardOutput.ToString(),
                standardError: standardError.ToString(), inner: error);
        }
        finally
        {
            StopProcess();
            try { await process.WaitForExitAsync().WaitAsync(TimeSpan.FromSeconds(5)); }
            catch (Exception error) when (error is TimeoutException or InvalidOperationException) { Debug.WriteLine(error); }
        }

        void StopProcess()
        {
            try { if (!process.HasExited) process.Kill(entireProcessTree: true); }
            catch (Exception error) when (error is InvalidOperationException or Win32Exception) { Debug.WriteLine(error); }
        }

        async Task WriteInputAsync()
        {
            try
            {
                if (request.StandardInput.Length > 0)
                    await process.StandardInput.WriteAsync(request.StandardInput.AsMemory(), lifetime.Token);
                process.StandardInput.Close();
            }
            catch { lifetime.Cancel(); throw; }
        }

        async Task<string> ReadAsync(StreamReader reader, StringBuilder text)
        {
            try
            {
                var buffer = new char[4096];
                long bytes = 0;
                int count;
                while ((count = await reader.ReadAsync(buffer, lifetime.Token)) != 0)
                {
                    bytes += Encoding.UTF8.GetByteCount(buffer.AsSpan(0, count));
                    if (request.MaximumOutputBytes is { } maximum && bytes > maximum)
                        throw outputLimit = new AdapterProcessLimitException("适配工具输出超过大小限制。");
                    text.Append(buffer, 0, count);
                }
                return text.ToString();
            }
            catch { lifetime.Cancel(); throw; }
        }
    }
}
