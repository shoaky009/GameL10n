using System.Net;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Text.RegularExpressions;

namespace GameL10n.Core.Development;

public sealed partial class AdaptationTools
{
    private const string ReferenceRoot = "https://raw.githubusercontent.com/GalTransl/GalTransl-GalGameReverse-Skill/main/";
    // Keep only bounded decoded documents in memory; raw downloads are shared outside project workspaces.
    private readonly Dictionary<string, ResearchDocument> researchDocuments = new(StringComparer.Ordinal);
    private sealed record ResearchDocument(string Content, string ContentSha256, DateTimeOffset RetrievedAt, bool UpstreamTruncated,
        bool Reused = false, bool Stale = false, long Bytes = 0);

    private async Task<ResearchDocument> ReadResearchAsync(string url, CancellationToken ct, bool refresh = false,
        int maximumBytes = 8 * 1024 * 1024)
    {
        ct.ThrowIfCancellationRequested();
        if (!refresh && researchDocuments.TryGetValue(url, out var cached)
            && (cached.Stale || DateTimeOffset.UtcNow - cached.RetrievedAt <= ReferenceCacheLifetime))
        {
            if (cached.Bytes > maximumBytes) throw DownloadTooLarge(maximumBytes);
            return cached with { Reused = true };
        }
        var download = await ReadPublicDownloadAsync(url, maximumBytes, refresh, ReferenceCacheLifetime, ct);
        var content = Encoding.UTF8.GetString(await File.ReadAllBytesAsync(download.Path, ct));
        if (download.Source.MediaType == "text/html")
        {
            content = Regex.Replace(content, @"<(script|style)\b[^>]*>[\s\S]*?</\1>", "", RegexOptions.IgnoreCase, TimeSpan.FromSeconds(1));
            content = WebUtility.HtmlDecode(Regex.Replace(content, "<[^>]+>", " ", RegexOptions.None, TimeSpan.FromSeconds(1)));
            content = Regex.Replace(content, @"[ \t]{2,}", " ", RegexOptions.None, TimeSpan.FromSeconds(1));
        }
        var upstreamTruncated = false;
        if (new Uri(url).Host.Equals("api.github.com", StringComparison.OrdinalIgnoreCase))
        {
            using var json = JsonDocument.Parse(content);
            upstreamTruncated = json.RootElement.ValueKind == JsonValueKind.Object
                && json.RootElement.TryGetProperty("truncated", out var flag) && flag.ValueKind == JsonValueKind.True;
        }
        var document = new ResearchDocument(content, Convert.ToHexStringLower(SHA256.HashData(Encoding.UTF8.GetBytes(content))),
            download.Source.RetrievedAt, upstreamTruncated, download.Reused, download.Stale, download.Source.Bytes);
        while (researchDocuments.Count > 0 && (researchDocuments.Count >= 4
            || researchDocuments.Values.Sum(item => item.Content.Length) + content.Length > 12 * 1024 * 1024))
            researchDocuments.Remove(researchDocuments.Keys.First());
        researchDocuments[url] = document;
        return document;
    }

