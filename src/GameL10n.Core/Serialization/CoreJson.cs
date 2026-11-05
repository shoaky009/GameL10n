using System.Text.Json;
using System.Text.Json.Nodes;
using System.Text.Json.Serialization;
using System.Text.Json.Serialization.Metadata;

namespace GameL10n.Core.Serialization;

public static class CoreJson
{
    public static IJsonTypeInfoResolver Resolver { get; } = JsonTypeInfoResolver.Combine(CoreJsonContext.Default, EngineJson.Resolver);
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
[JsonSerializable(typeof(global::GameL10n.Core.Development.GeneratedAdapterPackage))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.PublishedPackageVerification))]
[JsonSerializable(typeof(global::GameL10n.Core.Patching.PatchInstaller.Journal))]
[JsonSerializable(typeof(global::GameL10n.Core.Patching.PatchManifest))]
[JsonSerializable(typeof(global::GameL10n.Core.Pipeline.PatchTranslationSnapshot))]
[JsonSerializable(typeof(global::GameL10n.Core.Pipeline.PipelineProgress))]
[JsonSerializable(typeof(global::GameL10n.Core.Pipeline.PipelineResult))]
[JsonSerializable(typeof(global::GameL10n.Core.Pipeline.SourceFile[]))]
[JsonSerializable(typeof(global::GameL10n.Core.Pipeline.SourceSnapshot))]
[JsonSerializable(typeof(global::GameL10n.Core.Storage.DataDirectoryTransfer.Receipt))]
[JsonSerializable(typeof(global::GameL10n.Core.Storage.ExtractionManifest))]
[JsonSerializable(typeof(global::GameL10n.Core.Storage.ExtractionStore.ResourceExtraction))]
[JsonSerializable(typeof(global::GameL10n.Core.Storage.GameProject))]
[JsonSerializable(typeof(global::GameL10n.Core.Storage.GameProjectStore.ProjectIndex))]
[JsonSerializable(typeof(global::GameL10n.Core.TokenUsage))]
[JsonSerializable(typeof(global::GameL10n.Core.ValidationResult))]
[JsonSerializable(typeof(global::System.Collections.Generic.IReadOnlyList<global::GameL10n.Core.TranslationUnit>))]
[JsonSerializable(typeof(global::System.Collections.Generic.KeyValuePair<string, string>[]))]
[JsonSerializable(typeof(global::System.Collections.Generic.List<global::GameL10n.Core.Validation.ExecutableAnalysis>))]
[JsonSerializable(typeof(global::System.Collections.Generic.List<object>))]
[JsonSerializable(typeof(global::System.Collections.Generic.SortedDictionary<string, string>))]
[JsonSerializable(typeof(global::System.Linq.IOrderedEnumerable<global::GameL10n.Core.Pipeline.SourceFile>))]
[JsonSerializable(typeof(int))]
[JsonSerializable(typeof(int[]))]
[JsonSerializable(typeof(long))]
[JsonSerializable(typeof(long[]))]
[JsonSerializable(typeof(object))]
[JsonSerializable(typeof(object[]))]
[JsonSerializable(typeof(string))]
[JsonSerializable(typeof(string[]))]
internal partial class CoreJsonContext : JsonSerializerContext;
