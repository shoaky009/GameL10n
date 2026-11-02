using System.ComponentModel;
using System.Text;
using AvgTranslator.Core;
using AvgTranslator.Core.Development;
using AvgTranslator.Core.Pipeline;
using AvgTranslator.Core.Storage;
using AvgTranslator.Core.Translation;

namespace AvgTranslator.Desktop;

internal enum ProjectContentState { NotLoaded, Loading, Loaded, Unavailable }

internal sealed class GameWorkspace(GameProject project, string directory) : INotifyPropertyChanged
{
    public GameProject Project { get; set; } = project;
    public string Directory { get; } = directory;
    public string Id => Project.Id;
    public string Name => Project.Name;
    public string GameDirectory => Project.GameDirectory;
    public GameSession? Session { get; set; }
    public bool MetadataLoaded { get; set; }
    public Task? MetadataLoad { get; set; }
    public long TaskOrder { get; set; }
    public bool HasSavedAnalysis { get; set; }
    public ProjectContentState ContentState { get; set; }
    public bool ContentLoading => ContentState == ProjectContentState.Loading;
    public Task? ContentLoad { get; set; }
    public CancellationTokenSource? ContentLoadCancellation { get; set; }
    public string? ContentError { get; set; }
    public long CachedContentBytes { get; set; }
    public PipelineResult? Result { get; set; }
    public CancellationTokenSource? Cancellation { get; set; }
    public Task? RunningTask { get; set; }
    public string? RunId { get; set; }
    public GameTaskKind Kind { get; set; }
    public bool Busy => Cancellation is not null;
    public string Stage { get; set; } = project.TaskSummary is { } task ? task.Running ? "可继续" : task.Stage
        : project.Analysis is not null ? "已提取，可继续处理" : "打开后查看状态";
    public string StageMessage { get; set; } = "";
    public bool AcceptProgress { get; set; }
    public Action<string, string>? ReportProgress { get; set; }
    public bool IsBeingRemoved { get; set; }
    public string? Error { get; set; }
    public PipelineProgress? Progress { get; set; }
    public TokenUsage? AdaptationUsage { get; set; }
    public string? AdaptationSummary { get; set; }
    public EngineReviewResult? EngineReview { get; set; }
    public AdaptationProgressNote? AdaptationProgressNote { get; set; }
    public AdaptationProgressNote? AdaptationLastBreakthrough { get; set; }
    public bool HasAdaptationSession { get; set; }
    public AdaptationStatus? AdaptationStatus { get; set; }
    public string? AdaptationNextAction { get; set; }
    public IReadOnlyList<string> PendingTextCoverage { get; set; } = [];
    public IReadOnlyList<string> PendingRuntimeChecks { get; set; } = [];
    public AdaptationBlocker? AdaptationBlocker { get; set; }
    public string? AdaptationInstruction { get; set; }
    public AdaptationObjective? AdaptationObjective { get; set; }
    public List<BuiltinAdapterProbeObservation> BuiltinProbes { get; } = [];
    public DateTimeOffset LastViewed { get; set; }
    public Task DurableWrites { get; private set; } = Task.CompletedTask;
    public StringBuilder Log { get; } = new();
    public TranslationRow[]? Rows { get; set; }
    public Dictionary<string, TranslationRow> RowsById { get; set; } = new(StringComparer.Ordinal);
    public Dictionary<string, TranslationEdit> Edits { get; set; } = new(StringComparer.Ordinal);
    public GameSession? RowsSession { get; set; }
    public string? RowsPolicy { get; set; }
    public bool EditsDirty { get; set; }
    public string? EditSaveError { get; set; }
    public bool SelectionDirty { get; set; }
    public long SelectionRevision { get; set; }
    public long EditRevision { get; set; }
    public Task? LoadingRows { get; set; }
    public HashSet<string> SelectedTexts { get; } = new(StringComparer.Ordinal);
    public string? ActiveTextId { get; set; }
    public Dictionary<string, string>? PatchTextHashes { get; set; }
    public string? PatchIndexDirectory { get; set; }
    public string? RuntimeCheck { get; set; }
    public Dictionary<string, TranslationEdit> Candidates { get; set; } = new(StringComparer.Ordinal);
    public int ProjectTab { get; set; }
    public bool HasPendingTranslation { get; set; }
    public bool Retranslating { get; set; }
    public string Status => Progress is { Total: > 0 } progress && (!Busy || Kind == GameTaskKind.Translation)
        ? $"{Stage} · {progress.Completed:N0} / {progress.Total:N0}" + (progress.Failed > 0 ? $" · 失败 {progress.Failed:N0}" : "")
        : !MetadataLoaded && Project.TaskSummary is { Total: > 0 } saved
            ? $"{Stage} · {saved.Completed:N0} / {saved.Total:N0}" : Stage;
    public string PathLabel => GameDirectory;

    public void Notify() => PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(Status)));
    public event PropertyChangedEventHandler? PropertyChanged;

    // One ordered writer per game; a failed diagnostic write does not poison later state saves.
    public Task EnqueueWrite(Func<Task> write)
    {
        var previous = DurableWrites;
        return DurableWrites = ContinueAsync();
        async Task ContinueAsync()
        {
            try { await previous.ConfigureAwait(false); }
            catch (Exception error) { System.Diagnostics.Debug.WriteLine(error); }
            await write().ConfigureAwait(false);
        }
    }
}
