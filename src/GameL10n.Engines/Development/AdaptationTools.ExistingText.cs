using System.Text.Json;
using System.Text.Json.Nodes;
using GameL10n.Core.Serialization;

namespace GameL10n.Core.Development;

public sealed partial class AdaptationTools
{
    private object ReadBaselineUnits(JsonElement args)
    {
        var units = request.BaselineUnits;
        if (units is null)
            return new JsonObject
            {
                ["available"] = false,
                ["message"] = "没有已保存的提取基线；请复用现有适配器的提取接口，保持其 ID 和上下文规则。"
            };
        var resource = Text(args, "resourcePath", "").Replace('\\', '/');
        var offset = Number(args, "offset", 0, 0, int.MaxValue);
        var limit = Number(args, "limit", 10, 1, 20);
        var selected = resource.Length == 0 ? units : units.Where(unit =>
            unit.ResourcePath.Replace('\\', '/').Equals(resource, StringComparison.OrdinalIgnoreCase)).ToArray();
        // The full baseline stays local. Return only a page, with verbatim cache/identity fields.
        var page = selected.Skip(offset).Take(limit).ToArray();
        var next = Math.Min((long)offset + page.Length, selected.Count);
        return new JsonObject
        {
            ["available"] = true,
            ["total"] = selected.Count,
            ["offset"] = offset,
            ["nextOffset"] = next,
            ["more"] = next < selected.Count,
            ["units"] = EngineJson.Node(page, EngineJson.Web),
            ["resources"] = EngineJson.Node(units.Select(unit => unit.ResourcePath).Distinct(StringComparer.OrdinalIgnoreCase).Take(40).ToArray(), EngineJson.Web)
        };
    }
}
