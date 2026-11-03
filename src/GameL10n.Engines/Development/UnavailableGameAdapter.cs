namespace GameL10n.Core.Development;

/// <summary>Keeps saved text usable after an external adapter is removed, without providing resource operations.</summary>
public sealed class UnavailableGameAdapter(string id, string version) : IGameAdapter
{
    public string Id { get; } = id;
    public string Version { get; } = version;
    public string Message => $"引擎适配器版本已删除或文件丢失：{Id} · {Version}。已有文本和译文仍可翻译、校对，已生成的补丁仍可安装；请重新识别或智能适配后再生成新补丁。";

    public Task<GameProbe?> ProbeAsync(string gameDirectory, CancellationToken cancellationToken = default) =>
        Task.FromException<GameProbe?>(new GeneratedAdapterException(Message));

    public Task<ExtractionResult> ExtractAsync(string sourceDirectory, CancellationToken cancellationToken = default) =>
        Task.FromException<ExtractionResult>(new GeneratedAdapterException(Message));

    public Task<AdapterBuildResult> BuildPatchAsync(string sourceDirectory,
        IReadOnlyDictionary<string, string> translations, string patchDirectory,
        CancellationToken cancellationToken = default) =>
        Task.FromException<AdapterBuildResult>(new GeneratedAdapterException(Message));

    public Task<ValidationResult> ValidatePatchAsync(string sourceDirectory, string patchDirectory,
        CancellationToken cancellationToken = default) =>
        Task.FromException<ValidationResult>(new GeneratedAdapterException(Message));
}