    private async Task<object> FetchAsync(JsonElement args, CancellationToken ct)
    {
        var url = Text(args, "url");
        var query = Text(args, "query", "");
        if (query.Length > 256) throw new InvalidDataException("检索条件不能超过 256 字符。");
        var document = await ReadResearchAsync(url, ct, Boolean(args, "refresh", false));
        var content = document.Content;
        var offset = Number(args, "offset", 0, 0, content.Length);
        var length = Number(args, "length", 12000, 1, 24000);
        if (length < query.Length) throw new InvalidDataException("片段长度不能小于检索词长度。");
        if (query.Length == 0)
        {
            var nextOffset = Math.Min(content.Length, offset + length);
            return new { url, document.ContentSha256, document.RetrievedAt, document.UpstreamTruncated, document.Reused, document.Stale,
                totalCharacters = content.Length, offset, nextOffset, more = nextOffset < content.Length,
                content = content[offset..nextOffset] };
        }
        var matches = new List<object>();
        var position = offset;
        var remaining = length;
        while (matches.Count < 8 && remaining >= query.Length)
        {
            var found = content.IndexOf(query, position, StringComparison.OrdinalIgnoreCase);
            if (found < 0) { position = content.Length; break; }
            var start = Math.Max(0, found - Math.Min(240, (remaining - query.Length) / 4));
            var count = Math.Min(Math.Min(content.Length - start, query.Length + 720), remaining);
            matches.Add(new { matchOffset = found, offset = start, content = content.Substring(start, count) });
            remaining -= count;
            position = found + query.Length;
        }
        return new { url, document.ContentSha256, document.RetrievedAt, document.UpstreamTruncated, document.Reused, document.Stale,
            totalCharacters = content.Length, query, matches, nextOffset = position,
            more = position < content.Length && content.IndexOf(query, position, StringComparison.OrdinalIgnoreCase) >= 0 };
    }

    private async Task<object> SearchEngineReferencesAsync(JsonElement args, CancellationToken ct)
    {
        var query = Text(args, "query").Trim();
        if (query.Length is < 2 or > 128) throw new InvalidDataException("请使用 2 到 128 字符的引擎名称、别名或格式标识查询。");
        var key = NormalizeReferenceQuery(query);
        if (key is "siglus" or "siglusengine" or "scenepck")
            return new { query, status = "bundled", capability = SiglusAssets.Describe() };
        if (key.Length < 2) throw new InvalidDataException("查询必须包含引擎名称或格式标识。");
        if (key is "system4" or "system40") key = "alicesoft";
        var terms = ReferenceSearchTerms(query).Select(term => term is "system4" or "system40" ? "alicesoft" : term).ToArray();
        var url = ReferenceRoot + "catalog/engines.json";
        var selectedRoot = ReferenceRoot;
        ResearchDocument? document = null;
        object[] matches = [];
        var totalMatches = 0;
        string? error = null;
        try
        {
            var pin = session.EngineReference?.Kind == "galtransl" ? session.EngineReference : EngineToolkit.CurrentReference();
            if (pin is not null && File.Exists(EngineToolkit.LibraryPath(pin)))
            {
                var content = EngineToolkit.Read(pin, "catalog/engines.json");
                document = new(content, Convert.ToHexStringLower(SHA256.HashData(Encoding.UTF8.GetBytes(content))), DateTimeOffset.UtcNow, false, Reused: true);
                selectedRoot = ReferenceRoot.Replace("/main/", "/" + pin.Revision + "/", StringComparison.Ordinal);
                url = selectedRoot + "catalog/engines.json";
            }
            else document = await ReadResearchAsync(url, ct);
            using var json = JsonDocument.Parse(document.Content);
            if (json.RootElement.ValueKind != JsonValueKind.Object
                || !json.RootElement.TryGetProperty("engines", out var engines) || engines.ValueKind != JsonValueKind.Array
                || engines.EnumerateArray().Any(engine => engine.ValueKind != JsonValueKind.Object))
                throw new InvalidDataException("引擎参考索引结构已变化。");
            var found = engines.EnumerateArray().Select(engine => (Engine: engine, Rank: RankReference(engine, key, terms)))
                .Where(item => item.Rank.Exact > 0 || item.Rank.MatchedTerms > 0)
                .OrderByDescending(item => item.Rank.Exact).ThenByDescending(item => item.Rank.MatchedTerms)
                .ThenByDescending(item => item.Rank.Score).ThenBy(item => Text(item.Engine, "id"), StringComparer.Ordinal)
                .Select(item => item.Engine).ToArray();
            totalMatches = found.Length;
            matches = found.Take(6).Select(engine => (object)new
            {
                id = Text(engine, "id"), title = Limit(Text(engine, "title"), 300),
                aliases = ReferenceStrings(engine, "aliases").Take(12).ToArray(),
                documentUrl = ReferenceUrl(Text(engine, "document"), selectedRoot),
                sourceUrls = ReferenceStrings(engine, "python_references").Take(16).Select(path => ReferenceUrl(path, selectedRoot)).ToArray(),
                publicSymbols = engine.TryGetProperty("public_symbols", out var symbols) ? symbols.Clone() : (JsonElement?)null,
                scope = Limit(Text(engine, "scope", "Read the engine page for version and stage limits."), 1000),
                evidence = Limit(Text(engine, "evidence", "Reference only; no game verification implied."), 1000)
            }).ToArray();
        }
        catch (Exception exception) when (exception is HttpRequestException or IOException or JsonException
            || exception is OperationCanceledException && !ct.IsCancellationRequested)
        {
            error = Limit(exception.Message, 500);
        }
        return new
        {
            query, status = error is null ? "queried" : "unavailable", error,
            indexUrl = url, document?.ContentSha256, document?.RetrievedAt, document?.Reused, document?.Stale, totalMatches, matches,
            more = totalMatches > matches.Length,
            hint = error is null && totalMatches == 0 ? "请改用具体引擎名、别名或格式标识；archive、engine 等泛词不作为检索依据。未命中不代表不支持。" : null,
            guidance = ReferenceGuidance(key),
            furtherResearch = new
            {
                formats = ReferenceRoot + "catalog/formats.json", registrations = ReferenceRoot + "catalog/source-registries.json",
                formatNotes = ReferenceRoot + "catalog/format-research.md", provenance = ReferenceRoot + "provenance/NOTICE.md",
                sources = ReferenceRoot + "provenance/sources.json"
            },
            usage = "资料命中不代表已验证支持。优先 inspect_engine_reference(engine=id) 读取当前引擎指南与 API；固定版本共享源码库随完整发行包提供，所有项目本地复用，不在分析时补下载。"
                + "用 run_engine_python 调用现成 Python 模块验证样本，避免阅读/改写整套算法；use_engine_reference 可供正式适配器复用。"
                + "共享库不可用时可按 documentUrl 在线调查。未命中不代表不支持；可按 formats/registrations 继续查实际魔数。"
                + "按文件核对许可并保留来源；不自动部署 UIF、字体或 hook DLL。"
        };
    }

