using System.Collections.Concurrent;
using System.Text.Json;
using System.Text.RegularExpressions;
using AvgTranslator.Core.Adapters;
using AvgTranslator.Core.Storage;

namespace AvgTranslator.Core.Development;

public sealed record EngineKnowledgeTool(string Name, string Description, string Usage, IReadOnlyDictionary<string, string> Files);
public sealed record EngineKnowledgeOrigin(string GameId, string Title, string EvidenceFingerprint, IReadOnlyList<string> Evidence);
public sealed record EngineKnowledgeEntry(string Key, AdapterFamily Family, IReadOnlyList<string> Aliases,
    string Conditions, string Summary, string Guidance, IReadOnlyList<EngineKnowledgeTool> Tools,
    IReadOnlyList<EngineKnowledgeOrigin> Origins, string Revision = "", bool Enabled = true)
{
    public string Id => Family.AdapterId + "." + Key;
}

public sealed record EngineKnowledgeSummary(string Id, AdapterFamily Family, IReadOnlyList<string> Aliases,
    string Conditions, string Summary, IReadOnlyList<EngineKnowledgeOrigin> Origins, string Revision, bool Enabled, int ToolCount);

/// <summary>Shared engine guidance and source helpers. These are references, never adapter verification claims.</summary>
public sealed class EngineKnowledgeStore(string directory)
{
    private static readonly ConcurrentDictionary<string, SemaphoreSlim> Locks = new(StringComparer.OrdinalIgnoreCase);
    public string DirectoryPath { get; } = Path.GetFullPath(directory);
    public static EngineKnowledgeStore ForAdapters(LocalAdapterPackageStore store) => new(Path.Combine(store.RootDirectory, "engine-knowledge"));

    /// <summary>Display metadata only. Source helpers and guidance are validated when the entry is opened or used.</summary>
    public IReadOnlyList<EngineKnowledgeSummary> ReadSummaries(Action<string>? warning = null)
    {
        if (!Directory.Exists(DirectoryPath)) return [];
        var entries = new List<EngineKnowledgeSummary>();
        foreach (var path in Directory.EnumerateFiles(DirectoryPath, "*.json").Order(StringComparer.Ordinal))
        {
            try
            {
                var summary = JsonSerializer.Deserialize<KnowledgeMetadata>(
                    ReadKnowledgeBytes(Path.GetFileName(path)), LocalFiles.JsonOptions)
                    ?? throw new InvalidDataException("经验文件为空。");
                if (summary.Family is null) throw new InvalidDataException("缺少引擎分类。");
                summary.Family.Validate();
                var id = summary.Family.AdapterId + "." + summary.Key;
                if (Path.GetFileNameWithoutExtension(path) != id || summary.Aliases is null || summary.Origins is null
                    || summary.Tools is null || summary.Tools.Length > 4)
                    throw new InvalidDataException("经验索引内容无效。");
                RequireText(summary.Summary, 400, "概要");
                RequireText(summary.Conditions, 1600, "适用条件");
                entries.Add(new(id, summary.Family, summary.Aliases, summary.Conditions, summary.Summary,
                    summary.Origins, summary.Revision, summary.Enabled, summary.Tools.Length));
            }
            catch (Exception error) when (error is IOException or InvalidDataException or UnauthorizedAccessException or JsonException or ArgumentException)
            { warning?.Invoke("已跳过不可用的引擎经验 " + Path.GetFileName(path) + "：" + error.Message); }
        }
        return entries;
    }

    public EngineKnowledgeEntry? ReadEntry(string id)
    {
        if (!Regex.IsMatch(id, "^[a-z0-9][a-z0-9.-]{1,159}$", RegexOptions.CultureInvariant))
            throw new InvalidDataException("无效的引擎经验标识。");
        var path = Isolation.WorkspaceFiles.Resolve(DirectoryPath, id + ".json");
        if (!File.Exists(path)) return null;
        var entry = JsonSerializer.Deserialize<EngineKnowledgeEntry>(
            ReadKnowledgeBytes(id + ".json"), LocalFiles.JsonOptions)
            ?? throw new InvalidDataException("经验文件为空。");
        Validate(entry);
        if (entry.Id != id || entry.Revision != ContentRevision(entry))
            throw new InvalidDataException("经验内容或标识与版本不一致。");
        return entry;
    }

