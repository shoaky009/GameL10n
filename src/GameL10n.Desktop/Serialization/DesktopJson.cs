using System.Text.Json;
using System.Text.Json.Nodes;
using System.Text.Json.Serialization;
using System.Text.Json.Serialization.Metadata;

using GameL10n.Core.Serialization;

namespace GameL10n.Desktop;

internal static class DesktopJson
{
    public static IJsonTypeInfoResolver Resolver { get; } = JsonTypeInfoResolver.Combine(DesktopJsonContext.Default, CoreJson.Resolver);
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
[JsonSerializable(typeof(global::GameL10n.Core.TokenUsage))]
[JsonSerializable(typeof(global::GameL10n.Desktop.MainWindow.RuntimeCheck))]
[JsonSerializable(typeof(global::GameL10n.Desktop.MainWindow.SavedAdaptationSummary))]
[JsonSerializable(typeof(global::GameL10n.Desktop.MainWindow.SavedTaskState))]
[JsonSerializable(typeof(global::GameL10n.Desktop.MainWindow.TranslationScope))]
[JsonSerializable(typeof(global::GameL10n.Desktop.Settings.DesktopSettings))]
[JsonSerializable(typeof(int))]
[JsonSerializable(typeof(int[]))]
[JsonSerializable(typeof(long))]
[JsonSerializable(typeof(long[]))]
[JsonSerializable(typeof(object))]
[JsonSerializable(typeof(object[]))]
[JsonSerializable(typeof(string))]
[JsonSerializable(typeof(string[]))]
internal partial class DesktopJsonContext : JsonSerializerContext;
