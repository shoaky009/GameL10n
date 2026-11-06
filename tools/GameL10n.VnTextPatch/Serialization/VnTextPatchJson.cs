using System.Text.Json;
using System.Text.Json.Nodes;
using System.Text.Json.Serialization;
using System.Text.Json.Serialization.Metadata;

namespace GameL10n.VnTextPatch;

internal static class VnTextPatchJson
{
    public static IJsonTypeInfoResolver Resolver { get; } = JsonTypeInfoResolver.Combine(VnTextPatchJsonContext.Default);
    public static JsonSerializerOptions Default { get; } = new() { TypeInfoResolver = Resolver };
    public static JsonSerializerOptions Web { get; } = new(JsonSerializerDefaults.Web) { TypeInfoResolver = Resolver };
    public static JsonSerializerOptions Storage { get; } = new(Web) { WriteIndented = true };

    public static JsonTypeInfo<T> TypeInfo<T>(JsonSerializerOptions? options = null) =>
        (JsonTypeInfo<T>)(options ?? Storage).GetTypeInfo(typeof(T));

    public static JsonNode? Node<T>(T value, JsonSerializerOptions? options = null) =>
        value is JsonNode node ? node.DeepClone() : JsonSerializer.SerializeToNode(value, TypeInfo<T>(options ?? Web));

    public static JsonNode? Node(object? value, JsonSerializerOptions? options = null) =>
        value is null ? null : value is JsonNode node ? node.DeepClone()
            : JsonSerializer.SerializeToNode(value, (options ?? Web).GetTypeInfo(value.GetType()));

    public static JsonArray? Array(IEnumerable<JsonNode?>? values) => values is null ? null : new(values.ToArray());
}

[JsonSourceGenerationOptions(GenerationMode = JsonSourceGenerationMode.Metadata)]
[JsonSerializable(typeof(System.Collections.Generic.Dictionary<int, int>))]
[JsonSerializable(typeof(System.Collections.Generic.Dictionary<string, string>))]
[JsonSerializable(typeof(System.DateTimeOffset))]
[JsonSerializable(typeof(System.Text.Json.JsonElement))]
[JsonSerializable(typeof(System.Text.Json.Nodes.JsonArray))]
[JsonSerializable(typeof(System.Text.Json.Nodes.JsonNode))]
[JsonSerializable(typeof(System.Text.Json.Nodes.JsonObject))]
[JsonSerializable(typeof(System.Text.Json.Nodes.JsonObject[]))]
[JsonSerializable(typeof(bool))]
[JsonSerializable(typeof(double))]
[JsonSerializable(typeof(global::GameL10n.VnTextPatch.Request))]
[JsonSerializable(typeof(global::GameL10n.VnTextPatch.Response))]
[JsonSerializable(typeof(global::VNTextPatch.Shared.Scripts.Yuris.YurisTextSource))]
[JsonSerializable(typeof(global::VNTextPatch.Shared.Scripts.Yuris.YurisAttributeSource))]
[JsonSerializable(typeof(int))]
[JsonSerializable(typeof(int[]))]
[JsonSerializable(typeof(long))]
[JsonSerializable(typeof(long[]))]
[JsonSerializable(typeof(object))]
[JsonSerializable(typeof(object[]))]
[JsonSerializable(typeof(string))]
[JsonSerializable(typeof(string[]))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.UnitySchemaInspection.Field[]))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.UnitySchemaInspection.TextMethod[]))]
[JsonSerializable(typeof(global::GameL10n.Core.Validation.ExecutableAnalysis))]
[JsonSerializable(typeof(global::System.Collections.Generic.IReadOnlyDictionary<int, int>))]
internal partial class VnTextPatchJsonContext : JsonSerializerContext;
