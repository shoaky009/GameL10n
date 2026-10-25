using System.Collections.Concurrent;
using System.Text.Json;
using System.Text.Json.Serialization;
using AvgTranslator.Core.Pipeline;

namespace AvgTranslator.Core.Storage;

/// <summary>List and account-reference metadata. Full settings are loaded from game.json when opening a project.</summary>
public sealed record GameProjectSummary
{
    public required string Id { get; init; }
    public required string GameDirectory { get; init; }
    public required string Name { get; init; }
    public DateTimeOffset UpdatedAt { get; init; }
    public string? AdapterId { get; init; }
    public string? AdapterVersion { get; init; }
    public GameAnalysisSummary? Analysis { get; init; }
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public GameProjectTaskSummary? TaskSummary { get; init; }
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? AnalysisProfileId { get; init; }
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public IReadOnlyList<string>? TranslationProfileIds { get; init; }

    /// <summary>A display placeholder; load the complete project before editing settings or starting work.</summary>
    public GameProject ToProject() => new()
    {
        Id = Id, GameDirectory = GameDirectory, Name = Name, UpdatedAt = UpdatedAt,
        AdapterId = AdapterId, AdapterVersion = AdapterVersion, Analysis = Analysis, TaskSummary = TaskSummary,
        Settings = new() { AnalysisProfileId = AnalysisProfileId, TranslationProfileIds = TranslationProfileIds }
    };

    internal static GameProjectSummary FromProject(GameProject project) => new()
    {
        Id = project.Id, GameDirectory = project.GameDirectory, Name = project.Name, UpdatedAt = project.UpdatedAt,
        AdapterId = project.AdapterId, AdapterVersion = project.AdapterVersion, Analysis = project.Analysis, TaskSummary = project.TaskSummary,
        AnalysisProfileId = project.Settings.AnalysisProfileId, TranslationProfileIds = project.Settings.TranslationProfileIds
    };
}

public sealed partial class GameProjectStore
{
    private static readonly ConcurrentDictionary<string, SemaphoreSlim> IndexLocks = new(StringComparer.OrdinalIgnoreCase);
    private readonly SemaphoreSlim indexLock = IndexLocks.GetOrAdd(
        Path.TrimEndingDirectorySeparator(Path.GetFullPath(dataDirectory)), _ => new(1, 1));
    private string IndexPath => LocalFiles.UnderRoot(Path.GetFullPath(dataDirectory), "games/index.json");

    /// <summary>Reads one lightweight index. Only a missing or damaged index enumerates per-project game.json metadata.</summary>
    public async Task<IReadOnlyList<GameProjectSummary>> ListSummariesAsync(CancellationToken ct = default,
        Action<string, Exception>? onError = null)
    {
        await indexLock.WaitAsync(ct);
        try
        {
            var index = await ReadIndexAsync(ct, onError);
            return index.Projects.OrderByDescending(project => project.UpdatedAt).ToArray();
        }
        finally { indexLock.Release(); }
    }

    private async Task<ProjectIndex> ReadIndexAsync(CancellationToken ct, Action<string, Exception>? onError)
    {
        try
        {
            if (await LocalFiles.ReadJsonAsync<ProjectIndex>(IndexPath, ct) is { } index)
            {
                if (index.SchemaVersion != 1 || index.Projects is null
                    || index.Projects.Any(project => project is null || project.Name is null
                        || string.IsNullOrWhiteSpace(project.Id) || project.Id.Length != 24 || !project.Id.All(Uri.IsHexDigit)
                        || string.IsNullOrWhiteSpace(project.GameDirectory) || TranslationPipeline.GameId(project.GameDirectory) != project.Id)
                    || index.Projects.Select(project => project.Id).Distinct(StringComparer.Ordinal).Count() != index.Projects.Count)
                    throw new InvalidDataException("游戏项目索引格式无效。");
                return index;
            }
        }
        catch (Exception error) when (error is JsonException or InvalidDataException or ArgumentException)
        { /* The index contains derived metadata and can be rebuilt without opening tasks or extraction payloads. */ }

        var rebuilt = new ProjectIndex();
        var directory = Path.GetDirectoryName(IndexPath)!;
        if (Directory.Exists(directory))
            foreach (var path in Directory.EnumerateDirectories(directory))
            {
                ct.ThrowIfCancellationRequested();
                var id = Path.GetFileName(path);
                if (id.Length != 24 || !id.All(Uri.IsHexDigit)) continue;
                try
                {
                    if (await GetAsync(id, ct) is { IsRemoved: false } project)
                        rebuilt.Projects.Add(GameProjectSummary.FromProject(project));
                }
                catch (Exception error) when (IsMetadataReadError(error))
                {
                    if (onError is null) throw;
                    onError(id, error);
                }
            }
        await WriteIndexAsync(rebuilt, ct);
        return rebuilt;
    }

    private Task WriteIndexAsync(ProjectIndex index, CancellationToken ct) => LocalFiles.WriteJsonAsync(IndexPath, index, ct);

    private static bool IsMetadataReadError(Exception error) =>
        error is IOException or InvalidDataException or JsonException or UnauthorizedAccessException or ArgumentException;

    private sealed class ProjectIndex
    {
        [JsonRequired] public int SchemaVersion { get; init; } = 1;
        [JsonRequired] public List<GameProjectSummary> Projects { get; init; } = [];
    }
}
