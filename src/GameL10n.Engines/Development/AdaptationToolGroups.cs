using System.Text.Json;
using System.Text.Json.Nodes;
using GameL10n.Core.Serialization;

namespace GameL10n.Core.Development;

/// <summary>Optional schemas can be selected without restricting which host operations are supported.</summary>
internal static class AdaptationToolGroups
{
    private sealed record Group(string Name, string Description, string[] Tools);

    private static readonly Group[] Groups =
    [
        new("reuse", "Local engine knowledge, bundled parsers, reusable text/font components and adapter templates",
            ["inspect_engine_knowledge", "copy_engine_knowledge_tools", "inspect_existing_text", "inspect_builtin_adapter",
                "try_builtin_adapter", "copy_adapter", "inspect_engine_component", "run_engine_component", "compose_adapter_template",
                "inspect_adapter_component", "copy_adapter_components"]),
        new("development", "Adapter protocol, source edits, local probes, verification and publication",
            ["adapter_protocol", "write_file", "replace_text", "remove_file", "run_tool", "run_engine_python", "test_adapter",
                "update_release_files", "finish"]),
        new("research", "Shared engine guides, Python adapter bridge, repository search, web pages and extra downloads",
            ["search_engine_references", "inspect_engine_reference", "use_engine_reference",
                "search_repositories", "fetch_url", "download_tool"]),
        new("native", "Launch/file diagnostics and PE imports/disassembly",
            ["inspect_game_launch", "find_game_file", "inspect_pe_imports", "inspect_pe_code"]),
        new("unity", "Unity managed/IL2CPP types, fields, literals and native reference candidates", ["inspect_unity_schema"]),
        new("siglus", "Siglus scene commands, display rules and bundled parser sources", ["inspect_siglus_game"])
    ];

    private static readonly Dictionary<string, string> GroupByTool = Groups
        .SelectMany(group => group.Tools.Select(tool => (Tool: tool, Group: group.Name)))
        .ToDictionary(item => item.Tool, item => item.Group, StringComparer.Ordinal);

    internal static string Parameters { get; } = JsonSerializer.Serialize(new JsonObject
    {
        ["groups"] = new JsonObject
        {
            ["type"] = "array",
            ["minItems"] = 0,
            ["maxItems"] = Groups.Length + 1,
            ["items"] = new JsonObject
            {
                ["type"] = "string",
                ["enum"] = EngineJson.Node((Groups.Select(group => group.Name).Append("all")).ToArray(), EngineJson.Default)
            }
        },
        ["mode"] = new JsonObject
        {
            ["type"] = "string",
            ["enum"] = EngineJson.Node(new[] { "add", "replace" }, EngineJson.Default),
            ["default"] = "add"
        }
    }, EngineJson.TypeInfo<JsonObject>(EngineJson.Default));

    internal static string Context { get; } = """
        Tools are loaded in groups to reduce request size. Basic file/history/staging/progress/checkpoint/blocker tools remain available.
        Capability directory (group: purpose; tool names):
        """ + "\n" + string.Join("\n", Groups.Select(group =>
            $"{group.Name}: {group.Description}; {string.Join(", ", group.Tools)}")) + "\n"
        + "Before using a tool whose schema is absent, call load_tool_groups with the relevant group IDs. "
        + "The default mode=add enables more schemas; mode=replace selects only the supplied groups (an empty array keeps basic tools only). "
        + "Use replace when the working set changes; all is available for a broad inspection, not required when unsure. "
        + "Call alone; the next request includes the schemas. "
        + "Loading is local, requires no downloads and does not execute the loaded tools. "
        + "On checkpoint, resume or model change, unused groups are trimmed to current context and recent operations. "
        + "Any group can be loaded again; there are no phase locks.\n";

    internal static IReadOnlyList<AgentToolDefinition> ForRequest(AdaptationSession session)
    {
        // New tools default to visible until deliberately assigned to an optional group.
        return AdaptationTools.Definitions.Where(tool => !GroupByTool.TryGetValue(tool.Name, out var group)
            || session.EnabledToolGroups.Contains(group)).ToArray();
    }

