using System.Text.Json;
using GameL10n.Core.Isolation;
using System.Text.Json.Nodes;
using GameL10n.Core.Serialization;

namespace GameL10n.Core.Development;

public sealed record EngineArchiveComponent(string Id, int Version, string Format, string Module,
    IReadOnlyList<string> Operations, IReadOnlyList<string> Limitations);

/// <summary>Fixed entry points over algorithms in the shared, hash-pinned library.</summary>
public static class EngineArchiveComponents
{
    public const string ScriptPath = "tools/engine_archive.py";
    public static IReadOnlyList<EngineArchiveComponent> All { get; } =
    [
        new("archive.rpa3", 1, "RPA/ARC 3.0 single-chunk", "python.archives.rpa", ["inspect", "extract", "rebuild"],
            ["RPA 2/4, multi-chunk entries, scrambled archives and compiled RPYC are unsupported.",
             "Only a data-only pickle index is accepted; executable pickle operations are rejected."]),
        new("archive.xp3", 1, "Standard XP3 v1, filter=none", "python.archives.xp3", ["inspect", "extract", "rebuild"],
            ["Explicit filter=none is required from independent engine evidence; zero flags do not establish this.",
             "Encrypted/filtered, chained, obfuscated and executable-overlay XP3 are unsupported.",
             "Changed archives use standard v1 layout; game patch precedence and runtime compatibility require verification."])
    ];

    public static EngineArchiveComponent Find(string id) => All.FirstOrDefault(item => item.Id == id)
        ?? throw new InvalidDataException("未知引擎组件：" + id);

    public static object Describe(string? id = null) => new JsonObject
    {
        ["components"] = EngineJson.Node((id is null ? All : [Find(id)]).ToArray(), EngineJson.Web),
        ["dependencies"] = EngineJson.Node(new[] { "Shared Python " + EngineReferenceToolkit.PythonVersion,
            "Hash-pinned shared GalTransl library; standard library only, no pip or runtime downloads." }, EngineJson.Web),
        ["limits"] = new JsonObject
        {
            ["archiveMiB"] = 512,
            ["memberMiB"] = 64,
            ["totalDecodedMiB"] = 256,
            ["entries"] = 10000,
            ["memberNamesMiB"] = 2
        },
        ["input"] = "run_engine_component: id, operation, archive (resources/ or work/), output (new work/ directory). "
            + "Optional members or lowercase extensions select extraction; rebuild replacements map exact member names to work/ files. XP3 requires filter=none.",
        ["output"] = "Bounded summary plus work/<output>/manifest.json containing every member's name, length and SHA256. "
            + "extract writes files/; rebuild writes rebuilt/<original filename>, verifies all members, and preserves original bytes when unchanged.",
        ["usage"] = "Stage a real archive, then call the component directly; no Python wrapper or model-generated parser is needed. "
            + "Use try_builtin_adapter on extracted scripts. Component success is structural evidence, not runtime or adapter acceptance."
    };

    public static string Source
    {
        get
        {
            using var stream = typeof(EngineArchiveComponents).Assembly.GetManifestResourceStream("GameL10n.Components.engine_archive.py")
                ?? throw new InvalidDataException("内置封包组件入口缺失。");
            using var reader = new StreamReader(stream);
            return reader.ReadToEnd();
        }
    }
}

public sealed record EngineArchiveRequest(int ProtocolVersion, string Component, string Operation, string Archive,
    string ResponseFile, string? OutputDirectory = null, string? OutputFile = null, string? Filter = null,
    IReadOnlyList<string>? Members = null, IReadOnlyList<string>? Extensions = null,
    IReadOnlyDictionary<string, string>? Replacements = null);

public sealed class EngineArchiveComponentClient(IAdapterProcessRunner runner)
{
    public async Task<JsonElement> InvokeAsync(EnginePythonPaths paths, EngineArchiveRequest request, string scratchDirectory,
        CancellationToken ct = default)
    {
        var component = EngineArchiveComponents.Find(request.Component);
        if (!component.Operations.Contains(request.Operation)) throw new InvalidDataException("不支持的封包组件操作。");
        if (component.Id == "archive.xp3" && request.Filter != "none")
            throw new InvalidDataException("XP3 组件要求根据独立证据明确指定 filter=none，不能仅从标记为零推断。");
        var temporary = Path.Combine(scratchDirectory, ".engine-component-" + Guid.NewGuid().ToString("N"));
        WorkspaceFiles.RejectLinkAncestors(temporary);
        Directory.CreateDirectory(temporary);
        try
        {
            var script = Path.Combine(temporary, "engine_archive.py");
            await File.WriteAllTextAsync(script, EngineArchiveComponents.Source, ct);
            var result = await runner.RunAsync(new(paths.Python, EngineReferenceToolkit.PythonArguments(paths, script, []),
                temporary, JsonSerializer.Serialize(request, EngineJson.TypeInfo<EngineArchiveRequest>(new JsonSerializerOptions(EngineJson.Web))),
                TimeSpan.FromMinutes(5), 1024 * 1024), ct);
            if (!File.Exists(request.ResponseFile))
                throw new InvalidDataException($"封包组件未返回结果（退出码 {result.ExitCode}）："
                    + result.StandardError[..Math.Min(result.StandardError.Length, 1000)]);
            var directory = Path.GetDirectoryName(request.ResponseFile)!;
            var response = WorkspaceFiles.ReadBounded(directory, Path.GetFileName(request.ResponseFile), 8 * 1024 * 1024);
            using var json = JsonDocument.Parse(response);
            var root = json.RootElement;
            if (root.GetProperty("protocolVersion").GetInt32() != 1
                || result.ExitCode != 0 && root.GetProperty("success").GetBoolean())
                throw new InvalidDataException("封包组件返回了无效结果：" + result.StandardError[..Math.Min(result.StandardError.Length, 1000)]);
            return root.Clone();
        }
        finally { GeneratedGameAdapter.DeleteStagedTree(temporary); }
    }
}
