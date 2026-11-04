using System.Text.Json;

namespace GameL10n.Core.Development;

/// <summary>Per-request diagnostics without retaining successful payloads, source or game text.</summary>
internal sealed class EngineReviewTrace(string projectDirectory)
{
    private readonly string runId = Guid.NewGuid().ToString("N");
    private static readonly JsonSerializerOptions Json = new(JsonSerializerDefaults.Web);

    public async Task WriteAsync(object entry)
    {
        try
        {
            var directory = Path.Combine(projectDirectory, "logs");
            Directory.CreateDirectory(directory);
            await File.AppendAllTextAsync(Path.Combine(directory, $"engine-review-{DateTime.Now:yyyyMMdd}.jsonl"),
                JsonSerializer.Serialize(new { schemaVersion = 1, runId, at = DateTimeOffset.UtcNow,
                    coreBuild = typeof(EngineReviewService).Module.ModuleVersionId.ToString("N"), entry },
                    Json) + "\n");
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            // Optional diagnostics must not discard a completed review.
        }
    }
}