    internal static bool Enable(AdaptationSession session, IEnumerable<string> groups)
    {
        var changed = false;
        foreach (var group in Expand(groups))
            if (!session.EnabledToolGroups.Contains(group))
            {
                session.EnabledToolGroups.Add(group);
                changed = true;
            }
        return changed;
    }

    internal static void EnableForTools(AdaptationSession session, IEnumerable<string> tools) =>
        Enable(session, tools.Where(GroupByTool.ContainsKey).Select(tool => GroupByTool[tool]));

    internal static void SelectForContext(AdaptationSession session, IEnumerable<string> hints,
        IEnumerable<AgentMessage> recent)
    {
        session.EnabledToolGroups.Clear();
        Enable(session, hints);
        // Read only the working tail, not all archived calls. Old research must not keep schemas alive forever.
        foreach (var message in recent.Where(message => message.Role == "assistant").TakeLast(4))
            EnableForTools(session, ToolNames(message));
        EnableForTools(session, session.PendingCalls.Select(call => call.Name));
    }

    internal static object Load(AdaptationSession session, JsonElement args)
    {
        var mode = args.TryGetProperty("mode", out var selectedMode)
            ? selectedMode.ValueKind == JsonValueKind.String ? selectedMode.GetString() : null
            : "add";
        if (mode is not ("add" or "replace")) throw new InvalidDataException("mode 必须为 add 或 replace。");
        if (!args.TryGetProperty("groups", out var value) || value.ValueKind != JsonValueKind.Array
            || value.GetArrayLength() > Groups.Length + 1 || mode == "add" && value.GetArrayLength() == 0)
            throw new InvalidDataException("groups 必须为工具组数组；仅 mode=replace 时允许空数组。");
        var names = value.EnumerateArray().Select(item => item.ValueKind == JsonValueKind.String ? item.GetString() : null).ToArray();
        if (names.Any(name => !IsKnownGroup(name)))
            throw new InvalidDataException("未知工具组。可用组：" + string.Join(", ", Groups.Select(group => group.Name).Append("all")));
        if (mode == "replace") session.EnabledToolGroups.Clear();
        Enable(session, names.Select(name => name!));
        return new JsonObject
        {
            ["enabledGroups"] = EngineJson.Node(session.EnabledToolGroups.ToArray(), EngineJson.Web),
            ["availableToolCount"] = ForRequest(session).Count
        };
    }

    internal static void Restore(AdaptationSession session)
    {
        session.EnabledToolGroups = Expand((session.EnabledToolGroups ?? []).Where(IsKnownGroup))
            .Distinct(StringComparer.Ordinal).ToList();
        EnableForTools(session, session.PendingCalls.Select(call => call.Name));
    }

    private static IEnumerable<string> Expand(IEnumerable<string> groups) => groups.SelectMany(group =>
        group == "all" ? Groups.Select(item => item.Name) : [group]);

    private static IEnumerable<string> ToolNames(AgentMessage message)
    {
        foreach (var call in message.ToolCalls ?? []) yield return call.Name;
        if (message.ProviderData is not { } data) yield break;
        if (data.ValueKind == JsonValueKind.Array)
        {
            foreach (var item in data.EnumerateArray())
                if (String(item, "type") == "function_call" && String(item, "name") is { } name)
                    yield return name;
        }
        else if (data.ValueKind == JsonValueKind.Object && data.TryGetProperty("tool_calls", out var calls)
            && calls.ValueKind == JsonValueKind.Array)
            foreach (var call in calls.EnumerateArray())
                if (call.ValueKind == JsonValueKind.Object && call.TryGetProperty("function", out var function)
                    && String(function, "name") is { } name)
                    yield return name;
    }

    private static bool IsKnownGroup(string? name) => name == "all" || Groups.Any(group => group.Name == name);

    private static string? String(JsonElement element, string name) => element.ValueKind == JsonValueKind.Object
        && element.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.String ? value.GetString() : null;
}
