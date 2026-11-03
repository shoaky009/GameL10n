using System.Text.Json;

namespace GameL10n.Core.Development;

/// <summary>Tool discovery is additive: engine hints never restrict which operations can be requested.</summary>
internal static class AdaptationToolGroups
{
    private sealed record Group(string Name, string Description, string[] Tools);

    private static readonly Group[] Groups =
    [
        new("reuse", "Local engine knowledge, bundled parsers, archive components and adapter templates",
            ["inspect_engine_knowledge", "copy_engine_knowledge_tools", "inspect_existing_text", "inspect_builtin_adapter",
                "try_builtin_adapter", "copy_adapter", "inspect_engine_component", "run_engine_component", "compose_adapter_template"]),
        new("development", "Adapter protocol, source edits, local probes, verification and publication",
            ["adapter_protocol", "write_file", "replace_text", "remove_file", "run_tool", "run_csharp", "test_adapter",
                "update_release_files", "finish"]),
        new("research", "Shared engine guides/Python, repository search, web pages and extra downloads",
            ["search_engine_references", "inspect_engine_reference", "run_engine_python", "use_engine_reference",
                "search_repositories", "fetch_url", "download_tool"]),
        new("native", "Launch/file diagnostics, PE imports/disassembly and encoding/font/launcher components",
            ["inspect_game_launch", "find_game_file", "inspect_pe_imports", "inspect_pe_code",
                "inspect_adapter_component", "copy_adapter_components"]),
        new("unity", "Unity managed/IL2CPP types, fields, literals and native reference candidates", ["inspect_unity_schema"]),
        new("siglus", "Siglus scene commands, display rules and bundled parser sources", ["inspect_siglus_game"])
    ];

    private static readonly Dictionary<string, string> GroupByTool = Groups
        .SelectMany(group => group.Tools.Select(tool => (Tool: tool, Group: group.Name)))
        .ToDictionary(item => item.Tool, item => item.Group, StringComparer.Ordinal);

    internal static string Parameters { get; } = JsonSerializer.Serialize(new
    {
        groups = new { type = "array", minItems = 1, maxItems = Groups.Length + 1,
            items = new { type = "string", @enum = Groups.Select(group => group.Name).Append("all") } }
    });

    internal static string Context { get; } = """
        Tools are loaded in groups to reduce request size. Basic file/history/staging/progress/checkpoint/blocker tools remain available.
        Capability directory (group: purpose; tool names):
        """ + "\n" + string.Join("\n", Groups.Select(group =>
            $"{group.Name}: {group.Description}; {string.Join(", ", group.Tools)}")) + "\n"
        + "Before using a tool whose schema is absent, call load_tool_groups with one or more group IDs. "
        + "Use groups=[\"all\"] if unsure or a capability seems missing. Call alone; the next request includes the schemas. "
        + "Loading is local, requires no downloads and does not execute the loaded tools. "
        + "Enabled groups persist through checkpoints, restarts and model changes; no phase locks or unloading.\n";

    internal static IReadOnlyList<AgentToolDefinition> ForRequest(AdaptationSession session)
    {
        if (session.EnabledToolGroups.Contains("all")) return AdaptationTools.Definitions;
        // New tools default to visible until deliberately assigned to an optional group.
        return AdaptationTools.Definitions.Where(tool => !GroupByTool.TryGetValue(tool.Name, out var group)
            || session.EnabledToolGroups.Contains(group)).ToArray();
    }

    internal static bool Enable(AdaptationSession session, IEnumerable<string> groups)
    {
        if (session.EnabledToolGroups.Contains("all")) return false;
        var changed = false;
        foreach (var group in groups)
            if (!session.EnabledToolGroups.Contains(group))
            {
                session.EnabledToolGroups.Add(group);
                changed = true;
            }
        return changed;
    }

    internal static void EnableForTools(AdaptationSession session, IEnumerable<string> tools) =>
        Enable(session, tools.Where(GroupByTool.ContainsKey).Select(tool => GroupByTool[tool]));

    internal static object Load(AdaptationSession session, JsonElement args)
    {
        if (!args.TryGetProperty("groups", out var value) || value.ValueKind != JsonValueKind.Array
            || value.GetArrayLength() is < 1 || value.GetArrayLength() > Groups.Length + 1)
            throw new InvalidDataException("groups 必须为非空工具组数组，可使用 all 加载全部工具。");
        var names = value.EnumerateArray().Select(item => item.ValueKind == JsonValueKind.String ? item.GetString() : null).ToArray();
        if (names.Any(name => !IsKnownGroup(name)))
            throw new InvalidDataException("未知工具组。可用组：" + string.Join(", ", Groups.Select(group => group.Name).Append("all")));
        Enable(session, names.Select(name => name!));
        return new { enabledGroups = session.EnabledToolGroups.ToArray(), availableToolCount = ForRequest(session).Count };
    }

    internal static void Restore(AdaptationSession session)
    {
        session.EnabledToolGroups = (session.EnabledToolGroups ?? []).Where(IsKnownGroup).Distinct(StringComparer.Ordinal).ToList();
        EnableForTools(session, session.PendingCalls.Select(call => call.Name));
        // Include archived/provider-only calls before model changes or interrupted batches clear pending calls.
        foreach (var message in session.Messages)
        {
            EnableForTools(session, (message.ToolCalls ?? []).Select(call => call.Name));
            if (message.ProviderData is not { } data) continue;
            if (data.ValueKind == JsonValueKind.Array)
            {
                foreach (var item in data.EnumerateArray())
                    if (String(item, "type") == "function_call" && String(item, "name") is { } name)
                        EnableForTools(session, [name]);
            }
            else if (data.ValueKind == JsonValueKind.Object && data.TryGetProperty("tool_calls", out var calls)
                && calls.ValueKind == JsonValueKind.Array)
                foreach (var call in calls.EnumerateArray())
                    if (call.ValueKind == JsonValueKind.Object && call.TryGetProperty("function", out var function)
                        && String(function, "name") is { } name)
                        EnableForTools(session, [name]);
        }
    }

    private static bool IsKnownGroup(string? name) => name == "all" || Groups.Any(group => group.Name == name);

    private static string? String(JsonElement element, string name) => element.ValueKind == JsonValueKind.Object
        && element.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.String ? value.GetString() : null;
}