    private byte[] ReadKnowledgeBytes(string fileName)
    {
        var path = Isolation.WorkspaceFiles.Resolve(DirectoryPath, fileName);
        using var stream = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read | FileShare.Delete);
        if (stream.Length > 512 * 1024) throw new InvalidDataException("经验文件过大。");
        var bytes = new byte[(int)stream.Length];
        stream.ReadExactly(bytes);
        return bytes;
    }

    // Unknown properties are skipped by the serializer, including embedded helper source files.
    private sealed record KnowledgeMetadata(string Key, AdapterFamily Family, IReadOnlyList<string> Aliases,
        string Conditions, string Summary, IReadOnlyList<EngineKnowledgeOrigin> Origins, ToolMetadata[] Tools,
        string Revision = "", bool Enabled = true);
    private sealed record ToolMetadata();

    public IReadOnlyList<EngineKnowledgeEntry> ReadAll(Action<string>? warning = null)
    {
        if (!Directory.Exists(DirectoryPath)) return [];
        var entries = new List<EngineKnowledgeEntry>();
        foreach (var path in Directory.EnumerateFiles(DirectoryPath, "*.json", SearchOption.TopDirectoryOnly).Order(StringComparer.Ordinal))
        {
            try
            {
                if (ReadEntry(Path.GetFileNameWithoutExtension(path)) is { } entry) entries.Add(entry);
            }
            catch (Exception error) when (error is IOException or InvalidDataException or UnauthorizedAccessException or JsonException or ArgumentException)
            { warning?.Invoke("已跳过不可用的引擎经验 " + Path.GetFileName(path) + "：" + error.Message); }
        }
        return entries;
    }

    public static string Fingerprint(IEnumerable<EngineKnowledgeEntry> entries) => LocalFiles.HashText(string.Join('\n',
        entries.OrderBy(entry => entry.Id, StringComparer.Ordinal).Select(entry => entry.Id + ":" + entry.Revision + ":" + entry.Enabled)));

    public static IReadOnlyList<EngineKnowledgeEntry> Match(IEnumerable<EngineKnowledgeEntry> entries, IEnumerable<string> engineKeys)
    {
        var keys = engineKeys.Where(value => !string.IsNullOrWhiteSpace(value)).Select(value => value.Trim()).ToHashSet(StringComparer.OrdinalIgnoreCase);
        return entries.Where(entry => entry.Enabled && new[] { entry.Family.Key, entry.Family.AdapterId, entry.Family.Name }
            .Concat(entry.Aliases).Any(keys.Contains)).OrderBy(entry => entry.Id, StringComparer.Ordinal).ToArray();
    }

    public async Task<EngineKnowledgeEntry> SaveAsync(EngineKnowledgeEntry entry, string? expectedRevision, CancellationToken ct = default)
    {
        Validate(entry);
        var gate = Locks.GetOrAdd(DirectoryPath, _ => new(1, 1));
        await gate.WaitAsync(ct);
        try
        {
            var current = ReadEntry(entry.Id);
            if (current?.Revision != expectedRevision)
                throw new InvalidDataException("全局引擎经验已变化，请重新读取后再合并，不能覆盖较新的结论。");
            entry = entry with { Tools = (current?.Tools ?? []).Where(tool => !entry.Tools.Any(replacement => replacement.Name == tool.Name))
                .Concat(entry.Tools).ToArray() };
            Validate(entry);
            var origins = (current?.Origins ?? []).Concat(entry.Origins).GroupBy(origin => origin.GameId, StringComparer.Ordinal)
                .Select(group => group.Last()).ToArray();
            entry = entry with { Origins = origins, Enabled = current?.Enabled ?? entry.Enabled, Revision = ContentRevision(entry) };
            if (current is null || JsonSerializer.Serialize(current) != JsonSerializer.Serialize(entry))
                await LocalFiles.WriteJsonAsync(LocalFiles.UnderRoot(DirectoryPath, entry.Id + ".json"), entry, ct);
            await UpdateSummaryIndexAsync(entry, CancellationToken.None);
            return entry;
        }
        finally { gate.Release(); }
    }

    public async Task SetEnabledAsync(string id, bool enabled, CancellationToken ct = default)
    {
        var gate = Locks.GetOrAdd(DirectoryPath, _ => new(1, 1));
        await gate.WaitAsync(ct);
        try
        {
            var current = ReadEntry(id) ?? throw new InvalidDataException("引擎经验已不存在，请刷新列表。");
            var updated = current with { Enabled = enabled };
            await LocalFiles.WriteJsonAsync(LocalFiles.UnderRoot(DirectoryPath, current.Id + ".json"), updated, ct);
            await UpdateSummaryIndexAsync(updated, CancellationToken.None);
        }
        finally { gate.Release(); }
    }

    private Task UpdateSummaryIndexAsync(EngineKnowledgeEntry entry, CancellationToken ct) =>
        new LocalAdapterPackageStore(Path.GetDirectoryName(DirectoryPath)!).RecordKnowledgeIndexAsync(new(entry.Id,
            entry.Family, entry.Aliases, entry.Conditions, entry.Summary, entry.Origins, entry.Revision, entry.Enabled, entry.Tools.Count), ct);

    public static void Validate(EngineKnowledgeEntry entry)
    {
        if (entry.Family is null) throw new InvalidDataException("经验必须按引擎或结构模板归档。");
        entry.Family.Validate();
        if (entry.Key is null || !Regex.IsMatch(entry.Key, "^[a-z0-9][a-z0-9.-]{0,63}$", RegexOptions.CultureInvariant))
            throw new InvalidDataException("经验标识须为引擎格式或问题类别，使用小写字母、数字、点和连字符。");
        RequireText(entry.Conditions, 1600, "适用条件");
        RequireText(entry.Summary, 400, "概要");
        RequireText(entry.Guidance, 8000, "分析指引");
        if (entry.Aliases is null || entry.Aliases.Count > 12 || entry.Aliases.Any(alias => string.IsNullOrWhiteSpace(alias) || alias.Length > 160))
            throw new InvalidDataException("引擎别名无效。");
        if (entry.Tools is null || entry.Tools.Count > 4 || entry.Tools.Any(tool => tool is null)
            || entry.Tools.Select(tool => tool.Name).Distinct(StringComparer.Ordinal).Count() != entry.Tools.Count
            || entry.Origins is null || entry.Origins.Count == 0 || entry.Origins.Any(origin => origin is null || string.IsNullOrWhiteSpace(origin.GameId)
                || origin.Evidence is null || origin.Evidence.Count == 0))
            throw new InvalidDataException("经验的工具或证据来源无效。");
        var sourceBytes = 0;
        foreach (var tool in entry.Tools)
        {
            RequireText(tool.Name, 80, "工具名"); RequireText(tool.Description, 400, "工具说明"); RequireText(tool.Usage, 1600, "工具用法");
            if (tool.Files is null || tool.Files.Count is < 1 or > 12
                || tool.Files.Keys.Distinct(StringComparer.OrdinalIgnoreCase).Count() != tool.Files.Count)
                throw new InvalidDataException("工具须包含 1–12 个不重名的源码文件。");
            foreach (var (path, source) in tool.Files)
            {
                _ = Isolation.WorkspaceFiles.Resolve(Path.GetTempPath(), path);
                if (Path.GetExtension(path) is not (".cs" or ".py") || source is null || source.Length > 64 * 1024)
                    throw new InvalidDataException("共享工具只接受有界的 C# 或 Python 源码。");
                sourceBytes += System.Text.Encoding.UTF8.GetByteCount(source);
            }
        }
        if (sourceBytes > 128 * 1024) throw new InvalidDataException("一个经验条目的工具源码总量不能超过 128 KiB。");
    }

    private static void RequireText(string? value, int maximum, string name)
    {
        if (string.IsNullOrWhiteSpace(value) || value.Length > maximum) throw new InvalidDataException(name + "为空或过长。");
    }

    private static string ContentRevision(EngineKnowledgeEntry entry) => LocalFiles.HashText(JsonSerializer.Serialize(new
    {
        entry.Key, entry.Family, aliases = entry.Aliases.Order(StringComparer.Ordinal), entry.Conditions, entry.Summary, entry.Guidance,
        tools = entry.Tools.OrderBy(tool => tool.Name, StringComparer.Ordinal).Select(tool => new
        { tool.Name, tool.Description, tool.Usage, files = tool.Files.OrderBy(pair => pair.Key, StringComparer.Ordinal) })
    }));
}
