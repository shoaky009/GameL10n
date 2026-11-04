using System.Text;
using System.Text.Json;
using GameL10n.Core.Storage;

namespace GameL10n.Core.Development;

/// <summary>Aggregates project metadata locally; source and diagnostic excerpts are read only on demand.</summary>
public sealed class EngineReviewEvidence
{
    private static readonly JsonSerializerOptions EvidenceJson = new() { Encoder = System.Text.Encodings.Web.JavaScriptEncoder.UnsafeRelaxedJsonEscaping };
    private readonly Dictionary<string, string> hashes = new(StringComparer.Ordinal);
    private readonly HashSet<string> inspected = new(StringComparer.Ordinal);
    private string project = "";
    private string? gameDirectory;
    private string? gameMetadata;
    private readonly HashSet<string> engineKeys = new(StringComparer.OrdinalIgnoreCase);
    public IReadOnlyCollection<string> EngineKeys => engineKeys;
    public string Fingerprint => LocalFiles.HashText(string.Join('\n', hashes.OrderBy(pair => pair.Key, StringComparer.Ordinal).Select(pair => pair.Key + ":" + pair.Value)));
    public object Overview { get; private set; } = new { };
    public IReadOnlyCollection<string> Files => hashes.Keys;

    public static async Task<EngineReviewEvidence> LoadAsync(string projectDirectory, CancellationToken ct)
    {
        var result = new EngineReviewEvidence { project = Path.GetFullPath(projectDirectory) };
        object? sessionSummary = null;
        var sessionPath = LocalFiles.UnderRoot(result.project, "adaptation/current/session.json");
        if (File.Exists(sessionPath))
        {
            await result.AddAsync("adaptation/current/session.json", ct);
            await using var stream = File.OpenRead(sessionPath);
            using var doc = await JsonDocument.ParseAsync(stream, cancellationToken: ct);
            var session = doc.RootElement;
            if (session.TryGetProperty("verifiedPackage", out var verified) && verified.ValueKind == JsonValueKind.Object
                && verified.TryGetProperty("family", out var family) && family.ValueKind == JsonValueKind.Object)
            {
                result.engineKeys.Add(Text(family, "key", 160)); result.engineKeys.Add(Text(family, "name", 160));
            }
            if (session.TryGetProperty("builtinDiscovery", out var discovery) && discovery.ValueKind == JsonValueKind.Object
                && discovery.TryGetProperty("candidates", out var candidates) && candidates.ValueKind == JsonValueKind.Array)
                foreach (var candidate in candidates.EnumerateArray())
                {
                    result.engineKeys.Add(Text(candidate, "format", 160)); result.engineKeys.Add(Text(candidate, "adapterId", 160));
                }
            sessionSummary = new
            {
                summary = Text(session, "summary", 6000), notes = Text(session, "notes", 6000),
                status = Text(session, "lastStatus", 80), nextAction = Text(session, "nextAction", 1600),
                verification = Raw(session, "verificationDiagnostics", 8000),
                coverage = Raw(session, "textCoverage", 6000), candidates = Raw(session, "builtinDiscovery", 8000),
                family = session.TryGetProperty("verifiedPackage", out var package) && package.ValueKind == JsonValueKind.Object
                    ? Raw(package, "family", 800) : null
            };
        }
        var gamePath = LocalFiles.UnderRoot(result.project, "game.json");
        if (File.Exists(gamePath))
        {
            using var doc = JsonDocument.Parse(await File.ReadAllTextAsync(gamePath, ct));
            result.engineKeys.Add(Text(doc.RootElement, "adapterId", 160));
            result.gameDirectory = Text(doc.RootElement, "gameDirectory", 4096);
            result.gameMetadata = GameMetadata(doc.RootElement);
            result.hashes["game.json"] = LocalFiles.HashText(result.gameMetadata);
        }
        result.engineKeys.RemoveWhere(string.IsNullOrWhiteSpace);
        var files = new List<object>();
        var partialInventory = false;
        foreach (var area in new[] { "src", "work" })
        {
            var root = LocalFiles.UnderRoot(result.project, "adaptation/current/" + area);
            if (!Directory.Exists(root)) continue;
            var scanned = 0;
            foreach (var file in Directory.EnumerateFiles(root, "*", new EnumerationOptions { RecurseSubdirectories = true, AttributesToSkip = FileAttributes.ReparsePoint }))
            {
                ct.ThrowIfCancellationRequested();
                if (++scanned > 4096 || files.Count >= 128) { partialInventory = true; break; }
                if (Path.GetExtension(file).ToLowerInvariant() is not (".cs" or ".py" or ".md" or ".json" or ".txt")) continue;
                var size = new FileInfo(file).Length;
                if (size > 512 * 1024) continue;
                var relative = Path.GetRelativePath(result.project, file).Replace('\\', '/');
                await result.AddAsync(relative, ct);
                files.Add(new { path = relative, bytes = size });
            }
        }
        var tools = new Dictionary<string, (int Calls, int Failures, double Ms, long Bytes)>();
        var repeats = new Dictionary<string, int>();
        var failures = new Queue<object>();
        var runs = new Dictionary<string, object>();
        long inputTokens = 0, outputTokens = 0;
        double apiMs = 0, queueMs = 0;
        int calls = 0, incompleteLines = 0;
        object? lastSourceEdit = null, lastVerification = null, lastSuccessfulVerification = null;
        var logs = LocalFiles.UnderRoot(result.project, "logs");
        if (Directory.Exists(logs))
        foreach (var path in Directory.EnumerateFiles(logs, "adaptation-*.jsonl").Order(StringComparer.Ordinal))
        {
            var relative = Path.GetRelativePath(result.project, path).Replace('\\', '/');
            await result.AddAsync(relative, ct);
            foreach (var line in File.ReadLines(path))
            {
                ct.ThrowIfCancellationRequested();
                if (string.IsNullOrWhiteSpace(line)) continue;
                try
                {
                    using var doc = JsonDocument.Parse(line);
                    var row = doc.RootElement;
                    if (!row.TryGetProperty("entry", out var entry)) continue;
                    var operation = Text(entry, "operation", 80);
                    if (operation is "run-start" or "model-start")
                        runs[Text(row, "runId", 80)] = new { at = Text(row, "at", 80), coreBuild = Text(entry, "coreBuild", 80),
                            model = Text(entry, "model", 160), instructions = Number(entry, "instructionBytes"), tools = Number(entry, "toolDefinitionBytes") };
                    if (operation == "model")
                    {
                        calls++; inputTokens += (long)Number(entry, "inputTokens"); outputTokens += (long)Number(entry, "outputTokens");
                        apiMs += Number(entry, "apiMs"); queueMs += Number(entry, "queueMs");
                    }
                    if (operation == "output-limit-recovery")
                    {
                        inputTokens += (long)Number(entry, "inputTokens"); outputTokens += (long)Number(entry, "outputTokens");
                    }
                    if (operation != "tool") continue;
                    var tool = Text(entry, "tool", 80);
                    var failed = Text(entry, "status", 80) == "failed";
                    var eventInfo = new { path = relative, at = Text(row, "at", 80), runId = Text(row, "runId", 80),
                        turn = Number(row, "turn"), callId = Text(entry, "callId", 160), tool,
                        status = Text(entry, "status", 80), result = Raw(entry, "result", 2400), error = Text(entry, "error", 1200) };
                    if (tool is "write_file" or "replace_text" && !failed) lastSourceEdit = eventInfo;
                    if (tool == "test_adapter")
                    {
                        lastVerification = eventInfo;
                        if (!failed && entry.TryGetProperty("result", out var testResult)
                            && testResult.ValueKind == JsonValueKind.Object
                            && testResult.TryGetProperty("passed", out var passed) && passed.ValueKind == JsonValueKind.True)
                            lastSuccessfulVerification = eventInfo;
                    }
                    var prior = tools.GetValueOrDefault(tool);
                    tools[tool] = (prior.Calls + 1, prior.Failures + (failed ? 1 : 0), prior.Ms + Number(entry, "elapsedMs"), prior.Bytes + (long)Number(entry, "outputBytes"));
                    if (entry.TryGetProperty("call", out var call))
                    {
                        var fingerprint = Text(call, "argumentFingerprint", 80);
                        if (fingerprint.Length > 0) repeats[tool + ":" + fingerprint] = repeats.GetValueOrDefault(tool + ":" + fingerprint) + 1;
                    }
                    if (failed)
                    {
                        failures.Enqueue(new { at = Text(row, "at", 80), tool, error = Text(entry, "error", 1200) });
                        if (failures.Count > 12) failures.Dequeue();
                    }
                }
                catch (JsonException) { incompleteLines++; }
            }
        }
        result.Overview = new
        {
            modelCalls = calls, inputTokens, outputTokens, apiSeconds = apiMs / 1000, queueSeconds = queueMs / 1000,
            tools = tools.OrderByDescending(pair => pair.Value.Ms).Select(pair => new
            { tool = pair.Key, calls = pair.Value.Calls, failures = pair.Value.Failures, seconds = pair.Value.Ms / 1000, outputBytes = pair.Value.Bytes }),
            repeatedArguments = repeats.Where(pair => pair.Value > 1).OrderByDescending(pair => pair.Value).Take(12)
                .Select(pair => new { fingerprint = pair.Key, calls = pair.Value }),
            recentFailures = failures.ToArray(), recentRuns = runs.TakeLast(12).ToDictionary(), session = sessionSummary,
            lastSourceEdit, lastVerification, lastSuccessfulVerification,
            files, logFiles = result.hashes.Keys.Where(path => path.StartsWith("logs/", StringComparison.Ordinal)).ToArray(), partialInventory, incompleteLines,
            caveat = "相同参数可能因源码或资源变化而需要重跑；累计耗时不是单次耗时；旧日志缺失字段不等于没有问题。静态验收不代表实机显示或完整覆盖。"
        };
        return result;
    }

