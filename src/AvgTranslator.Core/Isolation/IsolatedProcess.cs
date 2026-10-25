namespace AvgTranslator.Core.Isolation;

/// <summary>Only staged files belong in WorkspacePath. Never pass a game directory or credentials.</summary>
public sealed record IsolatedProcessRequest(
    string ExecutablePath,
    IReadOnlyList<string> Arguments,
    string WorkspacePath,
    IReadOnlyList<string> ReadOnlyDirectories,
    string StandardInput = "",
    TimeSpan? Timeout = null,
    long MemoryLimitBytes = 768L * 1024 * 1024,
    int MaximumOutputBytes = 1024 * 1024);

public sealed record IsolatedProcessResult(int ExitCode, string StandardOutput, string StandardError);

public interface IIsolatedProcessRunner
{
    Task<IsolatedProcessResult> RunAsync(IsolatedProcessRequest request, CancellationToken cancellationToken);
}

/// <summary>A sandbox could not be established. Callers must never retry without isolation.</summary>
public sealed class IsolationUnavailableException(string message, Exception? inner = null) : Exception(message, inner);

public sealed class IsolatedProcessLimitException(string message) : Exception(message);
