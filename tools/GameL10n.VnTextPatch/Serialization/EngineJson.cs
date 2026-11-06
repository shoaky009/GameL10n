using System.Text.Json;
using System.Text.Json.Nodes;
using System.Text.Json.Serialization.Metadata;
using GameL10n.VnTextPatch;

namespace GameL10n.Core.Serialization;

/// <summary>Static JSON metadata for the fixed read-only inspectors; never loads adapter assemblies.</summary>
internal static class EngineJson
{
    internal static JsonSerializerOptions Web => VnTextPatchJson.Web;
    internal static JsonNode? Node<T>(T value, JsonSerializerOptions? options = null) => VnTextPatchJson.Node(value, options);
    internal static JsonNode? Node(object? value, JsonSerializerOptions? options = null) => VnTextPatchJson.Node(value, options);
    internal static JsonArray? Array(IEnumerable<JsonNode?>? values) => VnTextPatchJson.Array(values);
}
