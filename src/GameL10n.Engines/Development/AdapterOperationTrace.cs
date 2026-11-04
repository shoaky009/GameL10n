using System.Diagnostics;

namespace GameL10n.Core.Development;

/// <summary>Times adapter work through the caller's existing diagnostic sink.</summary>
public sealed class AdapterOperationTrace(Func<object, Task>? write, string operation,
    string? adapterId = null, string? version = null)
{
    private const long LargeFileBytes = 64L * 1024 * 1024;

    public async Task<T> MeasureAsync<T>(string phase, Func<Task<T>> action, object? detail = null, bool record = true)
    {
        if (write is null || !record) return await action();
        await WriteAsync(new { operation, adapterId, version, phase, status = "started", detail });
        var watch = Stopwatch.StartNew();
        Exception? failure = null;
        try { return await action(); }
        catch (Exception error) { failure = error; throw; }
        finally
        {
            await WriteAsync(new { operation, adapterId, version, phase,
                status = failure is OperationCanceledException ? "cancelled" : failure is null ? "completed" : "failed",
                elapsedMs = watch.ElapsedMilliseconds, errorType = failure?.GetType().Name,
                error = failure?.Message[..Math.Min(failure.Message.Length, 1500)], detail });
        }
    }

    public Task MeasureAsync(string phase, Func<Task> action, object? detail = null) =>
        MeasureAsync(phase, async () => { await action(); return true; }, detail);

    public Task<T> FileAsync<T>(string phase, string relativePath, string path, Func<Task<T>> action)
    {
        long bytes = 0;
        if (write is not null)
        {
            try { bytes = new FileInfo(path).Length; }
            catch (Exception error) when (error is IOException or UnauthorizedAccessException) { }
        }
        return MeasureAsync(phase, action, new { path = relativePath, bytes }, record: bytes >= LargeFileBytes);
    }

    public Task FileAsync(string phase, string relativePath, string path, Func<Task> action) =>
        FileAsync(phase, relativePath, path, async () => { await action(); return true; });

    private async Task WriteAsync(object detail)
    {
        try { await write!(detail); }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        { Debug.WriteLine(error); }
    }
}
