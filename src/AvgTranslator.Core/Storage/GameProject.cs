using System.Text.Json.Serialization;
using AvgTranslator.Core.Development;
using AvgTranslator.Core.Pipeline;
using AvgTranslator.Core.Providers;
using AvgTranslator.Core.Isolation;

namespace AvgTranslator.Core.Storage;

public sealed record GameTranslationSettings
{
    public string TargetLanguage { get; init; } = "简体中文";
    public string Style { get; init; } = "自然流畅，保留人物语气、情绪和原文含义。";
    public string Terminology { get; init; } = "";
    public int BatchSize { get; init; } = 24;
    // Missing values follow the global defaults; persisted values remain project overrides.
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public long? AdaptationTokenBudget { get; init; }
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public int? AdaptationMaximumTurns { get; init; }
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? AnalysisProfileId { get; init; }
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public IReadOnlyList<string>? TranslationProfileIds { get; init; }

    public ApiProfileConfiguration ResolveApiConfiguration(ApiProfileConfiguration defaults) => defaults with
    {
        AnalysisProfileId = AnalysisProfileId ?? defaults.AnalysisProfileId,
        TranslationProfileIds = TranslationProfileIds ?? defaults.GetTranslationProfileIds(),
        TranslationProfileId = null
    };

    public AdaptationLimits ResolveAdaptationLimits(AdaptationLimits defaults) => defaults with
    {
        MaximumTokens = AdaptationTokenBudget ?? defaults.MaximumTokens,
        MaximumTurns = AdaptationMaximumTurns ?? defaults.MaximumTurns
    };
}

public sealed record GameAnalysisSummary(string AdapterId, string AdapterVersion, string SourceFingerprint,
    int UnitCount, int ResourceCount, string EngineId, DateTimeOffset CompletedAt);

public sealed record GameProjectTaskSummary(string Stage, bool Running, int Completed = 0, int Total = 0);

public sealed record GameProject
{
    public int SchemaVersion { get; init; } = 2;
    public string Id { get; init; } = "";
    public string GameDirectory { get; init; } = "";
    public string Name { get; init; } = "";
    public GameTranslationSettings Settings { get; init; } = new();
    public string? AdapterId { get; init; }
    public string? AdapterVersion { get; init; }
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public GameAnalysisSummary? Analysis { get; init; }
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public GameProjectTaskSummary? TaskSummary { get; init; }
    public bool IsRemoved { get; init; }
    public DateTimeOffset UpdatedAt { get; init; } = DateTimeOffset.UtcNow;
}

public sealed partial class GameProjectStore(string dataDirectory)
{
    public string GetProjectDirectory(string id)
    {
        if (id.Length != 24 || id.Any(c => !Uri.IsHexDigit(c))) throw new InvalidDataException("无效的游戏项目标识。");
        return LocalFiles.UnderRoot(Path.GetFullPath(dataDirectory), "games/" + id);
    }

    public GameProject Create(string gameDirectory, GameTranslationSettings? settings = null)
    {
        var path = Path.TrimEndingDirectorySeparator(Path.GetFullPath(gameDirectory));
        return new() { Id = TranslationPipeline.GameId(path), GameDirectory = path,
            Name = Path.GetFileName(path), Settings = settings ?? new() };
    }

    public async Task SaveAsync(GameProject project, CancellationToken ct = default)
    {
        var path = Path.Combine(GetProjectDirectory(project.Id), "game.json");
        await indexLock.WaitAsync(ct);
        try
        {
            var index = await ReadIndexAsync(ct, (_, _) => { });
            // Invalidate before committing metadata so cancellation or a failed index write cannot leave stale entries.
            File.Delete(IndexPath);
            await LocalFiles.WriteJsonAsync(path, project, ct);
            index.Projects.RemoveAll(item => item.Id == project.Id);
            if (!project.IsRemoved) index.Projects.Add(GameProjectSummary.FromProject(project));
            await WriteIndexAsync(index, ct);
        }
        finally { indexLock.Release(); }
    }

    public Task<GameProject?> GetAsync(string id, CancellationToken ct = default) =>
        LocalFiles.ReadRecoverableJsonAsync<GameProject>(Path.Combine(GetProjectDirectory(id), "game.json"), ct,
            project => project.SchemaVersion == 2 && project.Id == id && project.Settings is not null
                && !string.IsNullOrWhiteSpace(project.GameDirectory) && TranslationPipeline.GameId(project.GameDirectory) == id);

    public async Task RemoveAsync(string id, bool deleteData, CancellationToken ct = default)
    {
        var projectDirectory = GetProjectDirectory(id);
        if (!Directory.Exists(projectDirectory))
        {
            await indexLock.WaitAsync(ct);
            try
            {
                if (!Directory.Exists(projectDirectory))
                {
                    var index = await ReadIndexAsync(ct, (_, _) => { });
                    index.Projects.RemoveAll(item => item.Id == id);
                    await WriteIndexAsync(index, ct);
                    return;
                }
            }
            finally { indexLock.Release(); }
        }
        if (!deleteData)
        {
            var project = await GetAsync(id, ct) ?? throw new InvalidDataException("项目记录不存在。");
            await SaveAsync(project with { IsRemoved = true }, ct);
            return;
        }
        await indexLock.WaitAsync(ct);
        try { await DeleteProjectAsync(id, ct); }
        finally { indexLock.Release(); }
    }

    private async Task DeleteProjectAsync(string id, CancellationToken ct)
    {
        var project = await GetAsync(id, ct) ?? throw new InvalidDataException("项目记录不存在。");
        var directory = Path.TrimEndingDirectorySeparator(Path.GetFullPath(GetProjectDirectory(id)));
        var gameDirectory = Path.TrimEndingDirectorySeparator(Path.GetFullPath(project.GameDirectory));
        if (gameDirectory.Equals(directory, StringComparison.OrdinalIgnoreCase)
            || gameDirectory.StartsWith(directory + Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase))
            throw new IOException("项目数据目录包含游戏安装文件，不能删除。请改为仅移除项目并保留文件。");
        WorkspaceFiles.RejectLinkAncestors(directory);
        var index = await ReadIndexAsync(ct, (_, _) => { });
        ct.ThrowIfCancellationRequested();
        File.Delete(IndexPath);
        try { Directory.Delete(directory, recursive: true); }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            // Retain a visible project for retry if a locked file interrupts deletion after game.json was removed.
            try { await LocalFiles.WriteJsonAsync(Path.Combine(directory, "game.json"), project, CancellationToken.None); }
            catch (Exception restoreError) when (restoreError is IOException or UnauthorizedAccessException)
            { throw new IOException("项目文件未完全删除，且项目记录无法恢复：" + restoreError.Message, error); }
            throw new IOException("项目文件未完全删除，项目已保留，可关闭占用文件的程序后重试。" + error.Message, error);
        }
        index.Projects.RemoveAll(item => item.Id == id);
        await WriteIndexAsync(index, ct);
    }

    public async Task<IReadOnlyList<GameProject>> ListAsync(CancellationToken ct = default, Action<string, Exception>? onError = null)
    {
        var result = new List<GameProject>();
        foreach (var summary in await ListSummariesAsync(ct, onError))
        {
            try
            {
                var project = await GetAsync(summary.Id, ct);
                if (project is { IsRemoved: false }) result.Add(project);
            }
            catch (Exception error) when (IsMetadataReadError(error))
            {
                // A damaged game's metadata must not prevent other games from opening.
                if (onError is null) throw;
                onError(summary.Id, error);
            }
        }
        return result.OrderByDescending(project => project.UpdatedAt).ToArray();
    }
}