    public async Task<object> ReadAsync(string path, int offset, int length, CancellationToken ct, string? query = null)
    {
        if (offset < 0 || length is < 1 or > 12000) throw new InvalidDataException("读取范围无效；每次最多 12000 字符。");
        if (query?.Length > 256) throw new InvalidDataException("查询最多 256 字符。");
        path = path.Replace('\\', '/');
        if (path == "overview.json") path = "overview";
        if (path.EndsWith("session.json", StringComparison.Ordinal)) throw new InvalidDataException("会话摘要已在 overview 中；请读取日志或源码，避免回放全部 API 历史。");
        var source = path == "overview" ? JsonSerializer.Serialize(Overview, EvidenceJson) : await ReadTextAsync(path, ct);
        inspected.Add(path);
        offset = Math.Min(offset, source.Length);
        var found = string.IsNullOrEmpty(query) ? offset : source.IndexOf(query, offset, StringComparison.OrdinalIgnoreCase);
        if (found < 0) return new { path, queryFound = false, totalCharacters = source.Length };
        offset = found;
        var count = Math.Min(length, source.Length - offset);
        return new { path, offset, totalCharacters = source.Length, queryFound = true,
            nextOffset = offset + count, content = source.Substring(offset, count) };
    }

    public async Task<string> ToolSourceAsync(string path, CancellationToken ct)
    {
        path = path.Replace('\\', '/');
        if (!inspected.Contains(path) || !path.StartsWith("adaptation/current/", StringComparison.Ordinal)
            || Path.GetExtension(path) is not (".cs" or ".py"))
            throw new InvalidDataException("只能复用已经读取过的项目 C#／Python 辅助源码。");
        var text = await ReadTextAsync(path, ct);
        if (text.Length > 64 * 1024 || new[] { project, gameDirectory }.Where(path => !string.IsNullOrWhiteSpace(path))
            .Any(path => new[] { path!, path!.Replace("\\", "\\\\"), path.Replace('\\', '/') }
                .Any(candidate => text.Contains(candidate, StringComparison.OrdinalIgnoreCase))))
            throw new InvalidDataException("工具源码过长或写死了项目目录，请将其参数化后再总结。");
        return text;
    }