    private static string NormalizeReferenceQuery(string text) => string.Concat(text.Where(char.IsLetterOrDigit)).ToLowerInvariant();
    private static IEnumerable<string> ReferenceStrings(JsonElement item, string property) =>
        item.TryGetProperty(property, out var values) && values.ValueKind == JsonValueKind.Array
            ? values.EnumerateArray().Where(value => value.ValueKind == JsonValueKind.String).Select(value => value.GetString()!) : [];

    private static string[] ReferenceSearchTerms(string text) =>
        Regex.Split(text.ToLowerInvariant(), @"[^\p{L}\p{Nd}]+", RegexOptions.None, TimeSpan.FromSeconds(1))
            .Where(term => term.Length >= 2 && term is not ("archive" or "archives" or "engine" or "engines"
                or "python" or "py" or "format" or "formats" or "script" or "scripts" or "file" or "files"
                or "reference" or "references" or "tool" or "tools"))
            .Distinct(StringComparer.Ordinal).ToArray();

    private static (int Exact, int MatchedTerms, int Score) RankReference(JsonElement item, string query, string[] terms)
    {
        var id = Text(item, "id", "");
        var title = Text(item, "title", "");
        var aliases = ReferenceStrings(item, "aliases").ToArray();
        var exact = NormalizeReferenceQuery(id) == query ? 3
            : aliases.Any(alias => NormalizeReferenceQuery(alias) == query) ? 2
            : terms.Length > 0 && NormalizeReferenceQuery(title) == query ? 1 : 0;
        var fields = new[] { (Value: id, Weight: 12), (Value: title, Weight: 6) }
            .Concat(aliases.Select(alias => (Value: alias, Weight: 10)))
            .Concat(ReferenceStrings(item, "python_references").Select(path => (Value: path, Weight: 8)))
            .Select(field => (Terms: ReferenceSearchTerms(field.Value), field.Weight)).ToArray();
        var matchedTerms = 0;
        var score = 0;
        foreach (var term in terms)
        {
            var weight = fields.Max(field => field.Terms.Contains(term, StringComparer.Ordinal) ? field.Weight
                : term.Length >= 3 && field.Terms.Any(value => value.StartsWith(term, StringComparison.Ordinal)) ? field.Weight / 2 : 0);
            if (weight == 0) continue;
            matchedTerms++;
            score += weight;
        }
        return (exact, matchedTerms, score);
    }

