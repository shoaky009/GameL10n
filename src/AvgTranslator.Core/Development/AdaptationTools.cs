using System.Net.Http.Headers;
using System.Security.Cryptography;
using System.Text;
using System.Text.Encodings.Web;
using System.Text.Json;
using AvgTranslator.Core.Adapters;
using AvgTranslator.Core.Isolation;
using AvgTranslator.Core.Storage;

namespace AvgTranslator.Core.Development;

/// <summary>Host operations for one engine development workspace; no provider credentials are passed to tools.</summary>
public sealed partial class AdaptationTools(AdaptationRequest request, AdaptationSession session, string workspace,
    GeneratedAdapterCompiler compiler, LocalAdapterPackageStore store, HttpClient? researchClient = null,
    BuiltinAdapterAssets? builtinAssets = null, BuiltinAdapterTrial? builtinTrial = null, AdapterComponentAssets? componentAssets = null,
    Action<string, string>? reportProgress = null)
{
    private static readonly HttpClient PublicHttp = new(new HttpClientHandler { AllowAutoRedirect = false, UseCookies = false })
        { Timeout = TimeSpan.FromMinutes(3) };
    private static readonly JsonSerializerOptions Json = new(JsonSerializerDefaults.Web)
        { Encoder = JavaScriptEncoder.UnsafeRelaxedJsonEscaping };
    public IGameAdapter? PublishedAdapter { get; private set; }
    private string ResourceDirectory => Path.Combine(workspace, "resources");
    private string SourceDirectory => Path.Combine(workspace, "src");
    private int unresolvedStopReports;

    public static IReadOnlyList<AgentToolDefinition> Definitions { get; } =
    [
        Define("inspect_engine_knowledge", "Search shared engine/format experience from previous projects. With id, read conditional guidance and helper index. Without id, query searches engine names/aliases; empty query lists this game's matched candidates. Verify conditions locally; no match is normal and does not mean unsupported.",
            """{"query":{"type":"string"},"id":{"type":"string"}}""", []),
        Define("copy_engine_knowledge_tools", "Copy one shared source helper into work/ without overwriting edits. Requires id and tool name from inspect_engine_knowledge. Inspect applicability and dependencies, then use run_csharp/run_engine_python on current samples. Does not execute, validate or publish an adapter. Call alone.",
            """{"id":{"type":"string"},"tool":{"type":"string"}}""", "id", "tool"),
        Define("list_files", "List game or workspace files with sizes. Pagination, no file contents.",
            """{"root":{"type":"string","enum":["game","workspace"]},"path":{"type":"string"},"offset":{"type":"integer","minimum":0,"maximum":1000000},"limit":{"type":"integer","minimum":1,"maximum":200}}""", "root"),
        Define("read_file", "Read a bounded file slice. encoding: hex, utf8, cp932, cp936. Source/JSON and workspace work/*.txt/.log/.csv reports default to utf8; unknown game resources default to hex. Use search_text byteOffsets for targeted reads instead of rereading whole files. Returns the encoding used.",
            """{"root":{"type":"string","enum":["game","workspace"]},"path":{"type":"string"},"offset":{"type":"integer","minimum":0},"length":{"type":"integer","minimum":1,"maximum":32768},"encoding":{"type":"string","enum":["hex","utf8","cp932","cp936"]}}""", "root", "path"),
        Define("read_tool_result", "Read a character slice of an earlier tool result omitted from replay. Uses the existing session, no rerun or external request. Optional query locates relevant text. Available until checkpoint discards that exchange; prefer existing evidence files across checkpoints.",
            """{"toolCallId":{"type":"string"},"offset":{"type":"integer","minimum":0},"length":{"type":"integer","minimum":1,"maximum":16384},"query":{"type":"string","maxLength":256}}""", "toolCallId"),
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
        Define("inspect_adapter_component", "Inspect one reusable encoding/font/launcher component from list_adapters.components: API, example, exact applicability and files. Optional path reads a bounded source slice (character offsets). Components are not complete adapters or proof of game compatibility.",
            """{"id":{"type":"string"},"path":{"type":"string"},"offset":{"type":"integer"},"length":{"type":"integer"}}""", "id"),
        Define("copy_adapter_components", "Copy 1-4 selected reusable components into src without overwriting differing files. C# sources join normal adapter compilation; the QLIE launcher includes a ready-to-copy standalone EXE under src/tools. No font files, framework installation or API calls. Confirm applicability before use; test_adapter still required. Call alone.",
            """{"ids":{"type":"array","items":{"type":"string"}}}""", "ids"),
        Define("try_builtin_adapter", "Try a built-in parser on 1-12 explicit files under workspace resources/ or work/. Total input at most 64 MiB. Returns count, bounded samples, concrete parse diagnostics and optionally host-checked original-byte roundtrip (default true). Include required companions. Does not translate, test rendering, publish or activate. Stage original samples first; unpack archives locally into work/.",
            """{"format":{"type":"string"},"directory":{"type":"string"},"files":{"type":"array","items":{"type":"string"}},"checkRoundtrip":{"type":"boolean"}}""", "format", "directory", "files"),
        Define("adapter_protocol", "Get the exact runtime adapter protocol. No model request or external operation.", "{}", []),
        Define("copy_adapter", "Reuse an adapter in src. Published adapters copy source/dependencies. Built-in IDs from list_adapters default to a small parser DLL plus a no-Main call bridge; editableSource=true copies only the selected parser's source and shared helpers when changes are needed. Existing differing files are not overwritten. Implement archive/probe/runtime glue and pass test_adapter before finish.",
            """{"id":{"type":"string"},"version":{"type":"string"},"editableSource":{"type":"boolean"}}""", "id", "version"),
        Define("search_engine_references", "Look up an engine, alias or format in the GalTransl index: returns scoped docs, module paths and public functions, NOT verified adapters. Uses the shared local library when prepared, otherwise online metadata. No game data is sent. Use inspect_engine_reference for local guides and run_engine_python to call existing algorithms instead of rewriting them.",
            """{"query":{"type":"string"}}""", "query"),
        Define("inspect_engine_reference", "Read one engine guide or a bounded source slice from the shared GalTransl Python toolkit. First use downloads and pins its source; later projects reuse it locally. Returns public function names, module paths and reuse instructions. engine is an exact ID from search_engine_references. Optional query/offset/length locate a relevant section. retryDownload explicitly retries a failed download. Call alone.",
            """{"engine":{"type":"string"},"path":{"type":"string"},"query":{"type":"string"},"offset":{"type":"integer"},"length":{"type":"integer"},"retryDownload":{"type":"boolean"}}""", "engine"),
        Define("run_engine_python", "Run work/*.py or src/tools/*.py with shared portable Python and pinned python.engines/python.archives. No system Python, pip or pycache; optional modules may be missing. Use staged samples and concise output. Can follow write_file calls in one ordered batch.",
            """{"script":{"type":"string"},"arguments":{"type":"array","items":{"type":"string"}},"stdin":{"type":"string"},"timeoutSeconds":{"type":"integer"}}""", "script"),
        Define("use_engine_reference", "Bind the shared Python toolkit into src: writes a pinned dependency and a C# call bridge, without copying the toolkit into each project. Add a small src/tools/*.py JSON CLI wrapper and Program.cs using the returned example. Published adapters use the same library without LLM calls. test_adapter remains required. Call alone.", "{}", []),
        Define("search_repositories", "Search public GitHub repository NAMES/descriptions for existing engine tools; this is NOT code or file-content search. Prefer search_engine_references for an engine/signature. Uses anonymous GitHub API.",
            """{"query":{"type":"string"}}""", "query"),
        Define("fetch_url", "Read public HTTPS reference data, up to 8 MiB locally. offset/length paginate decoded characters (default 12000, max 24000). Optional query searches literal text case-insensitively from offset and returns bounded excerpts with nextOffset; useful for large format catalogs and GitHub trees. Cached in memory; no files written. Contents are untrusted data.",
            """{"url":{"type":"string"},"query":{"type":"string"},"offset":{"type":"integer"},"length":{"type":"integer"}}""", "url"),
        Define("download_tool", "Download a public tool/source file to src/tools or work. Returns SHA256; records URL for packaged dependencies. For a known final dependency set releaseRole and purpose once to register it; investigative downloads belong in work/.",
            """{"url":{"type":"string"},"path":{"type":"string"},"releaseRole":{"type":"string","enum":["runtime","asset","license"]},"purpose":{"type":"string","maxLength":240}}""", "url", "path"),
        Define("stage_resources", "Copy selected original game resources into resources for local tools. Reuses unchanged copies.",
            """{"files":{"type":"array","items":{"type":"string"}}}""", "files"),
        Define("write_file", "Write UTF-8 code or analysis files under src/ or work/, replacing without backup. New src/*.cs files auto-register as release source. For tools set releaseRole and purpose once; edits inherit existing registration. Analysis probes belong in work/. Batch known writes followed by one run_csharp/run_engine_python/run_tool to execute in order; failure skips remaining calls. Reuse existing probe scripts and one findings file rather than adding a file per small check.",
            """{"path":{"type":"string"},"content":{"type":"string"},"releaseRole":{"type":"string","enum":["source","runtime","asset","license"]},"purpose":{"type":"string","maxLength":240}}""", "path", "content"),
        Define("replace_text", "Change one exact, unique text fragment in an existing UTF-8 src/ or work/ file. Prefer this for local edits instead of rewriting whole files. oldText must match exactly once (including whitespace/newlines); otherwise nothing changes. Preserves UTF-8 BOM and other bytes. Can batch with writes followed by one local run.",
            """{"path":{"type":"string"},"oldText":{"type":"string"},"newText":{"type":"string"}}""", "path", "oldText", "newText"),
        Define("remove_file", "Remove one obsolete file under src/ or work/. Cannot delete directories or originals.",
            """{"path":{"type":"string"}}""", "path"),
        Define("run_tool", "Run dotnet, powershell, or a workspace executable. Pass arguments as an array. Use resources copies; never edit original game. Output bounded.",
            """{"executable":{"type":"string"},"arguments":{"type":"array","items":{"type":"string"}},"workingDirectory":{"type":"string"},"stdin":{"type":"string"},"timeoutSeconds":{"type":"integer"}}""", "executable", "arguments"),
        Define("run_csharp", "Compile and run explicitly selected UTF-8 .cs files under work/ or src/ with pinned .NET 10. Use a work/ test entry point plus existing src/ parser helpers; do not copy helpers or include an unrelated Program.cs. No directory-wide compilation. Main or top-level statements, explicit using directives. Optional references lists local managed DLLs under work/ or src/tools/ (include dependencies). No project/restore/NuGet. Default workingDirectory is workspace root; arguments/stdin/stdout/stderr use UTF-8. Register CodePagesEncodingProvider for cp932/cp936. Timeout defaults to 120s (1-600); compiler capped at 120s. Returns compiled, exitCode, stdout, stderr; temporary build files are deleted, sources retained. Use staged resources; never modify originals.",
            """{"sourceFiles":{"type":"array","items":{"type":"string"}},"references":{"type":"array","items":{"type":"string"}},"arguments":{"type":"array","items":{"type":"string"}},"workingDirectory":{"type":"string"},"stdin":{"type":"string"},"timeoutSeconds":{"type":"integer"}}""", "sourceFiles"),
        Define("test_adapter", "Compile and verify the saved release selection. Omit releaseFiles to reuse it; supplying releaseFiles replaces the entire selection. New src C# files and copied built-in dependencies are registered automatically. Use update_release_files only for dependency changes or exclusions. Paths are FINAL PACKAGE paths: src/Program.cs maps to workspace src/Program.cs; tools/parser.py maps to workspace src/tools/parser.py. No globs or automatic directory inclusion. Exclude probes, samples, logs, unused tools and old implementations. Test this exact release before finish. Check discovery against real game/staged samples and independently verify extraction/roundtrip; repair also checks the existing baseline. discoveryOnly=true cannot publish.",
            """{"kind":{"type":"string","enum":["engine","template"]},"key":{"type":"string"},"name":{"type":"string"},"resourcePatterns":{"type":"array","items":{"type":"string"}},"releaseFiles":{"type":"array","minItems":1,"maxItems":512,"items":{"type":"object","properties":{"path":{"type":"string","maxLength":512},"role":{"type":"string","enum":["source","runtime","asset","license"]},"purpose":{"type":"string","minLength":1,"maxLength":240}},"required":["path","role","purpose"],"additionalProperties":false}},"discoveryOnly":{"type":"boolean"}}""", "kind", "key", "name", "resourcePatterns"),
        Define("update_release_files", "Inspect or incrementally update the saved release selection. files upserts exact PACKAGE paths with source/runtime/asset/license roles and purposes; remove excludes paths without deleting files. No arguments lists selection and unregistered files. Use only for changed dependencies or exclusions; test_adapter reuses this state across failures/restarts/checkpoints. Game originals belong to resources, modified game files to patch output, never release assets.",
            """{"files":{"type":"array","maxItems":512,"items":{"type":"object","properties":{"path":{"type":"string"},"role":{"type":"string","enum":["source","runtime","asset","license"]},"purpose":{"type":"string","maxLength":240}},"required":["path","role","purpose"],"additionalProperties":false}},"remove":{"type":"array","maxItems":512,"items":{"type":"string"}}}"""),
        Define("finish", "Publish and activate the last successfully tested package. Checks source/dependency/resource contents still match verification. Reading files and updating work/ reports do not require retesting.",
            """{"summary":{"type":"string"}}""", "summary"),
        Define("report_blocker", "Stop for specific user input or an investigated obstacle. unsupported additionally requires canContinueLocally and evidence (concrete attempted approaches, results and evidence locations). canContinueLocally MUST be true if any actionable local investigation, implementation or validation remains; this returns continue, not stop. CP932 rejecting Chinese, unfinished runtime analysis, missing parser/tools or failed searches alone are unfinished work. Do not ask users to program the adapter. Call alone, concise Chinese.",
            """{"kind":{"type":"string","enum":["needs_input","unsupported"]},"summary":{"type":"string"},"reason":{"type":"string"},"nextAction":{"type":"string"},"canContinueLocally":{"type":"boolean"},"evidence":{"type":"string"}}""", "kind", "summary", "reason", "nextAction"),
        Define("checkpoint", "Compact when requested or a major phase ends, not after each probe. Prefer <=3000 characters: preserve exact field order/stride/API facts needed by the current implementation, evidence locations, failed hypotheses, coverage/omissions and next action. Do not copy source/disassembly. Recent small tool exchanges and the previously read adapter protocol survive. Call alone.",
            """{"summary":{"type":"string"}}""", "summary")
    ];

    public static bool CanBatch(string tool) => tool is "list_files" or "list_adapters" or "read_file" or "read_tool_result" or "search_text"
        or "inspect_engine_knowledge" or "inspect_existing_text"
        or "find_game_file" or "inspect_pe_imports" or "inspect_pe_code" or "inspect_unity_schema" or "inspect_builtin_adapter" or "inspect_adapter_component" or "adapter_protocol" or "search_engine_references" or "search_repositories" or "fetch_url";

    // One ordered local edit/run step. Publication, staging and verification stay separate.
    internal static bool IsWriteRunBatch(IReadOnlyList<AgentToolCall> calls) => calls.Count is >= 2 and <= 8
        && calls.Take(calls.Count - 1).All(call => call.Name is "write_file" or "replace_text")
        && calls[^1].Name is "write_file" or "replace_text" or "run_csharp" or "run_engine_python" or "run_tool";

    public static string Stage(string tool) => tool switch
    {
        "inspect_engine_knowledge" or "copy_engine_knowledge_tools" => "复用同类引擎经验",
        "list_files" or "read_file" or "search_text" or "stage_resources" or "find_game_file" or "inspect_existing_text" => "分析游戏资源",
        "inspect_game_launch" => "检查游戏启动环境",
        "inspect_pe_imports" => "分析程序导入与调用位置",
        "inspect_pe_code" => "分析程序代码与分支",
        "inspect_unity_schema" => "检查 Unity 类型与字段结构",
        "search_engine_references" or "search_repositories" or "fetch_url" or "download_tool" => "查找引擎工具",
        "inspect_engine_reference" or "use_engine_reference" => "准备共享引擎工具",
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
        "test_adapter" => "正在编译并检查文本提取、原文回填和中文写入。",
        "finish" => "正在保存已通过资源验证的引擎适配器。",
        "run_tool" or "run_csharp" or "run_engine_python" => "正在运行本地资源分析工具。",
        "inspect_engine_reference" or "use_engine_reference" => "正在读取共享引擎工具，首次使用时会下载所需资源。",
        _ => Stage(tool) + "。"
    };

    public async Task<string> ExecuteAsync(AgentToolCall call, CancellationToken ct)
    {
        if (call.Arguments.Length > 512 * 1024) throw new InvalidDataException("工具参数过大。");
        using var document = JsonDocument.Parse(call.Arguments);
        var args = document.RootElement;
        if (args.ValueKind != JsonValueKind.Object) throw new InvalidDataException("工具参数必须为对象。");
        session.ReleaseFiles ??= session.VerifiedPackage?.ReleaseFiles;
        if (call.Name != "report_blocker") unresolvedStopReports = 0;
        object result = call.Name switch
        {
            "inspect_engine_knowledge" => InspectEngineKnowledge(args),
            "copy_engine_knowledge_tools" => await CopyEngineKnowledgeToolsAsync(args, ct),
            "list_files" => ListFiles(args),
            "read_file" => await ReadFileAsync(args, ct),
            "read_tool_result" => ReadToolResult(args),
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
            "use_engine_reference" => await UseEngineReferenceAsync(args, ct),
            "search_repositories" => await SearchRepositoriesAsync(args, ct),
            "fetch_url" => await FetchAsync(args, ct),
            "download_tool" => await DownloadAsync(args, ct),
            "stage_resources" => await StageAsync(args, ct),
            "write_file" => await WriteFileAsync(args, ct),
            "replace_text" => await ReplaceTextAsync(args, ct),
            "remove_file" => RemoveFile(args),
            "run_tool" => await RunAsync(args, ct),
            "run_csharp" => await RunCSharpAsync(args, ct),
            "update_release_files" => UpdateReleaseFiles(args),
            "test_adapter" => await TestAsync(args, ct),
            "finish" => await FinishAsync(args, ct),
            "report_blocker" => ReportBlocker(args),
            "checkpoint" => Checkpoint(args),
            _ => throw new InvalidDataException("Unknown tool: " + call.Name)
        };
        return JsonSerializer.Serialize(new { success = true, result }, Json);
    }

    private async Task<GameLaunchInspection> InspectLaunchAsync(CancellationToken ct)
    {
        var inspection = await GameLaunchInspector.InspectAsync(request.GameDirectory, ct);
        if (JsonSerializer.Serialize(session.LaunchInspection, Json) != JsonSerializer.Serialize(inspection, Json))
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

    private void RequireLaunchResources()
    {
        foreach (var profile in session.LaunchInspection?.LocaleProfiles ?? [])
            if (profile.ExecutableExists && session.ResourceHashes.ContainsKey(profile.Executable)
                && !session.ResourceHashes.ContainsKey(profile.Path))
                throw new InvalidDataException("所选程序有转区配置尚未纳入验证，请 stage_resources 并声明资源发现规则：" + profile.Path);
    }

    private object Checkpoint(JsonElement args)
    {
        var summary = Text(args, "summary");
        if (summary.Length is < 1 or > 8000) throw new InvalidDataException("进度摘要必须在 1 到 8000 字符之间。");
        UpdateCoverage(args);
        session.Checkpoint = summary;
        session.Notes = summary;
        return new { saved = true, objective = session.Objective?.ToString(),
            nextAction = "已保存当前目标与验收记录。继续摘要中的下一项；完整验收已通过且目标完成时可直接 finish。" };
    }

    private void UpdateCoverage(JsonElement args)
    {
        var current = session.TextCoverage;
        if (args.TryGetProperty("coverageSampleCorrections", out var corrections))
        {
            var items = corrections.Deserialize<TextCoverageSampleCorrection[]>(Json)
                ?? throw new InvalidDataException("样本纠正不能为空。");
            if (items.Length is < 1 or > 4) throw new InvalidDataException("每次纠正 1 到 4 个明确误标的样本。");
            foreach (var correction in items) current = current.CorrectSample(correction);
        }
        var samples = current.Samples.ToDictionary(s => (s.ResourcePath.Replace('\\', '/'), s.Locator));
        if (args.TryGetProperty("coverageSamples", out var values))
            foreach (var sample in values.Deserialize<TextCoverageSample[]>(Json) ?? throw new InvalidDataException("原文样本不能为空。"))
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
        return new { protocol = AutomaticAdaptationService.ProtocolDescription };
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
                    return new { stopped = true, status = "incomplete", reason = "模型连续请求停止，但未提供充分的停止说明。已保存未完成步骤，可继续适配。" };
                }
                return new { stopped = false, status = "continue", nextAction = blocker.NextAction,
                    reason = "尚有本地可执行工作，或缺少停止依据。请执行下一项具体工具操作；未验证、未实现、CP932 不支持中文以及未找到现成工具都不是不可适配的结论。确实无法继续时，补充 canContinueLocally=false 和具体尝试及结果 evidence。无需发布未经验证的补丁。" };
            }
        }
        session.Blocker = blocker;
        return new { stopped = true };
    }

    private object ListFiles(JsonElement args)
    {
        var root = Root(args);
        var directory = Text(args, "path", "");
        if (directory.Length > 0 && directory != ".") directory = WorkspaceFiles.Resolve(root, directory);
        else directory = root;
        if (!Directory.Exists(directory)) return new { files = Array.Empty<object>(), more = false };
        var offset = Number(args, "offset", 0, 0, 1_000_000);
        var limit = Number(args, "limit", 100, 1, 200);
        var files = Directory.EnumerateFiles(directory, "*", new EnumerationOptions
        { RecurseSubdirectories = true, AttributesToSkip = FileAttributes.ReparsePoint, IgnoreInaccessible = true })
            .Skip(offset).Take(limit + 1).ToArray();
        return new { files = files.Take(limit).Select(path => new
            { path = Path.GetRelativePath(root, path).Replace('\\', '/'), bytes = new FileInfo(path).Length }).ToArray(),
            more = files.Length > limit, nextOffset = offset + Math.Min(files.Length, limit) };
    }

    private object ReadToolResult(JsonElement args)
    {
        var id = Text(args, "toolCallId");
        var result = session.Messages.LastOrDefault(message => message.Role == "tool" && message.ToolCallId == id)?.Content
            ?? throw new InvalidDataException("此结果已不在当前会话中，请根据进度摘要读取对应的证据文件。");
        var offset = Number(args, "offset", 0, 0, result.Length);
        var length = Number(args, "length", 4096, 1, 16 * 1024);
        var query = Text(args, "query", "");
        if (query.Length > 256) throw new InvalidDataException("查询内容过长。");
        if (query.Length > 0)
        {
            var found = result.IndexOf(query, offset, StringComparison.OrdinalIgnoreCase);
            if (found < 0) return new { toolCallId = id, found = false, totalCharacters = result.Length };
            offset = Math.Max(offset, found - 160);
        }
        length = Math.Min(length, result.Length - offset);
        return new { toolCallId = id, offset, length, totalCharacters = result.Length,
            more = offset + length < result.Length, content = result.Substring(offset, length) };
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
        return new { bytes = stream.Length, offset, length, encoding, more = offset + length < stream.Length,
            content = encoding == "hex" ? Convert.ToHexString(data) : EncodingFor(encoding).GetString(data) };
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

    private sealed record TextSearchPage(IReadOnlyList<string> Matches, IReadOnlyList<long?> ByteOffsets,
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
            matches.AddRange(result.Matches.Select((excerpt, index) => (object)new
                { path = relative, byteOffset = result.ByteOffsets[index], encoding, excerpt }));
            if (result.More) { nextFileByteOffset = result.NextOffset; break; }
            nextOffset++;
            nextFileByteOffset = 0;
        }
        return new { matches, scannedFiles, scannedBytes, nextOffset, nextFileByteOffset,
            more = nextFileByteOffset > 0 || nextOffset - offset < files.Length };
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
        return new { staged = files, directory = "resources" };
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
        return await WriteFileAsync(JsonSerializer.SerializeToElement(new { path = relative, content = updated }), ct);
    }

    private async Task<object> WriteFileAsync(JsonElement args, CancellationToken ct)
    {
        var relative = Text(args, "path").Replace('\\', '/');
        var path = WritePath(relative);
        var isNew = !File.Exists(path);
        var release = DeclaredReleaseFile(args, relative);
        var content = Text(args, "content");
        var bytes = Encoding.UTF8.GetBytes(content);
        if (bytes.Length > 512 * 1024) throw new InvalidDataException("单次写入文件过大。");
        if (File.Exists(path) && new FileInfo(path).Length == bytes.Length
            && (await File.ReadAllBytesAsync(path, ct)).AsSpan().SequenceEqual(bytes))
        {
            if (release is not null) RegisterReleaseFiles([release]);
            return new { path = Text(args, "path"), bytes = bytes.Length, changed = false };
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
        if (release is not null) RegisterReleaseFiles([release]);
        else if (isNew && relative.StartsWith("src/", StringComparison.Ordinal)
            && !relative.StartsWith("src/tools/", StringComparison.OrdinalIgnoreCase)
            && relative.EndsWith(".cs", StringComparison.OrdinalIgnoreCase))
            RegisterReleaseFiles([new(relative, "source", "适配器实现")]);
        return new { path = Text(args, "path"), bytes = bytes.Length, changed = true };
    }

    private object RemoveFile(JsonElement args)
    {
        var path = WritePath(Text(args, "path"));
        InvalidateSource(path);
        File.Delete(path);
        var packagedPath = AdapterReleaseFiles.PackagePath(Text(args, "path").Replace('\\', '/'));
        session.ReleaseFiles = session.ReleaseFiles?.Where(file => !file.Path.Equals(packagedPath, StringComparison.OrdinalIgnoreCase)).ToArray();
        return new { removed = Text(args, "path") };
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
            RegisterReleaseFiles(copied);
            return result;
        }
        var pinned = store.LoadActive(request.GameId);
        var package = (pinned?.Id == Text(args, "id") && pinned.Version == Text(args, "version") ? pinned : null)
            ?? store.ListReusablePackages().SingleOrDefault(item =>
            item.Id == Text(args, "id") && item.Version == Text(args, "version"))
            ?? throw new InvalidDataException("未找到此版本的适配器。");
        if (Directory.Exists(SourceDirectory) && Directory.EnumerateFileSystemEntries(SourceDirectory).Any())
            throw new InvalidDataException("src 已包含代码。请先检查现有文件，避免覆盖已有开发成果。");
        session.VerifiedPackage = null;
        var source = Directory.Exists(Path.Combine(package.DirectoryPath, "src"))
            ? Path.Combine(package.DirectoryPath, "src") : package.DirectoryPath;
        foreach (var file in Directory.EnumerateFiles(source, "*", SearchOption.AllDirectories))
        {
            var relative = Path.GetRelativePath(source, file);
            if (!relative.EndsWith(".cs", StringComparison.OrdinalIgnoreCase) && !relative.Replace('\\', '/').StartsWith("tools/", StringComparison.Ordinal)) continue;
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
        return new { package.Id, package.Version, destination = "src", package.ReleaseFiles,
            nextAction = "沿用已声明文件，按本次实现补充或移除；旧包无 releaseFiles 时检查源码和依赖后显式选择，不要将所有 tools 文件默认发布。" };
    }

    private async Task<object> RunAsync(JsonElement args, CancellationToken ct)
    {
        var executable = Text(args, "executable");
        var arguments = Strings(args, "arguments").ToList();
        if (arguments.Count > 128 || arguments.Sum(value => value.Length) > 64 * 1024)
            throw new InvalidDataException("工具命令参数过大。");
        executable = executable switch
        {
            "dotnet" => compiler.Toolchain.ExecutablePath,
            "powershell" => Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.Windows), "System32", "WindowsPowerShell", "v1.0", "powershell.exe"),
            _ => WorkspaceFiles.Resolve(workspace, executable)
        };
        if (Text(args, "executable") == "powershell")
            arguments.InsertRange(0, ["-NoLogo", "-NoProfile", "-NonInteractive"]);
        var working = Text(args, "workingDirectory", ".");
        working = working == "." ? workspace : WorkspaceFiles.Resolve(workspace, working);
        Directory.CreateDirectory(working);
        var result = await compiler.Runner.RunAsync(new(executable, arguments, working,
            Text(args, "stdin", ""), TimeSpan.FromSeconds(Number(args, "timeoutSeconds", 120, 1, 600)), 1024 * 1024), ct);
        return new { result.ExitCode, stdout = Limit(result.StandardOutput, 16_000), stderr = Limit(result.StandardError, 8000) };
    }

    private async Task<object> RunCSharpAsync(JsonElement args, CancellationToken ct)
    {
        var sources = Strings(args, "sourceFiles");
        var references = args.TryGetProperty("references", out _) ? Strings(args, "references") : [];
        var arguments = args.TryGetProperty("arguments", out _) ? Strings(args, "arguments") : [];
        if (sources.Count is < 1 or > 64 || references.Count > 64)
            throw new InvalidDataException("每次运行 1 到 64 个 C# 源文件，最多引用 64 个本地 DLL。");
        if (arguments.Count > 128 || arguments.Sum(value => value.Length) > 64 * 1024)
            throw new InvalidDataException("工具命令参数过大。");
        string ResolveInput(string relative, bool source)
        {
            relative = relative.Replace('\\', '/');
            var allowedDirectory = relative.StartsWith("work/", StringComparison.Ordinal)
                || relative.StartsWith(source ? "src/" : "src/tools/", StringComparison.Ordinal);
            if (!allowedDirectory || !relative.EndsWith(source ? ".cs" : ".dll", StringComparison.OrdinalIgnoreCase))
                throw new InvalidDataException(source ? "分析源码必须是 work/ 或 src/ 中显式选择的 .cs 文件。" : "引用必须是 work/ 或 src/tools/ 中的 .dll 文件。");
            var path = WorkspaceFiles.Resolve(workspace, relative);
            var normalized = Path.GetRelativePath(workspace, path).Replace('\\', '/');
            if (!normalized.StartsWith("work/", StringComparison.Ordinal)
                && !normalized.StartsWith(source ? "src/" : "src/tools/", StringComparison.Ordinal))
                throw new InvalidDataException("C# 分析输入超出允许的源码或引用目录。");
            if (!File.Exists(path)) throw new FileNotFoundException("C# 分析文件不存在：" + relative);
            return path;
        }
        var sourcePaths = sources.Select(path => ResolveInput(path, true)).ToArray();
        var referencePaths = references.Select(path => ResolveInput(path, false)).ToArray();
        var working = Text(args, "workingDirectory", ".");
        working = working == "." ? workspace : WorkspaceFiles.Resolve(workspace, working);
        Directory.CreateDirectory(working);
        var (compiled, result) = await compiler.RunCSharpAsync(workspace, sourcePaths, referencePaths, arguments, working,
            Text(args, "stdin", ""), TimeSpan.FromSeconds(Number(args, "timeoutSeconds", 120, 1, 600)), ct);
        return new { compiled, result.ExitCode, stdout = Limit(result.StandardOutput, 16_000), stderr = Limit(result.StandardError, 8000) };
    }

    private async Task<object> TestAsync(JsonElement args, CancellationToken ct)
    {
        UpdateCoverage(args);
        var releaseFiles = args.TryGetProperty("releaseFiles", out var declared)
            ? AdapterReleaseFiles.Validate(declared.Deserialize<AdapterReleaseFile[]>(Json))
            : session.ReleaseFiles;
        session.ReleaseFiles = releaseFiles;
        session.VerifiedPackage = null;
        session.VerifiedCoverageFingerprint = null;
        session.VerifiedObjective = null;
        session.VerifiedBaselineFingerprint = null;
        if (session.ResourceHashes.Count == 0) throw new InvalidDataException("请先 stage_resources 选择用于验证的真实资源。");
        RequireLaunchResources();
        await CheckResourcesAsync(ct);
        var family = new AdapterFamily(Text(args, "kind"), Text(args, "key"), Text(args, "name"));
        family.Validate();
        var patterns = Strings(args, "resourcePatterns");
        releaseFiles = AdapterReleaseFiles.Validate(releaseFiles);
        var timer = System.Diagnostics.Stopwatch.StartNew();
        var package = await compiler.CompileWorkspaceAsync(SourceDirectory, Path.Combine(workspace, "build"), patterns, releaseFiles, ct, family);
        CheckReleaseGameInputs(package);
        var compileMs = timer.ElapsedMilliseconds;
        timer.Restart();
        var adapter = new GeneratedGameAdapter(package, compiler.Toolchain, compiler.Runner, store.WorkDirectory, EngineToolkit, reportProgress);
        try
        {
            await CheckDiscoveryAsync(adapter, ct);
        }
        catch (Exception error) when (error is GeneratedAdapterException or InvalidDataException or IOException)
        {
            session.VerificationDiagnostics = [error.Message];
            return new { passed = false, stage = "discovery", diagnostics = session.VerificationDiagnostics, runtimeTested = false,
                nextAction = "修复实际资源发现失败后重新 test_adapter；不要绕过失败发布。", compileMs, discoveryMs = timer.ElapsedMilliseconds };
        }
        var discoveryMs = timer.ElapsedMilliseconds;
        if (args.TryGetProperty("discoveryOnly", out var discoveryOnly) && discoveryOnly.GetBoolean())
        {
            session.VerificationDiagnostics = ["真实游戏与样本的探测结果一致；尚未执行提取及回填验收，不能发布。"];
            return new { passed = true, stage = "discovery", diagnostics = session.VerificationDiagnostics, runtimeTested = false,
                nextAction = "继续实现并执行完整 test_adapter；仅资源发现通过还不能发布。", compileMs, discoveryMs };
        }
        var verificationDirectory = WorkspaceFiles.Resolve(workspace, "verification");
        GeneratedGameAdapter.DeleteStagedTree(verificationDirectory);
        timer.Restart();
        var verification = await new GeneratedAdapterVerifier().VerifyAsync(adapter, ResourceDirectory, verificationDirectory, ct,
            sourceIsSnapshot: true, coverage: session.TextCoverage,
            baselineUnits: session.Objective == AdaptationObjective.Repair ? request.BaselineUnits : null);
        session.VerificationDiagnostics = verification.Diagnostics.ToArray();
        if (verification.Passed)
        {
            session.VerifiedPackage = package;
            session.VerificationRevision = GeneratedAdapterVerifier.Revision;
            session.VerifiedCoverageFingerprint = session.TextCoverage.Fingerprint();
            session.VerifiedObjective = session.Objective;
            session.VerifiedBaselineFingerprint = session.Objective == AdaptationObjective.Repair ? BaselineFingerprint() : null;
        }
        var nextAction = !verification.Passed
            ? "修复 diagnostics 中的真实失败后重新 test_adapter；未通过验收必须继续修复。"
            : session.Objective == AdaptationObjective.Repair
                ? "本次资源与已有文本验收已通过。若反馈已解决，直接 finish；不要自动开始补全其他文字类别。已知其他覆盖遗漏用 allowPartial=true 并在 summary 披露，运行效果待验证单列 runtimeChecks。长会话可先 checkpoint 保存本阶段。"
                : "完整资源验收已通过。当前目标完成后直接 finish，披露已知遗漏和 runtimeChecks；长会话可先 checkpoint。不要重复通过的测试或为未调查类别自动增加研究。";
        return new { verification.Passed, stage = verification.Passed ? "finish-ready" : "verification", nextAction,
            verification.Diagnostics, package.Id, package.Version, runtimeTested = false,
            pendingCoverage = session.TextCoverage.Pending, runtimeChecks = session.TextCoverage.RuntimeChecks,
            compileMs, discoveryMs, verificationMs = timer.ElapsedMilliseconds };
    }

    private string? BaselineFingerprint() => request.BaselineUnits is null ? null
        : LocalFiles.HashText(JsonSerializer.Serialize(request.BaselineUnits, Json));

    private async Task<object> FinishAsync(JsonElement args, CancellationToken ct)
    {
        if (request.ImproveExisting && !(session.Objective == AdaptationObjective.Repair && request.BaselineUnits is { Count: > 0 })
            && session.TextCoverage.Samples.Count == 0)
            throw new InvalidDataException("继续完善已有适配器时，请在 test_adapter 提供独立定位的 coverageSamples，确认反馈涉及的原文和字段已正确提取。");
        var package = session.VerifiedPackage ?? throw new InvalidDataException("尚无已通过验证的适配器；代码或资源变更后需要重新 test_adapter。");
        if (session.VerificationRevision != GeneratedAdapterVerifier.Revision)
            throw new InvalidDataException("已保存的验证未覆盖当前验收规则，请在现有源码上重新 test_adapter，无需重新分析或开发。");
        if (session.Objective == AdaptationObjective.Repair
            && (session.VerifiedObjective != AdaptationObjective.Repair || session.VerifiedBaselineFingerprint != BaselineFingerprint()))
            throw new InvalidDataException("已保存的验证未核对当前修复任务的原有文本基线，请重新 test_adapter；无需重新开发或扩展覆盖。");
        if (session.TextCoverage.Samples.Count > 0 && session.VerifiedCoverageFingerprint != session.TextCoverage.Fingerprint())
            throw new InvalidDataException("文字覆盖样本已更新，请 test_adapter 核对实际提取结果。");
        var partial = session.TextCoverage.Pending.Count > 0;
        if (partial && !(args.TryGetProperty("allowPartial", out var allow) && allow.ValueKind == JsonValueKind.True))
            throw new InvalidDataException((session.Objective == AdaptationObjective.Repair
                ? "本次修复可保留其他范围的已知覆盖遗漏；请 allowPartial=true，并在 summary 说明这些剩余限制，无需自动扩展其他类别。已知遗漏："
                : "仍有已知文字覆盖待办，请继续处理。确实暂时无法完成时可 allowPartial=true 发布部分结果，并在 summary 说明原因。待办：")
                + string.Join("；", session.TextCoverage.Pending));
        var summary = Text(args, "summary").Trim();
        if (summary.Length == 0) throw new InvalidDataException("请在 summary 说明本次完成内容及剩余限制。");
        // Local tools may edit sources or only inspect/report. Compare the actual compilation inputs
        // at publication, rather than making every probe invalidate a successful roundtrip.
        await CheckVerifiedSourcesAsync(package, ct);
        CheckReleaseGameInputs(package);
        RequireLaunchResources();
        await CheckResourcesAsync(ct);
        var probe = await CheckDiscoveryAsync(new GeneratedGameAdapter(package, compiler.Toolchain, compiler.Runner, store.WorkDirectory, EngineToolkit, reportProgress), ct);
        var hashes = probe.RequiredFiles.ToDictionary(file => file, file => session.ResourceHashes[file.Replace('\\', '/')]);
        var published = await store.PublishAndActivateAsync(request.GameId, package, session.VerificationDiagnostics, ct);
        session.VerifiedPackage = published;
        await store.RecordCompatibilityAsync(package.Family!, new(request.GameId,
            Path.GetFileName(Path.TrimEndingDirectorySeparator(request.GameDirectory)), probe.Description, package.Version, hashes,
            "resource-roundtrip-passed"), ct);
        session.Summary = (partial ? "已发布部分文字支持；仍有未覆盖内容。\n" : "") + Limit(summary, 2000)
            + (partial ? "\n已知未覆盖内容：" + string.Join("；", session.TextCoverage.Pending) : "")
            + (session.TextCoverage.RuntimeChecks.Count > 0 ? "\n运行效果待验证：" + string.Join("；", session.TextCoverage.RuntimeChecks) : "");
        session.Stage = partial ? "部分文字可试译" : "适配完成";
        PublishedAdapter = new GeneratedGameAdapter(published, compiler.Toolchain, compiler.Runner, store.WorkDirectory, EngineToolkit, reportProgress);
        return new { published.Id, published.Version, summary = session.Summary, runtimeTested = false,
            coverageStatus = partial ? "partial" : session.TextCoverage.Samples.Count > 0 ? "samples-verified" : "not-assessed",
            pendingCoverage = session.TextCoverage.Pending, runtimeChecks = session.TextCoverage.RuntimeChecks };
    }

    private async Task CheckVerifiedSourcesAsync(GeneratedAdapterPackage package, CancellationToken ct)
    {
        if (package.ManifestVersion < 3 || package.ReleaseFiles is null)
            throw new InvalidDataException("旧验证结果未声明正式包文件，请沿用现有源码并通过 releaseFiles 重新 test_adapter，无需重新分析引擎。");
        var releaseFiles = AdapterReleaseFiles.Validate(package.ReleaseFiles);
        var unchanged = true;
        foreach (var file in releaseFiles)
        {
            var input = AdapterReleaseFiles.SourcePath(SourceDirectory, file);
            if (package.PackageFiles is null || !package.PackageFiles.TryGetValue(file.Path, out var hash)
                || !File.Exists(input) || await LocalFiles.HashFileAsync(input, ct) != hash)
            { unchanged = false; break; }
        }
        if (!unchanged)
        {
            session.VerifiedPackage = null;
            throw new InvalidDataException("适配源码或依赖与已验证版本不同，请重新 test_adapter；分析报告和笔记不影响验证。");
        }
    }

    private async Task<GameProbe> CheckDiscoveryAsync(GeneratedGameAdapter adapter, CancellationToken ct)
    {
        var gameCandidates = adapter.DiscoverResources(request.GameDirectory);
        var sampleCandidates = adapter.DiscoverResources(ResourceDirectory);
        var candidateDifferences = Differences(gameCandidates, sampleCandidates);
        try
        {
            var sample = await adapter.ProbeAsync(ResourceDirectory, ct)
                ?? throw new InvalidDataException("适配器未识别暂存样本。probe 只获得文件前 64KB，请检查头部识别条件；完整解析应在 extract/build 阶段执行。");
            var actual = await adapter.ProbeAsync(request.GameDirectory, ct)
                ?? throw new InvalidDataException("适配器未识别真实游戏目录。请检查其他 EXE、BIN 或封包是否影响候选选择；完整解析不能放在只提供前 64KB 的 probe 中。");
            var requiredDifferences = Differences(actual.RequiredFiles, sample.RequiredFiles);
            if (requiredDifferences.Length > 0)
                throw new InvalidDataException("真实游戏与样本的必需资源不一致：" + requiredDifferences
                    + "。请暂存缺少的必需资源，或修正资源规则及 probe 筛选；不要为通过校验直接包含无关文件。");
            if (!string.Equals(actual.OriginalExecutable?.Replace('\\', '/'), sample.OriginalExecutable?.Replace('\\', '/'), StringComparison.OrdinalIgnoreCase))
                throw new InvalidDataException($"启动入口不一致：游戏={actual.OriginalExecutable ?? "未指定"}，样本={sample.OriginalExecutable ?? "未指定"}。请按引擎结构区分原启动器和设置/卸载程序。");
            return sample;
        }
        catch (Exception error) when (error is GeneratedAdapterException or InvalidDataException or IOException)
        {
            throw new InvalidDataException(error.Message + (candidateDifferences.Length == 0 ? "" : "\n资源规则匹配的候选差异：" + candidateDifferences), error);
        }

        static string Differences(IEnumerable<string> actual, IEnumerable<string> sample)
        {
            var game = actual.Select(path => path.Replace('\\', '/')).ToHashSet(StringComparer.OrdinalIgnoreCase);
            var staged = sample.Select(path => path.Replace('\\', '/')).ToHashSet(StringComparer.OrdinalIgnoreCase);
            var missing = game.Except(staged, StringComparer.OrdinalIgnoreCase).Order(StringComparer.Ordinal).ToArray();
            var extra = staged.Except(game, StringComparer.OrdinalIgnoreCase).Order(StringComparer.Ordinal).ToArray();
            var parts = new List<string>();
            if (missing.Length > 0) parts.Add($"样本缺少 {missing.Length} 项：" + string.Join("、", missing.Take(12)));
            if (extra.Length > 0) parts.Add($"样本多出 {extra.Length} 项：" + string.Join("、", extra.Take(12)));
            return string.Join("；", parts);
        }
    }

    private async Task CheckResourcesAsync(CancellationToken ct)
    {
        foreach (var (relative, hash) in session.ResourceHashes)
        {
            if (await LocalFiles.HashFileAsync(WorkspaceFiles.Resolve(request.GameDirectory, relative), ct) != hash
                || await LocalFiles.HashFileAsync(WorkspaceFiles.Resolve(ResourceDirectory, relative), ct) != hash)
                throw new InvalidDataException("原资源或工作副本发生变化，请重新 stage_resources 并验证：" + relative);
        }
    }

    private async Task<object> SearchRepositoriesAsync(JsonElement args, CancellationToken ct)
    {
        var query = Text(args, "query");
        if (query.Length > 512) throw new InvalidDataException("检索条件过长。");
        using var response = await GetAsync(new("https://api.github.com/search/repositories?per_page=8&q=" + Uri.EscapeDataString(query)), ct);
        var text = await ReadResponseAsync(response, 256 * 1024, ct);
        using var json = JsonDocument.Parse(text);
        return json.RootElement.GetProperty("items").EnumerateArray().Select(item => new
        {
            name = item.GetProperty("full_name").GetString(), url = item.GetProperty("html_url").GetString(),
            description = item.GetProperty("description").GetString(),
            license = item.TryGetProperty("license", out var license) && license.ValueKind == JsonValueKind.Object
                ? license.GetProperty("spdx_id").GetString() : null
        }).ToArray();
    }

    private async Task<object> DownloadAsync(JsonElement args, CancellationToken ct)
    {
        var relative = Text(args, "path").Replace('\\', '/');
        if (!relative.StartsWith("src/tools/", StringComparison.Ordinal) && !relative.StartsWith("work/", StringComparison.Ordinal))
            throw new InvalidDataException("下载目标必须在 src/tools/ 或 work/。");
        var path = WritePath(relative);
        var release = DeclaredReleaseFile(args, relative);
        InvalidateSource(path);
        var url = Text(args, "url");
        using var response = await GetAsync(new(url), ct);
        const long maximum = 256L * 1024 * 1024;
        if (response.Content.Headers.ContentLength > maximum) throw new InvalidDataException("工具下载超过 256 MB，请选择必要的工具文件。");
        Directory.CreateDirectory(Path.GetDirectoryName(path)!);
        var temporary = path + ".partial";
        try
        {
            await using (var output = File.Create(temporary))
            await using (var input = await response.Content.ReadAsStreamAsync(ct))
            {
                var buffer = new byte[128 * 1024];
                long total = 0;
                int count;
                while ((count = await input.ReadAsync(buffer, ct)) != 0)
                {
                    total += count;
                    if (total > maximum) throw new InvalidDataException("工具下载超过大小限制。");
                    await output.WriteAsync(buffer.AsMemory(0, count), ct);
                }
            }
            File.Move(temporary, path, true);
        }
        finally { if (File.Exists(temporary)) File.Delete(temporary); }
        var sha256 = await LocalFiles.HashFileAsync(path, ct);
        if (relative.StartsWith("src/tools/", StringComparison.Ordinal))
        {
            var manifest = Path.Combine(SourceDirectory, "tools", "sources.json");
            var sources = await LocalFiles.ReadJsonAsync<Dictionary<string, ToolSource>>(manifest, ct) ?? [];
            sources[relative["src/tools/".Length..]] = new(url, sha256);
            await LocalFiles.WriteJsonAsync(manifest, sources, ct);
        }
        if (release is not null) RegisterReleaseFiles([release]);
        return new { path = relative, sha256, bytes = new FileInfo(path).Length,
            releaseRegistered = session.ReleaseFiles?.Any(file => file.Path == AdapterReleaseFiles.PackagePath(relative)) == true };
    }

    private async Task<HttpResponseMessage> GetAsync(Uri uri, CancellationToken ct)
    {
        for (var redirect = 0; redirect < 6; redirect++)
        {
            if (uri.Scheme != "https" || !string.IsNullOrEmpty(uri.UserInfo) || uri.IsLoopback
                || uri.HostNameType != UriHostNameType.Dns || !uri.Host.Contains('.')
                || uri.Host.EndsWith(".local", StringComparison.OrdinalIgnoreCase))
                throw new InvalidDataException("资料工具只允许无凭据的公共 HTTPS 地址。");
            using var requestMessage = new HttpRequestMessage(HttpMethod.Get, uri);
            requestMessage.Headers.UserAgent.ParseAdd("AvgTranslator/1.0");
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

    private static async Task<string> ReadResponseAsync(HttpResponseMessage response, int maximum, CancellationToken ct)
    {
        if (response.Content.Headers.ContentLength > maximum)
            throw new InvalidDataException($"公开资料超过 {maximum / 1024} KiB 读取上限，请改读具体文件或子目录；未将截断内容当作完整资料。");
        await using var stream = await response.Content.ReadAsStreamAsync(ct);
        using var memory = new MemoryStream();
        var buffer = new byte[8192];
        int count;
        while ((count = await stream.ReadAsync(buffer.AsMemory(0, Math.Min(buffer.Length, maximum + 1 - (int)memory.Length)), ct)) != 0)
        {
            memory.Write(buffer, 0, count);
            if (memory.Length > maximum)
                throw new InvalidDataException($"公开资料超过 {maximum / 1024} KiB 读取上限，请改读具体文件或子目录；未将截断内容当作完整资料。");
        }
        return Encoding.UTF8.GetString(memory.ToArray());
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
        var schema = JsonSerializer.SerializeToElement(new { type = "object", properties = fields, required, additionalProperties = false });
        return new(name, description, schema);
    }
    private sealed record ToolSource(string Url, string Sha256);
}