    public bool IsEvidence(string path) => path is "overview" or "overview.json" || inspected.Contains(path.Replace('\\', '/'));

    public async Task EnsureUnchangedAsync(CancellationToken ct)
    {
        foreach (var (relative, hash) in hashes)
            if (relative == "game.json") await CheckGameMetadataAsync(ct);
            else if (await LocalFiles.HashFileAsync(LocalFiles.UnderRoot(project, relative), ct) != hash)
                throw new IOException("总结期间项目证据发生了变化，请待当前任务结束后重新总结。");
    }

    private async Task AddAsync(string relative, CancellationToken ct) => hashes[relative] = await LocalFiles.HashFileAsync(LocalFiles.UnderRoot(project, relative), ct);
    private async Task<string> ReadTextAsync(string path, CancellationToken ct)
    {
        if (!hashes.TryGetValue(path, out var hash)) throw new InvalidDataException("只能读取当前项目证据索引中的文件。");
        if (path == "game.json") { await CheckGameMetadataAsync(ct); return gameMetadata!; }
        var fullPath = LocalFiles.UnderRoot(project, path);
        if (await LocalFiles.HashFileAsync(fullPath, ct) != hash) throw new IOException("证据文件已变化，请重新总结。");
        return await File.ReadAllTextAsync(fullPath, Encoding.UTF8, ct);
    }

    private async Task CheckGameMetadataAsync(CancellationToken ct)
    {
        using var doc = JsonDocument.Parse(await File.ReadAllTextAsync(LocalFiles.UnderRoot(project, "game.json"), ct));
        if (GameMetadata(doc.RootElement) != gameMetadata)
            throw new IOException("总结期间游戏目录或解析结果发生变化，请重新总结。");
    }

    // Task/UI/API settings change while a summary runs; only game identity and extraction inputs are evidence.
    private static string GameMetadata(JsonElement game) => JsonSerializer.Serialize(new
    {
        id = Text(game, "id", 160), gameDirectory = Text(game, "gameDirectory", 4096),
        adapterId = Text(game, "adapterId", 160), adapterVersion = Text(game, "adapterVersion", 160),
        sourceFingerprint = game.TryGetProperty("analysis", out var analysis) && analysis.ValueKind == JsonValueKind.Object
            ? Text(analysis, "sourceFingerprint", 256) : null
    }, EvidenceJson);

    private static double Number(JsonElement value, string key) => value.TryGetProperty(key, out var item) && item.ValueKind == JsonValueKind.Number && item.TryGetDouble(out var number) ? number : 0;
    private static string Text(JsonElement value, string key, int limit) => value.TryGetProperty(key, out var item) && item.ValueKind == JsonValueKind.String
        ? Cut(item.GetString()!, limit) : "";
    private static string? Raw(JsonElement value, string key, int limit) => value.TryGetProperty(key, out var item) ? Cut(JsonSerializer.Serialize(item, EvidenceJson), limit) : null;
    private static string Cut(string value, int limit) => value.Length > limit ? value[..limit] + "…" : value;
}