    private static string ReferenceUrl(string path, string root = ReferenceRoot)
    {
        if (path.Length == 0 || path.StartsWith('/') || path.Contains('\\') || path.Split('/').Any(part => part is "." or "..")
            || path.Any(character => !char.IsAsciiLetterOrDigit(character) && character is not ('-' or '_' or '.' or '/')))
            throw new InvalidDataException("参考索引含无效资源路径。");
        return root + path;
    }

    private static IReadOnlyList<object> ReferenceGuidance(string query)
    {
        var guidance = new List<object>
        {
            new { topic = "缺口定位", url = ReferenceRoot + "guides/extending-engines.md",
                note = "分别确认容器、解码、文本语义、回填、打包、运行显示。缺 writer 或已知算法尚未实现时继续补齐当前层；没有搜索结果不是不支持的证据。" }
        };
        if (query == "kag" || new[] { "kirikiri", "krkr", "xp3", "吉里吉里" }.Any(query.Contains))
            guidance.Add(new { topic = "Kirikiri 脚本编码", url = ReferenceRoot + "engines/kirikiri.md",
                note = KirikiriTextGuidance.Summary });
        if (query.Contains("qlie", StringComparison.Ordinal) || query.Contains("imos", StringComparison.Ordinal))
            guidance.Add(new { topic = "QLIE 方言", url = ReferenceRoot + "engines/qlie.md",
                note = "先确认 PACK/Hash 版本与脚本方言。核对内部姓名和显示姓名（＠）、多行对白、^select 与跳转命令、^savetext；[pc,...] 可能是布局命令或显示文本，按实际语法判断，不能一律翻译或冻结。" });
        if (query.Contains("bishop", StringComparison.Ordinal) || query.Contains("bsscript", StringComparison.Ordinal) || query == "bsx")
            guidance.Add(new { topic = "Bishop 格式区别", url = ReferenceRoot + "engines/bishop.md",
                note = BuiltinAdapterDiscovery.BsscriptGuidance });
        if (query.Contains("nekosdk", StringComparison.Ordinal) || query.Contains("nekopack", StringComparison.Ordinal))
            guidance.Add(new { topic = "NekoSDK 的解析与显示分别验证", url = ReferenceRoot + "engines/nekosdk.md",
                note = "先调用 python.archives.nekopack 的 open_pak/read_member/build_pak，以及 python.engines.nekosdk 的 extract_fields/replace_fields。字段为空可能是命令记录变体，不代表无文本；用已解包样本确认结构后只补变体。容器和往返通过后集中处理当前显示路径，避免重新搜索已解决的解包算法。若采用 VNTextProxy，固定代理版本，核对映射表索引、过滤字节和实际 EXE 字符遍历边界，再批量测试边界码位及容量；单个字形探针不证明游戏运行兼容。" });
        if (new[] { "cp932", "sjis", "shiftjis", "jis", "字体", "编码", "alicesoft", "ain", "qlie", "imos" }.Any(query.Contains))
            guidance.Add(new { topic = "中文编码与字符映射", url = ReferenceRoot + "guides/jis-substitution.md",
                note = "先证明脚本确为 CP932。映射在整个补丁范围统一规划，避开未翻译正文、姓名、界面字及已有映射；译文缓存保留真实中文。重提取实际产物并比对修改与未修改项。编码可逆不证明原引擎接受私用码位或游戏内字形正确；不能默认部署参考项目的 DLL。" });
        return guidance;
    }
}
