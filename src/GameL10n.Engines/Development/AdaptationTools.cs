using System.Net.Http.Headers;
using System.Security.Cryptography;
using System.Text;
using System.Text.Encodings.Web;
using System.Text.Json;
using GameL10n.Core.Adapters;
using GameL10n.Core.Isolation;
using GameL10n.Core.Storage;
using System.Text.Json.Nodes;
using GameL10n.Core.Serialization;

namespace GameL10n.Core.Development;

/// <summary>Host operations for one engine development workspace; no provider credentials are passed to tools.</summary>
public sealed partial class AdaptationTools(AdaptationRequest request, AdaptationSession session, string workspace,
    AdapterRuntime runtime, LocalAdapterPackageStore store, HttpClient? researchClient = null,
    BuiltinAdapterAssets? builtinAssets = null, BuiltinAdapterTrial? builtinTrial = null, AdapterComponentAssets? componentAssets = null,
    Action<string, string>? reportProgress = null, AdapterDevelopmentWorkflow? development = null)
{
    private static readonly HttpClient PublicHttp = new(new HttpClientHandler { AllowAutoRedirect = false, UseCookies = false })
        { Timeout = TimeSpan.FromMinutes(3) };
    private static readonly JsonSerializerOptions Json = new(EngineJson.Web)
        { Encoder = JavaScriptEncoder.UnsafeRelaxedJsonEscaping };
    private AdapterDevelopmentWorkflow? developmentWorkflow = development;
    internal AdapterDevelopmentWorkflow Development => developmentWorkflow ??= new(request, session, workspace,
        runtime, store, researchClient, builtinAssets, reportProgress);
    public IGameAdapter? PublishedAdapter => Development.PublishedAdapter;
    private string ResourceDirectory => Path.Combine(workspace, "resources");
    private string SourceDirectory => Path.Combine(workspace, "src");
    private int unresolvedStopReports;

    public static IReadOnlyList<AgentToolDefinition> Definitions { get; } =
    [
        Define("load_tool_groups", "Select tool schemas for the next request. mode=add (default) loads more groups; mode=replace keeps only the supplied groups, or basic tools with groups=[]. Load the relevant groups; all is available for broad inspection. Checkpoint/resume/model changes trim unused schemas, and any group can be reloaded. Local only; does not run tools or download anything. Call alone.",
            AdaptationToolGroups.Parameters, "groups"),
        Define("report_progress", "Brief Chinese progress for the user when evidence or direction changes. exploring: unresolved issue and next probe; breakthrough: observed evidence and what it enables; implementing: established route; correction: retract an earlier claim. <=400 characters, no private reasoning, percentages or ETA. Accompany normal tools in the same response; no extra model turn. This report never validates or publishes an adapter.",
            """{"kind":{"type":"string","enum":["exploring","breakthrough","implementing","correction"]},"message":{"type":"string","minLength":1,"maxLength":400}}""", "kind", "message"),
        Define("inspect_engine_knowledge", "Search shared engine/format experience from previous projects. With id, read conditional guidance and helper index. Without id, query searches engine names/aliases; empty query lists this game's candidates (exact matches first, then related variants). offset pages through candidates. Verify conditions locally; a family match does not prove variant compatibility, and no match does not mean unsupported.",
            """{"query":{"type":"string"},"id":{"type":"string"},"offset":{"type":"integer","minimum":0,"maximum":100000}}""", []),
        Define("copy_engine_knowledge_tools", "Copy one shared source helper into work/ without overwriting edits. Requires id and tool name from inspect_engine_knowledge. Inspect applicability and dependencies, then use run_engine_python on current samples. Does not execute, validate or publish an adapter. Call alone.",
            """{"id":{"type":"string"},"tool":{"type":"string"}}""", "id", "tool"),
        Define("list_files", "List game or workspace files with sizes. Pagination, no file contents.",
            """{"root":{"type":"string","enum":["game","workspace"]},"path":{"type":"string"},"offset":{"type":"integer","minimum":0,"maximum":1000000},"limit":{"type":"integer","minimum":1,"maximum":200}}""", "root"),
        Define("read_file", "Read a bounded file slice. encoding: hex, utf8, cp932, cp936. Source/JSON and workspace work/*.txt/.log/.csv reports default to utf8; unknown game resources default to hex. Use search_text byteOffsets for targeted reads instead of rereading whole files. Returns the encoding used.",
            """{"root":{"type":"string","enum":["game","workspace"]},"path":{"type":"string"},"offset":{"type":"integer","minimum":0},"length":{"type":"integer","minimum":1,"maximum":32768},"encoding":{"type":"string","enum":["hex","utf8","cp932","cp936"]}}""", "root", "path"),
        Define("read_tool_result", "Read a character slice of an earlier tool result omitted from replay. Uses the existing session, no rerun or external request. Optional query locates relevant text. Original results remain available across automatic compaction, checkpoint and restart.",
            """{"toolCallId":{"type":"string"},"offset":{"type":"integer","minimum":0},"length":{"type":"integer","minimum":1,"maximum":16384},"query":{"type":"string","maxLength":256}}""", "toolCallId"),
        Define("read_session_history", "Retrieve an archived message's text and exact tool arguments without rerunning tools. messageIndex is the original zero-based session position; default 0. Optional query finds the next matching message from that position. offset/length page through its text. Returns nextMessageIndex for further searching. Provider encrypted reasoning is excluded.",
            """{"messageIndex":{"type":"integer","minimum":0},"offset":{"type":"integer","minimum":0},"length":{"type":"integer","minimum":1,"maximum":16384},"query":{"type":"string","maxLength":256}}"""),
        Define("inspect_existing_text", "Read the existing extraction baseline without re-extracting resources. Returns a bounded page with original IDs, locators and cache fields. Use resourcePath to inspect relevant existing support before a repair; preserve these fields so committed translations remain reusable.",
            """{"resourcePath":{"type":"string"},"offset":{"type":"integer","minimum":0,"maximum":1000000},"limit":{"type":"integer","minimum":1,"maximum":20}}""", []),
        Define("search_text", "Search a file byte range or a directory of text/source files. Returns at most 20 excerpts with byte offsets. File offset is bytes; directory offset is a file cursor, with fileByteOffset for resuming inside that file. Pass returned nextOffset and nextFileByteOffset until more=false. Directory scans are bounded to 64 text files and 4 MiB per call; binary archives are excluded. Source defaults to utf8; unknown game files to cp932.",
            """{"root":{"type":"string","enum":["game","workspace"]},"path":{"type":"string"},"query":{"type":"string"},"encoding":{"type":"string"},"offset":{"type":"integer"},"fileByteOffset":{"type":"integer","minimum":0}}""", "root", "path", "query"),
        Define("inspect_game_launch", "Refresh read-only launch evidence: native system ACP, per-EXE Locale Emulator profiles, bounded startup scripts and recent error-log excerpts. Does not run anything. Missing profiles do not prove locale independence; partial means incomplete inspection.", "{}", []),
        Define("inspect_pe_imports", "Read real PE imports and candidate x86/x64 indirect call/jump sites, with IAT RVAs and bounded hex context. query filters comma-separated names/DLLs. Pages default to 20, capped at 40; use nextOffset. Byte patterns are not decoded control flow or runtime proof. Read-only.",
            """{"root":{"type":"string","enum":["game","workspace"]},"path":{"type":"string"},"query":{"type":"string","maxLength":500},"offset":{"type":"integer","minimum":0,"maximum":8192},"limit":{"type":"integer","minimum":1,"maximum":40}}""", "root", "path"),
        Define("inspect_pe_code", "Disassemble 1-8 known code ranges in an x86/x64 PE using bundled Capstone. No scripts, Python setup or downloads. addressKind defaults to va (preferred image VA), also rva or file. Addresses are hex strings such as 0x429fb0 or decimal strings; each length is bytes, total <=16384. Returns a compact VA/bytes/instruction listing, per-range RVA/file mapping and nextAddress (VA); direct branch targets are in operands. Decoding starts exactly where requested; start alignment/reachability still need evidence. Read-only, never executes the game. Prefer this over writing one disassembly script per address.",
            """{"root":{"type":"string","enum":["game","workspace"]},"path":{"type":"string"},"addressKind":{"type":"string","enum":["va","rva","file"]},"ranges":{"type":"array","minItems":1,"maxItems":8,"items":{"type":"object","properties":{"address":{"type":"string"},"length":{"type":"integer","minimum":1,"maximum":4096}},"required":["address","length"],"additionalProperties":false}},"instructionLimit":{"type":"integer","minimum":1,"maximum":200}}""", "root", "path", "ranges"),
        Define("inspect_unity_schema", "Read-only Unity investigation. types (default): query comma-separated type names in IL2CPP v31 or managed/dummy .NET assemblies; returns fields, bases and text-method clues. literals: search visible text in the metadata literal table. references: supply matching x64 GameAssembly via assemblyPath, then queryKind=literal finds candidate consumers; queryKind=method pages through literals referenced by a display function/type and shows their other consumers. References are bounded byte-pattern candidates, not proof of display-only use or complete call graphs. No game execution, scripts, downloads or files. Use inspect_pe_code to verify code uses; don't infer serialized offsets from native layout or declarations.",
            """{"root":{"type":"string","enum":["game","workspace"]},"path":{"type":"string"},"mode":{"type":"string","enum":["types","literals","references"]},"assemblyPath":{"type":"string","description":"references only: matching Windows x64 GameAssembly path, relative to the same root; path must be its IL2CPP v31 metadata."},"queryKind":{"type":"string","enum":["literal","method"],"description":"references only; default literal. method matches a consumer function/type name to inspect the whole display group."},"query":{"type":"string","maxLength":256},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":8}}""", "root", "path"),
        Define("find_game_file", "Locate an exact reported relative path or filename in the game, returning same-name matches and whether the scan was partial. Use before concluding that a file/movie error means missing resources. Does not read or modify file contents.",
            """{"path":{"type":"string"}}""", "path"),
        Define("list_adapters", "List reusable published adapters and built-in script parsers, with saved engine candidates. Optional query filters names/formats. refreshCandidates rescans bounded local evidence; use after game resources change, not on every continuation.",
            """{"query":{"type":"string"},"refreshCandidates":{"type":"boolean"}}""", []),
        Define("inspect_builtin_adapter", "Inspect one bundled parser's capabilities, limitations, source index and reuse instructions. Supply path from its index to read a bounded source slice; offset/length count characters. Does not copy files or call a model.",
            """{"format":{"type":"string"},"path":{"type":"string"},"offset":{"type":"integer"},"length":{"type":"integer"}}""", "format"),
        Define("inspect_siglus_game", "Use bundled SiglusSceneScriptUtility on this game: returns scene/text counts, actual command argument references, template and embedded source index. source/query reads bounded embedded source without unpacking files. No model calls, translation, installation or game writes. Reuse copy_adapter engine.siglus for new display rules instead of writing another parser. Call alone.",
            """{"source":{"type":"string"},"query":{"type":"string"},"offset":{"type":"integer"},"length":{"type":"integer"}}""", []),
        Define("inspect_adapter_component", "Inspect one reusable text/encoding/font/launcher component from list_adapters.components: API, example, exact applicability and files. Optional path reads a bounded source slice (character offsets). Components are not complete adapters or proof of game compatibility.",
            """{"id":{"type":"string"},"path":{"type":"string"},"offset":{"type":"integer"},"length":{"type":"integer"}}""", "id"),
        Define("copy_adapter_components", "Copy 1-4 selected Python helpers or precompiled patch components without overwriting differing files. Font/encoding helpers call the precompiled application host; the QLIE launcher includes a ready-to-copy standalone EXE under src/tools. No C# compilation, font files or framework installation. Confirm applicability before use; test_adapter still required. Call alone.",
            """{"ids":{"type":"array","items":{"type":"string"}}}""", "ids"),
        Define("try_builtin_adapter", "Try a built-in parser on 1-12 explicit files under workspace resources/ or work/. Total input at most 64 MiB. Returns count, bounded samples, concrete parse diagnostics and optionally host-checked original-byte roundtrip (default true). Include required companions. Does not translate, test rendering, publish or activate. Stage original samples first; unpack archives locally into work/.",
            """{"format":{"type":"string"},"directory":{"type":"string"},"files":{"type":"array","items":{"type":"string"}},"checkRoundtrip":{"type":"boolean"}}""", "format", "directory", "files"),
        Define("adapter_protocol", "Get the exact runtime adapter protocol. No model request or external operation.", "{}", []),
        Define("copy_adapter", "Reuse an adapter in src. Published adapters copy source/dependencies; legacy C# source is reference material and must be ported to Python before a new release. Built-in IDs register the pinned parser and a Python helper calling the precompiled host. Existing differing files are not overwritten. Implement src/adapter.py, including bounded probe and container glue, then pass test_adapter before finish.",
            """{"id":{"type":"string"},"version":{"type":"string"}}""", "id", "version"),
        Define("search_engine_references", "Look up an engine, alias or format in the GalTransl index: returns scoped docs, module paths and public functions, NOT verified adapters. Uses the shared local library when prepared, otherwise online metadata. No game data is sent. Use inspect_engine_reference for local guides and run_engine_python to call existing algorithms instead of rewriting them.",
            """{"query":{"type":"string"}}""", "query"),
        Define("inspect_engine_reference", "Read one engine guide or a bounded source slice from the shared GalTransl Python toolkit. Uses the fixed source bundled with the app or already installed locally; no automatic download. Returns public function names, module paths and reuse instructions. engine is an exact ID from search_engine_references. Optional query/offset/length locate a relevant section. retryDownload retries local preparation after missing tool files are restored. Call alone.",
            """{"engine":{"type":"string"},"path":{"type":"string"},"query":{"type":"string"},"offset":{"type":"integer"},"length":{"type":"integer"},"retryDownload":{"type":"boolean"}}""", "engine"),
        Define("run_engine_python", "Run exactly one script (work/*.py or src/tools/*.py) OR module (python.engines.* / python.archives.* from the pinned library). module calls an existing CLI directly, e.g. arguments=[--help], without writing a wrapper. Use this for all temporary analysis scripts. Bundled portable Python, no downloads, system Python, pip or pycache. UTF-8 stdin/stdout/stderr; workingDirectory defaults to the workspace root. Use staged samples and concise output. Can follow write_file calls in one ordered batch.",
            """{"script":{"type":"string","description":"Existing work/*.py or src/tools/*.py. Omit when module is supplied."},"module":{"type":"string","description":"Exact pinned module name, such as python.engines.kirikiri_hxv4_text. Omit when script is supplied."},"arguments":{"type":"array","maxItems":128,"items":{"type":"string"}},"workingDirectory":{"type":"string"},"stdin":{"type":"string"},"timeoutSeconds":{"type":"integer","minimum":1,"maximum":600,"default":120}}""", []),
        Define("inspect_engine_component", "List fixed callable archive components or inspect one id. Returns supported formats, read/write operations, budgets and dependency contract. No downloads or source reading. Prefer run_engine_component over writing a Python wrapper for a supported format.",
            """{"id":{"type":"string"}}"""),
        Define("run_engine_component", "Directly inspect/extract/rebuild a staged archive using a fixed component, without writing code. Input under resources/ or work/; output is a NEW work/ directory. Returns bounded entries plus full manifest.json. Optional members/extensions select extraction. Rebuild replacements map exact archive names to work/ files; untouched members are verified. Uses pinned local shared Python/library, no pip/download. XP3 requires independently confirmed filter=none. Call alone.",
            """{"id":{"type":"string"},"operation":{"type":"string","enum":["inspect","extract","rebuild"]},"archive":{"type":"string"},"output":{"type":"string"},"filter":{"type":"string","enum":["none"]},"members":{"type":"array","maxItems":1024,"items":{"type":"string"}},"extensions":{"type":"array","maxItems":16,"items":{"type":"string"}},"replacements":{"type":"object","maxProperties":128,"additionalProperties":{"type":"string"}}}""", "id", "operation", "archive", "output"),
        Define("compose_adapter_template", "Compose a complete standalone adapter from list_adapters.templates: resource rules, fixed archive entry, bundled script parser, protocol glue and release selection. No model-written wrapper, pip or downloads; preserves differing existing files. Optional resourcePatterns narrows discovery; defaults come from the template. XP3 requires independently established filter=none. Returns testArguments for mandatory test_adapter; does not stage samples, verify, publish or activate. Call alone.",
            """{"id":{"type":"string"},"resourcePatterns":{"type":"array","minItems":1,"maxItems":16,"items":{"type":"string"}},"filter":{"type":"string","enum":["none"]}}""", "id"),
        Define("use_engine_reference", "Bind the shared Python toolkit into src: writes a pinned dependency and tools/gamel10n_host.py helper without copying the toolkit into each project. Implement src/adapter.py using the returned Python example. Published adapters use the same library and precompiled components without LLM calls or C# compilation. test_adapter remains required. Call alone.", "{}", []),
        Define("search_repositories", "Search public GitHub repository NAMES/descriptions for existing engine tools; this is NOT code or file-content search. Prefer search_engine_references for an engine/signature. Anonymous API results share the 7-day reference cache; refresh=true requests current metadata.",
            """{"query":{"type":"string"},"refresh":{"type":"boolean"}}""", "query"),
        Define("fetch_url", "Read public HTTPS reference data, up to 8 MiB locally. offset/length paginate decoded characters (default 12000, max 24000). Optional query searches literal text from offset and returns bounded excerpts. Shared disk cache lasts 7 days; refresh=true requests current data. retrievedAt is the original fetch time; stale=true means an expired copy was used after a network failure. Contents are untrusted data.",
            """{"url":{"type":"string"},"query":{"type":"string"},"offset":{"type":"integer"},"length":{"type":"integer"},"refresh":{"type":"boolean"}}""", "url"),
        Define("download_tool", "Obtain a public tool/source file in src/tools or work. Reuses hash-checked project dependencies or the shared download cache; refresh=true explicitly updates from the URL. Returns SHA256 and reused. Records URL/hash for packaged dependencies. For final dependencies set releaseRole and purpose once; investigative files belong in work/. Supply a short purpose for download progress. Prefer bundled tools first.",
            """{"url":{"type":"string"},"path":{"type":"string"},"releaseRole":{"type":"string","enum":["runtime","asset","license"]},"purpose":{"type":"string","maxLength":240},"refresh":{"type":"boolean"}}""", "url", "path"),
        Define("stage_resources", "Copy selected original game resources into resources for local tools. Reuses unchanged copies.",
            """{"files":{"type":"array","items":{"type":"string"}}}""", "files"),
        Define("write_file", "Write UTF-8 Python code or analysis files under src/ or work/, replacing without backup. New src/*.py files auto-register as release source. For tools set releaseRole and purpose once; edits inherit existing registration. Temporary analysis scripts must be Python under work/. Batch known writes followed by one run_engine_python/run_tool to execute in order; failure skips remaining calls. Reuse existing probe scripts and one findings file rather than adding a file per small check.",
            """{"path":{"type":"string"},"content":{"type":"string"},"releaseRole":{"type":"string","enum":["source","runtime","asset","license"]},"purpose":{"type":"string","maxLength":240}}""", "path", "content"),
        Define("replace_text", "Change one exact, unique text fragment in an existing UTF-8 src/ or work/ file. Prefer this for local edits instead of rewriting whole files. oldText must match exactly once (including whitespace/newlines); otherwise nothing changes. Preserves UTF-8 BOM and other bytes. Can batch with writes followed by one local run.",
            """{"path":{"type":"string"},"oldText":{"type":"string"},"newText":{"type":"string"}}""", "path", "oldText", "newText"),
        Define("remove_file", "Remove one obsolete file under src/ or work/. Cannot delete directories or originals.",
            """{"path":{"type":"string"}}""", "path"),
        Define("run_tool", "Run existing dotnet/powershell tools or a workspace executable. For analysis scripts use run_engine_python; do not compile C# probes. Pass arguments as an array. Use resources copies; never edit original game. Output bounded.",
            """{"executable":{"type":"string"},"arguments":{"type":"array","maxItems":128,"items":{"type":"string"}},"workingDirectory":{"type":"string"},"stdin":{"type":"string"},"timeoutSeconds":{"type":"integer","minimum":1,"maximum":600,"default":120}}""", "executable", "arguments"),
        Define("test_adapter", "Check Python syntax, package and verify the saved release selection with entry src/adapter.py. The pinned shared Python dependency is prepared automatically. Identical inputs reuse the last completed result (reused=true), including deterministic failures; change relevant implementation or evidence before retesting. Omit releaseFiles to reuse it; supplying releaseFiles replaces the selection. New src Python files and copied dependencies auto-register. Use update_release_files only for dependency changes or exclusions. Paths are FINAL PACKAGE paths: src/adapter.py maps to workspace src/adapter.py; tools/parser.py maps to workspace src/tools/parser.py. No globs or automatic directory inclusion. Exclude probes, samples, logs, unused tools and old implementations. Test this exact release before finish. Check discovery against real game/staged samples and independently verify extraction/roundtrip; repair also checks the existing baseline. discoveryOnly=true cannot publish. Optional recognition declares proven required file/signature facts and nonblocking hints; omit to retain it. It is independent of resourcePatterns. Do not invent game-specific filenames or research extra facts solely to fill it.",
            """{"kind":{"type":"string","enum":["engine","template"]},"key":{"type":"string"},"name":{"type":"string"},"resourcePatterns":{"type":"array","items":{"type":"string"}},"releaseFiles":{"type":"array","minItems":1,"maxItems":512,"items":{"type":"object","properties":{"path":{"type":"string","maxLength":512},"role":{"type":"string","enum":["source","runtime","asset","license"]},"purpose":{"type":"string","minLength":1,"maxLength":240}},"required":["path","role","purpose"],"additionalProperties":false}},"discoveryOnly":{"type":"boolean"},"recognition":""" + AdapterRecognition.JsonSchema + "}", "kind", "key", "name", "resourcePatterns"),
        Define("update_release_files", "Inspect or incrementally update the saved release selection. files upserts exact PACKAGE paths with source/runtime/asset/license roles and purposes; remove excludes paths without deleting files. No arguments lists selection and unregistered files. Use only for changed dependencies or exclusions; test_adapter reuses this state across failures/restarts/checkpoints. Game originals belong to resources, modified game files to patch output, never release assets.",
            """{"files":{"type":"array","maxItems":512,"items":{"type":"object","properties":{"path":{"type":"string"},"role":{"type":"string","enum":["source","runtime","asset","license"]},"purpose":{"type":"string","maxLength":240}},"required":["path","role","purpose"],"additionalProperties":false}},"remove":{"type":"array","maxItems":512,"items":{"type":"string"}}}"""),
        Define("finish", "Publish and activate the last successfully tested package. Checks source/dependency/resource contents still match verification. Reading files and updating work/ reports do not require retesting.",
            """{"summary":{"type":"string"}}""", "summary"),
        Define("report_blocker", "Stop for specific user input or an investigated obstacle. unsupported additionally requires canContinueLocally and evidence (concrete attempted approaches, results and evidence locations). canContinueLocally MUST be true if any actionable local investigation, implementation or validation remains; this returns continue, not stop. CP932 rejecting Chinese, unfinished runtime analysis, missing parser/tools or failed searches alone are unfinished work. Do not ask users to program the adapter. Call alone, concise Chinese.",
            """{"kind":{"type":"string","enum":["needs_input","unsupported"]},"summary":{"type":"string"},"reason":{"type":"string"},"nextAction":{"type":"string"},"canContinueLocally":{"type":"boolean"},"evidence":{"type":"string"}}""", "kind", "summary", "reason", "nextAction"),
        Define("checkpoint", "Compact when requested or a major phase ends, not after each probe. Prefer <=3000 characters: preserve exact field order/stride/API facts needed by the current implementation, evidence locations, failed hypotheses, coverage/omissions and next action. Do not copy source/disassembly. Recent small tool exchanges and the previously read adapter protocol survive. Call alone.",
            """{"summary":{"type":"string"}}""", "summary")
    ];

    public static bool CanBatch(string tool) => tool is "list_files" or "list_adapters" or "read_file" or "read_tool_result" or "read_session_history" or "search_text"
        or "inspect_engine_knowledge" or "inspect_existing_text" or "inspect_engine_component"
        or "find_game_file" or "inspect_pe_imports" or "inspect_pe_code" or "inspect_unity_schema" or "inspect_builtin_adapter" or "inspect_adapter_component" or "adapter_protocol" or "search_engine_references" or "search_repositories" or "fetch_url";

    // One ordered local edit/run step. Publication, staging and verification stay separate.
    internal static bool IsWriteRunBatch(IReadOnlyList<AgentToolCall> calls) => calls.Count is >= 2 and <= 8
        && calls.Take(calls.Count - 1).All(call => call.Name is "write_file" or "replace_text")
        && calls[^1].Name is "write_file" or "replace_text" or "run_engine_python" or "run_tool";

    public static string Stage(string tool) => tool switch
    {
        "load_tool_groups" => "准备分析工具",
        "report_progress" => "更新适配进展",
        "inspect_engine_knowledge" or "copy_engine_knowledge_tools" => "复用同类引擎经验",
        "list_files" or "read_file" or "search_text" or "stage_resources" or "find_game_file" or "inspect_existing_text" => "分析游戏资源",
        "inspect_game_launch" => "检查游戏启动环境",
        "inspect_pe_imports" => "分析程序导入与调用位置",
        "inspect_pe_code" => "分析程序代码与分支",
        "inspect_unity_schema" => "检查 Unity 类型与字段结构",
        "search_engine_references" or "search_repositories" or "fetch_url" or "download_tool" => "查找引擎工具",
        "inspect_engine_reference" or "use_engine_reference" => "准备共享引擎工具",
        "inspect_engine_component" or "run_engine_component" => "调用共享引擎组件",
        "compose_adapter_template" => "组合引擎适配模板",
        "list_adapters" or "copy_adapter" or "inspect_builtin_adapter" => "检查现有适配器",
        "inspect_adapter_component" or "copy_adapter_components" => "复用编码与运行组件",
        "try_builtin_adapter" => "验证内置解析能力",
        "inspect_siglus_game" => "分析 Siglus 显示规则",
        "update_release_files" => "整理适配器发布文件",
        "test_adapter" => "验证提取与回填",
        "finish" => "保存引擎适配器",
        "report_blocker" => "整理适配结果",
        _ => "开发引擎适配器"
    };
    public static string Description(string tool) => tool switch
    {
        "test_adapter" => "正在检查 Python 适配器、文本提取、原文回填和中文写入。",
        "finish" => "正在保存已通过资源验证的引擎适配器。",
        "run_tool" or "run_engine_python" => "正在运行本地资源分析工具。",
        "inspect_engine_reference" or "use_engine_reference" => "正在检查发行包中的共享引擎工具。",
        _ => Stage(tool) + "。"
    };

    public async Task<string> ExecuteAsync(AgentToolCall call, CancellationToken ct)
    {
        if (call.Arguments.Length > 512 * 1024) throw new InvalidDataException("工具参数过大。");
        using var document = JsonDocument.Parse(call.Arguments);
        var args = document.RootElement;
        if (args.ValueKind != JsonValueKind.Object) throw new InvalidDataException("工具参数必须为对象。");
        session.ReleaseFiles ??= session.VerifiedPackage?.ReleaseFiles;
        session.Recognition ??= session.VerifiedPackage?.Recognition;
        if (call.Name != "report_blocker") unresolvedStopReports = 0;
        object result = call.Name switch
        {
            "load_tool_groups" => AdaptationToolGroups.Load(session, args),
            "report_progress" => ReportProgress(args),
            "inspect_engine_knowledge" => InspectEngineKnowledge(args),
            "copy_engine_knowledge_tools" => await CopyEngineKnowledgeToolsAsync(args, ct),
            "list_files" => ListFiles(args),
            "read_file" => await ReadFileAsync(args, ct),
            "read_tool_result" => ReadToolResult(args),
            "read_session_history" => ReadSessionHistory(args),
            "inspect_existing_text" => ReadBaselineUnits(args),
            "search_text" => await SearchTextAsync(args, ct),
            "inspect_game_launch" => await InspectLaunchAsync(ct),
            "inspect_pe_imports" => await PeImportInspection.InspectAsync(ReadPath(args), Text(args, "query", ""),
                Number(args, "offset", 0, 0, 8192), Math.Min(Number(args, "limit", 20, 1, int.MaxValue), 40), ct),
            "inspect_pe_code" => await InspectPeCodeAsync(args, ct),
            "inspect_unity_schema" => await InspectUnityAsync(args, ct),
            "find_game_file" => GameLaunchInspector.FindFile(request.GameDirectory, Text(args, "path"), ct),
            "list_adapters" => await ListAdaptersAsync(args, ct),
            "inspect_builtin_adapter" => InspectBuiltinAdapter(args),
            "inspect_siglus_game" => await InspectSiglusGameAsync(args, ct),
            "inspect_adapter_component" => InspectAdapterComponent(args),
            "copy_adapter_components" => await CopyAdapterComponentsAsync(args, ct),
            "try_builtin_adapter" => await TryBuiltinAdapterAsync(args, ct),
            "adapter_protocol" => AdapterProtocol(),
            "copy_adapter" => await CopyAdapterAsync(args, ct),
            "search_engine_references" => await SearchEngineReferencesAsync(args, ct),
            "inspect_engine_reference" => await InspectEngineReferenceAsync(args, ct),
            "run_engine_python" => await RunEnginePythonAsync(args, ct),
            "inspect_engine_component" => EngineArchiveComponents.Describe(Text(args, "id", "") is { Length: > 0 } id ? id : null),
            "run_engine_component" => await RunEngineComponentAsync(args, ct),
            "compose_adapter_template" => await ComposeAdapterTemplateAsync(args, ct),
            "use_engine_reference" => await UseEngineReferenceAsync(args, ct),
            "search_repositories" => await SearchRepositoriesAsync(args, ct),
            "fetch_url" => await FetchAsync(args, ct),
            "download_tool" => await DownloadAsync(args, ct),
            "stage_resources" => await StageAsync(args, ct),
            "write_file" => await WriteFileAsync(args, ct),
            "replace_text" => await ReplaceTextAsync(args, ct),
            "remove_file" => RemoveFile(args),
            "run_tool" => await RunAsync(args, ct),
            "update_release_files" => UpdateReleaseFiles(args),
            "test_adapter" => await TestAsync(args, ct),
            "finish" => await FinishAsync(args, ct),
            "report_blocker" => ReportBlocker(args),
            "checkpoint" => Checkpoint(args),
            _ => throw new InvalidDataException("Unknown tool: " + call.Name)
        };
        return JsonSerializer.Serialize(new JsonObject
        {
            ["success"] = true,
            ["result"] = EngineJson.Node(result, Json)
        }, EngineJson.TypeInfo<JsonObject>(Json));
    }

    private object ReportProgress(JsonElement args)
    {
        var changed = session.UpdateProgress(AdaptationProgressNote.FromTool(Text(args, "kind"), Text(args, "message")));
        if (changed) reportProgress?.Invoke("更新适配进展", session.ProgressNote!.Message);
        return new JsonObject
        {
            ["changed"] = changed,
            ["note"] = EngineJson.Node(session.ProgressNote, Json),
            ["validation"] = false
        };
    }

    private async Task<GameLaunchInspection> InspectLaunchAsync(CancellationToken ct)
    {
        var inspection = await GameLaunchInspector.InspectAsync(request.GameDirectory, ct);
        if (JsonSerializer.Serialize(session.LaunchInspection, EngineJson.TypeInfo<GameLaunchInspection?>(Json)) != JsonSerializer.Serialize(inspection, EngineJson.TypeInfo<GameLaunchInspection>(Json)))
            session.VerifiedPackage = null;
        session.LaunchInspection = inspection;
        return inspection;
    }

    private Task<object> InspectPeCodeAsync(JsonElement args, CancellationToken ct)
    {
        if (!args.TryGetProperty("ranges", out var values) || values.ValueKind != JsonValueKind.Array || values.GetArrayLength() is < 1 or > 8)
            throw new InvalidDataException("ranges 必须包含 1–8 个代码范围。");
        var ranges = values.EnumerateArray().Select(value =>
        {
            if (value.ValueKind != JsonValueKind.Object) throw new InvalidDataException("代码范围必须包含 address 和 length。");
            var addressText = Text(value, "address").Trim();
            var hex = addressText.StartsWith("0x", StringComparison.OrdinalIgnoreCase);
            if (!ulong.TryParse(hex ? addressText.AsSpan(2) : addressText.AsSpan(),
                hex ? System.Globalization.NumberStyles.HexNumber : System.Globalization.NumberStyles.None,
                System.Globalization.CultureInfo.InvariantCulture, out var address))
                throw new InvalidDataException("address 必须是 0x 开头的十六进制或十进制地址字符串。");
            return new PeCodeRange(address, Number(value, "length", 256, 1, 4096));
        }).ToArray();
        return PeCodeInspection.InspectAsync(ReadPath(args), Text(args, "addressKind", "va"), ranges,
            Number(args, "instructionLimit", 80, 1, 200), ct, compact: true);
    }

    private object Checkpoint(JsonElement args)
    {
        var summary = Text(args, "summary");
        if (summary.Length is < 1 or > 8000) throw new InvalidDataException("进度摘要必须在 1 到 8000 字符之间。");
        UpdateCoverage(args);
        session.Checkpoint = summary;
        session.Notes = summary;
        return new JsonObject
        {
            ["saved"] = true,
            ["objective"] = session.Objective?.ToString(),
            ["nextAction"] = "已保存当前目标与验收记录。继续摘要中的下一项；完整验收已通过且目标完成时可直接 finish。"
        };
    }

    private void UpdateCoverage(JsonElement args)
    {
        var current = session.TextCoverage;
        if (args.TryGetProperty("coverageSampleCorrections", out var corrections))
        {
            var items = corrections.Deserialize<TextCoverageSampleCorrection[]>(EngineJson.TypeInfo<TextCoverageSampleCorrection[]>(Json))
                ?? throw new InvalidDataException("样本纠正不能为空。");
            if (items.Length is < 1 or > 4) throw new InvalidDataException("每次纠正 1 到 4 个明确误标的样本。");
            foreach (var correction in items) current = current.CorrectSample(correction);
        }
        var samples = current.Samples.ToDictionary(s => (s.ResourcePath.Replace('\\', '/'), s.Locator));
        if (args.TryGetProperty("coverageSamples", out var values))
            foreach (var sample in values.Deserialize<TextCoverageSample[]>(EngineJson.TypeInfo<TextCoverageSample[]>(Json)) ?? throw new InvalidDataException("原文样本不能为空。"))
            {
                new TextCoverage([sample], []).Validate();
                samples[(sample.ResourcePath.Replace('\\', '/'), sample.Locator)] = sample;
            }
        var pending = args.TryGetProperty("pendingCoverage", out _) ? Strings(args, "pendingCoverage") : current.Pending;
        var runtimeChecks = args.TryGetProperty("runtimeChecks", out _) ? Strings(args, "runtimeChecks") : current.RuntimeChecks;
        var updated = current with { Samples = samples.Values.ToArray(), Pending = pending, RuntimeChecks = runtimeChecks };
        updated.Validate();
        if (updated.Fingerprint() != session.TextCoverage.Fingerprint())
        {
            session.VerifiedPackage = null;
            session.VerifiedCoverageFingerprint = null;
        }
        session.TextCoverage = updated;
    }

    private object AdapterProtocol()
    {
        session.ProtocolRead = true;
        return new JsonObject
        {
            ["protocol"] = AutomaticAdaptationService.ProtocolDescription
        };
    }

    private object ReportBlocker(JsonElement args)
    {
        var kind = Text(args, "kind");
        if (kind is not ("needs_input" or "unsupported")) throw new InvalidDataException("无效的适配停止原因。");
        string Required(string name, int maximum)
        {
            var value = Text(args, name).Trim();
            if (value.Length < 1 || value.Length > maximum) throw new InvalidDataException(name + " 必须说明具体原因或操作，且不能过长。");
            return value;
        }
        var summary = Required("summary", 2000);
        var blocker = new AdaptationBlocker(kind == "needs_input", Required("reason", 2000), Required("nextAction", 2000));
        session.Summary = summary;
        if (kind == "unsupported")
        {
            var assessed = args.TryGetProperty("canContinueLocally", out var canContinue)
                && canContinue.ValueKind is JsonValueKind.True or JsonValueKind.False;
            var evidence = Text(args, "evidence", "").Trim();
            if (!assessed || canContinue.ValueKind == JsonValueKind.True || evidence.Length is < 1 or > 2000)
            {
                // Bound repeated stop requests without any intervening work. Do not relabel unfinished work as unsupported.
                if (++unresolvedStopReports >= 2)
                {
                    session.Blocker = blocker with { WorkRemaining = true };
                    return new JsonObject
                    {
                        ["stopped"] = true,
                        ["status"] = "incomplete",
                        ["reason"] = "模型连续请求停止，但未提供充分的停止说明。已保存未完成步骤，可继续适配。"
                    };
                }
                return new JsonObject
                {
                    ["stopped"] = false,
                    ["status"] = "continue",
                    ["nextAction"] = blocker.NextAction,
                    ["reason"] = "尚有本地可执行工作，或缺少停止依据。请执行下一项具体工具操作；未验证、未实现、CP932 不支持中文以及未找到现成工具都不是不可适配的结论。确实无法继续时，补充 canContinueLocally=false 和具体尝试及结果 evidence。无需发布未经验证的补丁。"
                };
            }
        }
        session.Blocker = blocker;
        return new JsonObject
        {
            ["stopped"] = true
        };
    }

    private object ListFiles(JsonElement args)
    {
        var root = Root(args);
        var directory = Text(args, "path", "");
        if (directory.Length > 0 && directory != ".") directory = WorkspaceFiles.Resolve(root, directory);
        else directory = root;
        if (!Directory.Exists(directory)) return new JsonObject
        {
            ["files"] = EngineJson.Node(Array.Empty<object>(), Json),
            ["more"] = false
        };
        var offset = Number(args, "offset", 0, 0, 1_000_000);
        var limit = Number(args, "limit", 100, 1, 200);
        var files = Directory.EnumerateFiles(directory, "*", new EnumerationOptions
        { RecurseSubdirectories = true, AttributesToSkip = FileAttributes.ReparsePoint, IgnoreInaccessible = true })
            .Skip(offset).Take(limit + 1).ToArray();
        return new JsonObject
        {
            ["files"] = EngineJson.Array(files.Take(limit).Select(path => new JsonObject
        {
            ["path"] = Path.GetRelativePath(root, path).Replace('\\', '/'),
            ["bytes"] = new FileInfo(path).Length
        }).ToArray()),
            ["more"] = files.Length > limit,
            ["nextOffset"] = offset + Math.Min(files.Length, limit)
        };
    }

    private object ReadToolResult(JsonElement args)
    {
        var id = Text(args, "toolCallId");
        var result = session.Messages.LastOrDefault(message => message.Role == "tool" && message.ToolCallId == id)?.Content
                ?? throw new InvalidDataException("会话中没有此工具结果，请检查 toolCallId 或根据进度摘要读取对应的证据文件。");
        var offset = Number(args, "offset", 0, 0, result.Length);
        var length = Number(args, "length", 4096, 1, 16 * 1024);
        var query = Text(args, "query", "");
        if (query.Length > 256) throw new InvalidDataException("查询内容过长。");
        if (query.Length > 0)
        {
            var found = result.IndexOf(query, offset, StringComparison.OrdinalIgnoreCase);
            if (found < 0) return new JsonObject
            {
                ["toolCallId"] = id,
                ["found"] = false,
                ["totalCharacters"] = result.Length
            };
            offset = Math.Max(offset, found - 160);
        }
        length = Math.Min(length, result.Length - offset);
        return new JsonObject
        {
            ["toolCallId"] = id,
            ["offset"] = offset,
            ["length"] = length,
            ["totalCharacters"] = result.Length,
            ["more"] = offset + length < result.Length,
            ["content"] = result.Substring(offset, length)
        };
    }

    private object ReadSessionHistory(JsonElement args)
    {
        var index = Number(args, "messageIndex", 0, 0, session.Messages.Count);
        var query = Text(args, "query", "");
        if (query.Length > 256) throw new InvalidDataException("查询内容过长。");
        for (; index < session.Messages.Count; index++)
        {
            var message = session.Messages[index];
            var content = AdaptationHistory.ReadableMessage(message);
            var found = query.Length == 0 ? 0 : content.IndexOf(query, StringComparison.OrdinalIgnoreCase);
            if (found < 0) continue;
            var offset = Number(args, "offset", Math.Max(0, found - 160), 0, content.Length);
            var length = Math.Min(Number(args, "length", 4096, 1, 16 * 1024), content.Length - offset);
            return new JsonObject
            {
                ["found"] = true,
                ["messageIndex"] = index,
                ["nextMessageIndex"] = index + 1,
                ["role"] = message.Role,
                ["toolCallId"] = message.ToolCallId,
                ["offset"] = offset,
                ["length"] = length,
                ["totalCharacters"] = content.Length,
                ["more"] = offset + length < content.Length,
                ["content"] = content.Substring(offset, length)
            };
        }
        return new JsonObject
        {
            ["found"] = false,
            ["nextMessageIndex"] = index,
            ["totalMessages"] = session.Messages.Count
        };
    }

    private async Task<object> ReadFileAsync(JsonElement args, CancellationToken ct)
    {
        var path = ReadPath(args);
        await using var stream = File.OpenRead(path);
        var offset = LongNumber(args, "offset", 0, 0, stream.Length);
        var length = (int)Math.Min(Number(args, "length", 8192, 1, 32 * 1024), stream.Length - offset);
        stream.Position = offset;
        var data = new byte[length];
        await stream.ReadExactlyAsync(data, ct);
        var encoding = Text(args, "encoding", IsWorkspaceText(args, path) ? "utf8" : "hex");
        return new JsonObject
        {
            ["bytes"] = stream.Length,
            ["offset"] = offset,
            ["length"] = length,
            ["encoding"] = encoding,
            ["more"] = offset + length < stream.Length,
            ["content"] = encoding == "hex" ? Convert.ToHexString(data) : EncodingFor(encoding).GetString(data)
        };
    }

    private async Task<object> SearchTextAsync(JsonElement args, CancellationToken ct)
    {
        var query = Text(args, "query");
        if (query.Length is < 1 or > 256) throw new InvalidDataException("搜索文本长度必须在 1 到 256 之间。");
        var path = Text(args, "path") is "" or "." ? Root(args) : ReadPath(args);
        WorkspaceFiles.RejectLinkAncestors(path);
        if (Directory.Exists(path)) return await SearchDirectoryTextAsync(args, path, query, ct);
        return await SearchFileTextAsync(path, query, Text(args, "encoding", IsWorkspaceText(args, path) ? "utf8" : "cp932"),
            LongNumber(args, "offset", 0, 0, long.MaxValue), 4 * 1024 * 1024, 20, ct);
    }

    internal sealed record TextSearchPage(IReadOnlyList<string> Matches, IReadOnlyList<long?> ByteOffsets,
        string Encoding, int ScannedBytes, long NextOffset, bool More);

    private static async Task<TextSearchPage> SearchFileTextAsync(string path, string query, string encodingName,
        long offset, int maximumBytes, int maximumMatches, CancellationToken ct)
    {
        await using var stream = File.OpenRead(path);
        if (offset > stream.Length) throw new InvalidDataException("搜索偏移超出文件长度。");
        stream.Position = offset;
        var bytes = new byte[(int)Math.Min(maximumBytes, stream.Length - offset)];
        await stream.ReadExactlyAsync(bytes, ct);
        var encoding = EncodingFor(encodingName);
        var content = encoding.GetString(bytes);
        // A lossy decode (or a slice ending halfway through a character) cannot provide reliable
        // byte offsets by re-encoding the decoded prefix. Keep excerpts, but mark offsets unknown.
        var exactOffsets = encoding.GetBytes(content).AsSpan().SequenceEqual(bytes);
        var matches = new List<string>();
        var byteOffsets = new List<long?>();
        var position = 0;
        while (matches.Count < maximumMatches && (position = content.IndexOf(query, position, StringComparison.OrdinalIgnoreCase)) >= 0)
        {
            var start = Math.Max(0, position - 80);
            matches.Add(content.Substring(start, Math.Min(content.Length - start, query.Length + 160)));
            byteOffsets.Add(exactOffsets ? offset + encoding.GetByteCount(content.AsSpan(0, position)) : null);
            position += query.Length;
        }
        var consumed = matches.Count == maximumMatches && exactOffsets ? encoding.GetByteCount(content.AsSpan(0, position)) : bytes.Length;
        return new(matches, byteOffsets, encodingName, bytes.Length, offset + consumed, offset + consumed < stream.Length);
    }

    private async Task<object> SearchDirectoryTextAsync(JsonElement args, string directory, string query, CancellationToken ct)
    {
        var offset = Number(args, "offset", 0, 0, 1_000_000);
        var fileByteOffset = LongNumber(args, "fileByteOffset", 0, 0, long.MaxValue);
        var files = Directory.EnumerateFiles(directory, "*", new EnumerationOptions
        { RecurseSubdirectories = true, AttributesToSkip = FileAttributes.ReparsePoint, IgnoreInaccessible = true })
            .Where(path => !Path.GetFileName(path).StartsWith(".env", StringComparison.OrdinalIgnoreCase)
                && Path.GetExtension(path).ToLowerInvariant() is ".cs" or ".py" or ".ps1" or ".md" or ".txt"
                    or ".json" or ".jsonl" or ".xml" or ".csv" or ".log" or ".ini" or ".cfg" or ".ks" or ".rpy"
                    or ".csproj" or ".rsp" or ".c" or ".h" or ".cpp" or ".hpp" or ".yaml" or ".yml" or ".toml")
            .Skip(offset).Take(65).ToArray();
        var matches = new List<object>();
        var scannedBytes = 0;
        var scannedFiles = 0;
        var nextOffset = offset;
        long nextFileByteOffset = fileByteOffset;
        foreach (var file in files.Take(64))
        {
            ct.ThrowIfCancellationRequested();
            if (matches.Count >= 20 || scannedBytes >= 4 * 1024 * 1024) break;
            var relative = Path.GetRelativePath(Root(args), file).Replace('\\', '/');
            WorkspaceFiles.Resolve(Root(args), relative);
            var encoding = Text(args, "encoding", IsWorkspaceText(args, file) ? "utf8" : "cp932");
            var result = await SearchFileTextAsync(file, query, encoding, nextFileByteOffset,
                Math.Min(256 * 1024, 4 * 1024 * 1024 - scannedBytes), 20 - matches.Count, ct);
            scannedFiles++;
            scannedBytes += result.ScannedBytes;
            matches.AddRange(result.Matches.Select((excerpt, index) => (object)new JsonObject
            {
                ["path"] = relative,
                ["byteOffset"] = result.ByteOffsets[index],
                ["encoding"] = encoding,
                ["excerpt"] = excerpt
            }));
            if (result.More) { nextFileByteOffset = result.NextOffset; break; }
            nextOffset++;
            nextFileByteOffset = 0;
        }
        return new JsonObject
        {
            ["matches"] = EngineJson.Node((matches).ToArray(), Json),
            ["scannedFiles"] = scannedFiles,
            ["scannedBytes"] = scannedBytes,
            ["nextOffset"] = nextOffset,
            ["nextFileByteOffset"] = nextFileByteOffset,
            ["more"] = nextFileByteOffset > 0 || nextOffset - offset < files.Length
        };
    }

    private bool IsWorkspaceText(JsonElement args, string path) => Path.GetExtension(path).ToLowerInvariant() is
        ".cs" or ".py" or ".ps1" or ".md" or ".json" or ".csproj" or ".rsp"
        || Text(args, "root") == "workspace" && Path.GetRelativePath(Root(args), path).Replace('\\', '/').StartsWith("work/", StringComparison.OrdinalIgnoreCase)
            && Path.GetExtension(path).ToLowerInvariant() is ".txt" or ".log" or ".csv" or ".jsonl";

    private void InvalidateSource(string path)
    {
        if (!path.StartsWith(Path.GetFullPath(SourceDirectory) + Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase)) return;
        if (session.VerifiedPackage is { ManifestVersion: >= 3, ReleaseFiles: { } files }
            && !files.Any(file => AdapterReleaseFiles.SourcePath(SourceDirectory, file).Equals(path, StringComparison.OrdinalIgnoreCase))) return;
        session.VerifiedPackage = null;
    }

    private async Task<object> StageAsync(JsonElement args, CancellationToken ct)
    {
        var files = Strings(args, "files");
        if (files.Count is < 1 or > 256) throw new InvalidDataException("每次选择 1 到 256 个资源文件。");
        foreach (var relative in files)
        {
            ct.ThrowIfCancellationRequested();
            var source = WorkspaceFiles.Resolve(request.GameDirectory, relative);
            var target = WorkspaceFiles.Resolve(ResourceDirectory, relative);
            var hash = await LocalFiles.HashFileAsync(source, ct);
            if (!session.ResourceHashes.TryGetValue(relative.Replace('\\', '/'), out var previousHash) || previousHash != hash)
                session.VerifiedPackage = null;
            if (!File.Exists(target) || await LocalFiles.HashFileAsync(target, ct) != hash)
            {
                Directory.CreateDirectory(Path.GetDirectoryName(target)!);
                await using var input = new FileStream(source, FileMode.Open, FileAccess.Read, FileShare.Read, 128 * 1024, true);
                await using var output = new FileStream(target, FileMode.Create, FileAccess.Write, FileShare.None, 128 * 1024, true);
                await input.CopyToAsync(output, ct);
            }
            session.ResourceHashes[relative.Replace('\\', '/')] = hash;
        }
        return new JsonObject
        {
            ["staged"] = EngineJson.Node((files).ToArray(), Json),
            ["directory"] = "resources"
        };
    }

    private async Task<object> ReplaceTextAsync(JsonElement args, CancellationToken ct)
    {
        var relative = Text(args, "path");
        var path = WritePath(relative);
        if (new FileInfo(path).Length > 512 * 1024) throw new InvalidDataException("局部编辑文件不得超过 512 KiB。");
        var bytes = await File.ReadAllBytesAsync(path, ct);
        var hasBom = bytes.AsSpan().StartsWith(Encoding.UTF8.GetPreamble());
        var content = new UTF8Encoding(false, true).GetString(bytes.AsSpan(hasBom ? 3 : 0));
        var oldText = Text(args, "oldText");
        var newText = Text(args, "newText");
        if (oldText.Length == 0) throw new InvalidDataException("oldText 不能为空，请提供需要替换的唯一原文片段。");
        var start = content.IndexOf(oldText, StringComparison.Ordinal);
        if (start < 0) throw new InvalidDataException("未找到完全匹配的 oldText，文件未修改。请读取相关片段后重试。");
        if (content.IndexOf(oldText, start + 1, StringComparison.Ordinal) >= 0)
            throw new InvalidDataException("oldText 匹配多处，文件未修改。请增加上下文，使其仅匹配一次。");
        var updated = (hasBom ? "\uFEFF" : "") + content[..start] + newText + content[(start + oldText.Length)..];
        return await WriteFileAsync(JsonSerializer.SerializeToElement(new JsonObject
        {
            ["path"] = relative,
            ["content"] = updated
        }, EngineJson.TypeInfo<JsonObject>(EngineJson.Default)), ct);
    }

    private async Task<object> WriteFileAsync(JsonElement args, CancellationToken ct)
    {
        var relative = Text(args, "path").Replace('\\', '/');
        if (relative.EndsWith(".cs", StringComparison.OrdinalIgnoreCase) || relative.EndsWith(".csproj", StringComparison.OrdinalIgnoreCase))
            throw new InvalidDataException("智能分析和正式适配器只生成 Python；已有 C# 仅供参考或按旧包运行。请使用 work/*.py 或 src/adapter.py。");
        var path = WritePath(relative);
        var isNew = !File.Exists(path);
        var release = DeclaredReleaseFile(args, relative);
        var content = Text(args, "content");
        var bytes = Encoding.UTF8.GetBytes(content);
        if (bytes.Length > 512 * 1024) throw new InvalidDataException("单次写入文件过大。");
        if (File.Exists(path) && new FileInfo(path).Length == bytes.Length
            && (await File.ReadAllBytesAsync(path, ct)).AsSpan().SequenceEqual(bytes))
        {
            if (release is not null) Development.RegisterReleaseFiles([release]);
            return new JsonObject
            {
                ["path"] = Text(args, "path"),
                ["bytes"] = bytes.Length,
                ["changed"] = false
            };
        }
        InvalidateSource(path);
        Directory.CreateDirectory(Path.GetDirectoryName(path)!);
        var temporary = path + ".write.tmp";
        try
        {
            await File.WriteAllTextAsync(temporary, content, new UTF8Encoding(false), ct);
            ct.ThrowIfCancellationRequested();
            File.Move(temporary, path, true);
        }
        finally { if (File.Exists(temporary)) File.Delete(temporary); }
        if (path.StartsWith(Path.GetFullPath(SourceDirectory) + Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase))
            session.LastImplementationTurn = session.TotalTurns;
        if (release is not null) Development.RegisterReleaseFiles([release]);
        else if (isNew && relative.StartsWith("src/", StringComparison.Ordinal)
            && !relative.StartsWith("src/tools/", StringComparison.OrdinalIgnoreCase)
            && relative.EndsWith(".py", StringComparison.OrdinalIgnoreCase))
            Development.RegisterReleaseFiles([new(relative, "source", "适配器实现")]);
        return new JsonObject
        {
            ["path"] = Text(args, "path"),
            ["bytes"] = bytes.Length,
            ["changed"] = true
        };
    }

    private object RemoveFile(JsonElement args)
    {
        var path = WritePath(Text(args, "path"));
        InvalidateSource(path);
        File.Delete(path);
        var packagedPath = AdapterReleaseFiles.PackagePath(Text(args, "path").Replace('\\', '/'));
        session.ReleaseFiles = session.ReleaseFiles?.Where(file => !file.Path.Equals(packagedPath, StringComparison.OrdinalIgnoreCase)).ToArray();
        return new JsonObject
        {
            ["removed"] = Text(args, "path")
        };
    }

    private async Task<object> CopyAdapterAsync(JsonElement args, CancellationToken ct)
    {
        if (Text(args, "id") == SiglusAssets.Family.AdapterId && Text(args, "version") == SiglusAssets.Version)
        {
            session.VerifiedPackage = null;
            return await CopySiglusAsync(ct);
        }
        var builtin = VnTextPatchFormatCatalog.All.FirstOrDefault(format => format.Family.AdapterId == Text(args, "id"));
        if (builtin is not null && Text(args, "version") == VnTextPatchFormatCatalog.Version)
        {
            session.VerifiedPackage = null;
            var copied = new List<AdapterReleaseFile>();
            var result = await BuiltinAssets.CopyAsync(builtin.Key, workspace, Boolean(args, "editableSource", false), ct, copied);
            Development.RegisterReleaseFiles(copied);
            await EngineToolkit.ResolveVnTextPatchAsync(SourceDirectory, ct, BuiltinAssets);
            return result;
        }
        var pinned = store.LoadActive(request.GameId);
        var package = pinned?.Id == Text(args, "id") && pinned.Version == Text(args, "version") ? pinned : null;
        if (package is null)
        {
            var candidate = (await store.ReadDiscoveryCandidatesAsync(ct: ct)).SingleOrDefault(item =>
                item.Id == Text(args, "id") && item.Version == Text(args, "version"))
                ?? throw new InvalidDataException("未找到此版本的适配器。");
            package = store.LoadVersion(candidate.Id, candidate.Version);
        }
        if (Directory.Exists(SourceDirectory) && Directory.EnumerateFileSystemEntries(SourceDirectory).Any())
            throw new InvalidDataException("src 已包含代码。请先检查现有文件，避免覆盖已有开发成果。");
        session.VerifiedPackage = null;
        session.Recognition = package.Recognition;
        var source = Directory.Exists(Path.Combine(package.DirectoryPath, "src"))
            ? Path.Combine(package.DirectoryPath, "src") : package.DirectoryPath;
        foreach (var file in Directory.EnumerateFiles(source, "*", SearchOption.AllDirectories))
        {
            var relative = Path.GetRelativePath(source, file);
            if (!relative.EndsWith(".cs", StringComparison.OrdinalIgnoreCase) && !relative.EndsWith(".py", StringComparison.OrdinalIgnoreCase) && !relative.Replace('\\', '/').StartsWith("tools/", StringComparison.Ordinal)) continue;
            var target = WorkspaceFiles.Resolve(SourceDirectory, relative);
            WorkspaceFiles.RejectLinkAncestors(file);
            Directory.CreateDirectory(Path.GetDirectoryName(target)!);
            await using var input = File.OpenRead(file);
            await using var output = File.Create(target);
            await input.CopyToAsync(output, ct);
        }
        var dependencies = Path.Combine(package.DirectoryPath, "tools");
        if (Directory.Exists(dependencies))
            foreach (var file in Directory.EnumerateFiles(dependencies, "*", SearchOption.AllDirectories))
            {
                var target = WorkspaceFiles.Resolve(SourceDirectory, "tools/" + Path.GetRelativePath(dependencies, file).Replace('\\', '/'));
                WorkspaceFiles.RejectLinkAncestors(file);
                Directory.CreateDirectory(Path.GetDirectoryName(target)!);
                await using var input = File.OpenRead(file);
                await using var output = File.Create(target);
                await input.CopyToAsync(output, ct);
            }
        session.ReleaseFiles = package.ReleaseFiles?.ToArray();
        return new JsonObject
        {
            ["id"] = package.Id,
            ["version"] = package.Version,
            ["destination"] = "src",
            ["releaseFiles"] = EngineJson.Node((package.ReleaseFiles)?.ToArray(), Json),
            ["nextAction"] = package.IsPython
                ? "沿用已声明文件，按本次实现补充或移除；不要将所有 tools 文件默认发布。"
                : "旧包继续原样运行；新开发入口使用 src/adapter.py，将所需 C# 逻辑迁移为 Python，并从发布清单排除旧源码后重新验收。旧包无 releaseFiles 时显式选择所需文件，不要将所有 tools 文件默认发布。"
        };
    }

    private async Task<object> RunAsync(JsonElement args, CancellationToken ct)
    {
        var executable = Text(args, "executable");
        var arguments = Strings(args, "arguments").ToList();
        if (arguments.Count > 128 || arguments.Sum(value => value.Length) > 64 * 1024)
            throw new InvalidDataException("工具命令参数过大。");
        executable = executable switch
        {
            "dotnet" => runtime.Toolchain.ExecutablePath,
            "powershell" => Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.Windows), "System32", "WindowsPowerShell", "v1.0", "powershell.exe"),
            _ => WorkspaceFiles.Resolve(workspace, executable)
        };
        if (Text(args, "executable") == "powershell")
            arguments.InsertRange(0, ["-NoLogo", "-NoProfile", "-NonInteractive"]);
        var working = Text(args, "workingDirectory", ".");
        working = working == "." ? workspace : WorkspaceFiles.Resolve(workspace, working);
        Directory.CreateDirectory(working);
        var result = await runtime.Runner.RunAsync(new(executable, arguments, working,
            Text(args, "stdin", ""), TimeSpan.FromSeconds(Number(args, "timeoutSeconds", 120, 1, 600)), 1024 * 1024,
            VnTextPatchAssemblyPath: await AnalysisParserPathAsync(ct)), ct);
        return new JsonObject
        {
            ["exitCode"] = result.ExitCode,
            ["stdout"] = Limit(result.StandardOutput, 16_000),
            ["stderr"] = Limit(result.StandardError, 8000)
        };
    }

    private async Task<object> TestAsync(JsonElement args, CancellationToken ct)
    {
        UpdateCoverage(args);
        if (args.TryGetProperty("recognition", out var recognitionDeclaration))
            session.Recognition = recognitionDeclaration.ValueKind == JsonValueKind.Null ? null
                : recognitionDeclaration.Deserialize<AdapterRecognition>(EngineJson.TypeInfo<AdapterRecognition>(Json));
        var releaseFiles = args.TryGetProperty("releaseFiles", out var declared)
            ? AdapterReleaseFiles.Validate(declared.Deserialize<AdapterReleaseFile[]>(EngineJson.TypeInfo<AdapterReleaseFile[]>(Json)))
            : session.ReleaseFiles;
        session.ReleaseFiles = releaseFiles;
        var result = await Development.TestAsync(new(Text(args, "kind"), Text(args, "key"), Text(args, "name")),
            Strings(args, "resourcePatterns"), Boolean(args, "discoveryOnly", false), ct);
        var nextAction = result.Stage == AdapterTestStage.Discovery
            ? result.Passed ? "继续实现并执行完整 test_adapter；仅资源发现通过还不能发布。"
                : "修复实际资源发现失败后重新 test_adapter；不要绕过失败发布。"
            : !result.Passed ? "修复 diagnostics 中的真实失败后重新 test_adapter；未通过验收必须继续修复。"
            : session.Objective == AdaptationObjective.Repair
                ? "本次资源与已有文本验收已通过。若反馈已解决，直接 finish；不要自动开始补全其他文字类别。已知其他覆盖遗漏用 allowPartial=true 并在 summary 披露，运行效果待验证单列 runtimeChecks。长会话可先 checkpoint 保存本阶段。"
                : "完整资源验收已通过。当前目标完成后直接 finish，披露已知遗漏和 runtimeChecks；长会话可先 checkpoint。不要重复通过的测试或为未调查类别自动增加研究。";
        return new JsonObject
        {
            ["passed"] = result.Passed,
            ["stage"] = EngineJson.Node(result.Stage, Json),
            ["diagnostics"] = EngineJson.Node((result.Diagnostics).ToArray(), Json),
            ["id"] = result.Id,
            ["version"] = result.Version,
            ["reused"] = result.Reused,
            ["compileMs"] = result.CompileMs,
            ["discoveryMs"] = result.DiscoveryMs,
            ["verificationMs"] = result.VerificationMs,
            ["nextAction"] = nextAction,
            ["runtimeTested"] = false,
            ["pendingCoverage"] = EngineJson.Node((session.TextCoverage.Pending).ToArray(), Json),
            ["runtimeChecks"] = EngineJson.Node((session.TextCoverage.RuntimeChecks).ToArray(), Json)
        };
    }

    private async Task<object> FinishAsync(JsonElement args, CancellationToken ct)
    {
        var published = await Development.PublishAsync(Text(args, "summary"), Boolean(args, "allowPartial", false), ct);
        return new JsonObject
        {
            ["id"] = published.Id,
            ["version"] = published.Version,
            ["summary"] = session.Summary,
            ["runtimeTested"] = false,
            ["coverageStatus"] = session.TextCoverage.Pending.Count > 0 ? "partial" : session.TextCoverage.Samples.Count > 0 ? "samples-verified" : "not-assessed",
            ["pendingCoverage"] = EngineJson.Node((session.TextCoverage.Pending).ToArray(), Json),
            ["runtimeChecks"] = EngineJson.Node((session.TextCoverage.RuntimeChecks).ToArray(), Json)
        };
    }

    private async Task<object> SearchRepositoriesAsync(JsonElement args, CancellationToken ct)
    {
        var query = Text(args, "query");
        if (query.Length > 512) throw new InvalidDataException("检索条件过长。");
        var document = await ReadResearchAsync("https://api.github.com/search/repositories?per_page=8&q=" + Uri.EscapeDataString(query),
            ct, Boolean(args, "refresh", false), 256 * 1024);
        using var json = JsonDocument.Parse(document.Content);
        var repositories = json.RootElement.GetProperty("items").EnumerateArray().Select(item => new JsonObject
        {
            ["name"] = item.GetProperty("full_name").GetString(),
            ["url"] = item.GetProperty("html_url").GetString(),
            ["description"] = item.GetProperty("description").GetString(),
            ["license"] = item.TryGetProperty("license", out var license) && license.ValueKind == JsonValueKind.Object
                ? license.GetProperty("spdx_id").GetString() : null
        }).ToArray();
        return new JsonObject
        {
            ["retrievedAt"] = document.RetrievedAt,
            ["reused"] = document.Reused,
            ["stale"] = document.Stale,
            ["repositories"] = EngineJson.Array(repositories)
        };
    }

    private async Task<HttpResponseMessage> GetAsync(Uri uri, CancellationToken ct)
    {
        for (var redirect = 0; redirect < 6; redirect++)
        {
            ValidatePublicUri(uri);
            using var requestMessage = new HttpRequestMessage(HttpMethod.Get, uri);
            requestMessage.Headers.UserAgent.ParseAdd("GameL10n/1.0");
            requestMessage.Headers.Accept.Add(new MediaTypeWithQualityHeaderValue("application/json"));
            var response = await (researchClient ?? PublicHttp).SendAsync(requestMessage, HttpCompletionOption.ResponseHeadersRead, ct);
            if ((int)response.StatusCode is >= 300 and < 400 && response.Headers.Location is { } location)
            { uri = new Uri(uri, location); response.Dispose(); continue; }
            if (!response.IsSuccessStatusCode)
            {
                var status = (int)response.StatusCode;
                response.Dispose();
                throw new HttpRequestException($"公开资料请求失败（HTTP {status}）。");
            }
            return response;
        }
        throw new HttpRequestException("资料地址重定向过多。");
    }

    private string Root(JsonElement args) => Text(args, "root") switch
    { "game" => request.GameDirectory, "workspace" => workspace, _ => throw new InvalidDataException("root 必须是 game 或 workspace。") };
    private Task<object> InspectUnityAsync(JsonElement args, CancellationToken ct)
    {
        string mode = Text(args, "mode", "types");
        return mode == "references"
            ? UnityTextReferenceInspection.InspectAsync(ReadPath(args), ReadPath(args, "assemblyPath"), Text(args, "query"),
                Text(args, "queryKind", "literal"), Number(args, "offset", 0, 0, 1_000_000), Number(args, "limit", 4, 1, 8), ct)
            : UnitySchemaInspection.InspectAsync(ReadPath(args), Text(args, "query", ""), Number(args, "offset", 0, 0, 1_000_000),
                Number(args, "limit", 4, 1, 8), ct, mode);
    }

    private string ReadPath(JsonElement args, string property = "path")
    {
        var path = WorkspaceFiles.Resolve(Root(args), Text(args, property));
        if (Path.GetExtension(path).ToLowerInvariant() is ".key" or ".pem" or ".pfx" || Path.GetFileName(path).StartsWith(".env", StringComparison.OrdinalIgnoreCase))
            throw new InvalidDataException("不能读取凭据文件。");
        return path;
    }
    private string WritePath(string relative)
    {
        relative = relative.Replace('\\', '/');
        if (!relative.StartsWith("src/", StringComparison.Ordinal) && !relative.StartsWith("work/", StringComparison.Ordinal))
            throw new InvalidDataException("只能修改 src/ 或 work/ 中的文件。");
        return WorkspaceFiles.Resolve(workspace, relative);
    }
    private static Encoding EncodingFor(string name)
    {
        Encoding.RegisterProvider(CodePagesEncodingProvider.Instance);
        return name switch { "utf8" => Encoding.UTF8, "cp932" => Encoding.GetEncoding(932), "cp936" => Encoding.GetEncoding(936),
            _ => throw new InvalidDataException("encoding 必须是 hex、utf8、cp932 或 cp936。") };
    }
    private static string Text(JsonElement args, string name, string? fallback = null) =>
        args.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.String ? value.GetString()!
            : fallback ?? throw new InvalidDataException("Missing string argument: " + name);
    private static List<string> Strings(JsonElement args, string name)
    {
        if (!args.TryGetProperty(name, out var value) || value.ValueKind != JsonValueKind.Array || value.GetArrayLength() > 1024)
            throw new InvalidDataException("Invalid array argument: " + name);
        return value.EnumerateArray().Select(item => item.ValueKind == JsonValueKind.String ? item.GetString()!
            : throw new InvalidDataException("Array must contain strings: " + name)).ToList();
    }
    private static long LongNumber(JsonElement args, string name, long fallback, long minimum, long maximum)
    {
        var result = fallback;
        if (args.TryGetProperty(name, out var value)
            && (value.ValueKind != JsonValueKind.Number || !value.TryGetInt64(out result)))
            throw new InvalidDataException($"参数 {name} 必须是 {minimum} 到 {maximum} 之间的整数。");
        return result >= minimum && result <= maximum ? result
            : throw new InvalidDataException($"参数 {name}={result} 超出范围；允许 {minimum} 到 {maximum}，默认 {fallback}。");
    }
    private static int Number(JsonElement args, string name, int fallback, int minimum, int maximum) =>
        (int)LongNumber(args, name, fallback, minimum, maximum);
    private static string Limit(string text, int maximum) => text.Length <= maximum ? text : text[..maximum] + "\n[truncated]";
    private static AgentToolDefinition Define(string name, string description, string properties, params string[] required)
    {
        var fields = System.Text.Json.Nodes.JsonNode.Parse(properties)!.AsObject();
        if (name is "checkpoint" or "test_adapter")
        {
            fields["coverageSamples"] = System.Text.Json.Nodes.JsonNode.Parse("""{"type":"array","maxItems":64,"items":{"type":"object","properties":{"resourcePath":{"type":"string"},"locator":{"type":"string"},"sourceText":{"type":"string"}},"required":["resourcePath","locator","sourceText"],"additionalProperties":false},"description":"Merge independent original-text anchors by resourcePath/locator; host checks extraction. Omission retains previous samples."}""");
            var sampleSchema = fields["coverageSamples"]!["items"]!.DeepClone();
            fields["coverageSampleCorrections"] = new System.Text.Json.Nodes.JsonObject
            {
                ["type"] = "array", ["maxItems"] = 4,
                ["description"] = "Correct an independently proven mislabelled anchor, never to conceal a reader defect. Supply the exact original sample, a real independently located replacement, reason and evidence (resource/byte offsets or reference). No blanket deletion. All other anchors remain; full test_adapter must pass again. Omission retains samples.",
                ["items"] = new System.Text.Json.Nodes.JsonObject
                {
                    ["type"] = "object",
                    ["properties"] = new System.Text.Json.Nodes.JsonObject
                    {
                        ["original"] = sampleSchema.DeepClone(), ["replacement"] = sampleSchema,
                        ["reason"] = new System.Text.Json.Nodes.JsonObject { ["type"] = "string", ["maxLength"] = 600 },
                        ["evidence"] = new System.Text.Json.Nodes.JsonObject { ["type"] = "string", ["maxLength"] = 2000 }
                    },
                    ["required"] = new System.Text.Json.Nodes.JsonArray("original", "replacement", "reason", "evidence"),
                    ["additionalProperties"] = false
                }
            };
            fields["pendingCoverage"] = System.Text.Json.Nodes.JsonNode.Parse("""{"type":"array","maxItems":24,"items":{"type":"string"},"description":"Replace concrete known missing extraction. Omission retains prior tasks. Do not treat untested runtime behavior as missing coverage or guess/delete legacy items; classify explicitly from evidence."}""");
            fields["runtimeChecks"] = System.Text.Json.Nodes.JsonNode.Parse("""{"type":"array","maxItems":24,"items":{"type":"string"},"description":"Replace pending startup/rendering checks, separate from known missing text. Omission retains prior checks. Untested runtime checks alone do not block publication after full resource validation."}""");
        }
        if (name == "finish") fields["allowPartial"] = System.Text.Json.Nodes.JsonNode.Parse("""{"type":"boolean","description":"Explicitly publish usable partial support with known missing coverage, disclosed in summary. Repair may retain unrelated coverage omissions without expanding scope."}""");
        var schema = JsonSerializer.SerializeToElement(new JsonObject
        {
            ["type"] = "object",
            ["properties"] = EngineJson.Node(fields, EngineJson.Default),
            ["required"] = EngineJson.Node(required, EngineJson.Default),
            ["additionalProperties"] = false
        }, EngineJson.TypeInfo<JsonObject>(EngineJson.Default));
        return new(name, description, schema);
    }
    internal sealed record ToolSource(string Url, string Sha256);
}
