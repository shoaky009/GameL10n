using System.Text.Json;
using System.Text.Json.Nodes;
using System.Text.Json.Serialization;
using System.Text.Json.Serialization.Metadata;

namespace GameL10n.Core.Serialization;

public static class EngineJson
{
    public static IJsonTypeInfoResolver Resolver { get; } = JsonTypeInfoResolver.Combine(EngineJsonContext.Default, TranslationJson.Resolver);
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
[JsonSerializable(typeof(global::GameL10n.Core.Adapters.AdapterFamily))]
[JsonSerializable(typeof(global::GameL10n.Core.Adapters.VnTextPatchClient.Response))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.AdaptationObjective?))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.AdaptationProgressNote))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.AdaptationSession))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.AdaptationTools.DownloadReceipt))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.AdaptationTools.TextSearchPage))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.AdapterRecognition))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.AdapterReleaseFile[]))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.AdapterTemplateConfiguration))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.AdapterTemplateDefinition))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.AdapterTemplateDefinition[]))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.AdapterTestStage))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.AdapterToolPaths))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.AdapterVerificationMilestone))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.AgentMessage))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.AutomaticBuiltinTrialReport))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.AutomaticTemplateReport))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.BuiltinAdapterDiscoveryReport))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.BuiltinAdapterProbeObservation[]))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.BuiltinAdapterTrialResult))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.BuiltinAdapterTrialSample[]))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.BuiltinParserDependency))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.EngineArchiveComponent[]))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.EngineArchiveRequest))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.EngineKnowledgeEntry))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.EngineKnowledgeOrigin[]))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.EngineKnowledgeStore.KnowledgeMetadata))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.EnginePythonDependency))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.EngineReferencePin))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.EngineReferenceToolkit.PythonReceipt))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.EngineReviewResult))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.EngineReviewService.ReviewDraft))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.GameLaunchInspection))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.GeneratedAdapterPackage))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.GeneratedGameAdapter.CliResponse))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.GeneratedGameAdapter.ProbeFile[]))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.LocalAdapterPackageStore.Activation))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.LocalAdapterPackageStore.CompatibilityCatalog))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.LocalAdapterPackageStore.CompatibilityRecord))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.LocalAdapterPackageStore.IndexPackage))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.LocalAdapterPackageStore.LibraryIndex))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.LocalAdapterPackageStore.LibraryState))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.PinnedDotNetToolchain))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.ProbeRange[]))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.SuccessfulAnalysisProbe[]))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.TextCoverage))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.TextCoverageSampleCorrection[]))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.TextCoverageSample[]))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.UnitySchemaInspection.Field[]))]
[JsonSerializable(typeof(global::GameL10n.Core.Development.UnitySchemaInspection.TextMethod[]))]
[JsonSerializable(typeof(global::GameL10n.Core.GameProbe))]
[JsonSerializable(typeof(global::GameL10n.Core.TokenUsage))]
[JsonSerializable(typeof(global::GameL10n.Core.TranslationUnit[]))]
[JsonSerializable(typeof(global::System.Collections.Generic.Dictionary<string, global::GameL10n.Core.Development.AdaptationTools.ToolSource>))]
[JsonSerializable(typeof(global::System.Collections.Generic.Dictionary<string, object>))]
[JsonSerializable(typeof(global::System.Collections.Generic.Dictionary<string, string[][]>))]
[JsonSerializable(typeof(global::System.Collections.Generic.IEnumerable<global::System.Text.Json.Nodes.JsonObject>))]
[JsonSerializable(typeof(global::System.Collections.Generic.IReadOnlyDictionary<string, string>))]
[JsonSerializable(typeof(global::System.Collections.Generic.IReadOnlyList<global::GameL10n.Core.Development.AgentToolCall>))]
[JsonSerializable(typeof(global::System.Collections.Generic.IReadOnlyList<global::GameL10n.Core.Development.AgentToolDefinition>))]
[JsonSerializable(typeof(global::System.Collections.Generic.IReadOnlyList<global::GameL10n.Core.Development.TextCoverageSample>))]
[JsonSerializable(typeof(global::System.Collections.Generic.IReadOnlyList<global::GameL10n.Core.TranslationUnit>))]
[JsonSerializable(typeof(global::System.Collections.Generic.IReadOnlyList<string>))]
[JsonSerializable(typeof(global::System.Collections.Generic.KeyValuePair<string, string>[]))]
[JsonSerializable(typeof(global::System.Collections.Generic.List<string>))]
[JsonSerializable(typeof(global::System.Text.Json.JsonElement?))]
[JsonSerializable(typeof(global::System.Text.Json.JsonElement[]))]
[JsonSerializable(typeof(int))]
[JsonSerializable(typeof(int[]))]
[JsonSerializable(typeof(long))]
[JsonSerializable(typeof(long[]))]
[JsonSerializable(typeof(object))]
[JsonSerializable(typeof(object[]))]
[JsonSerializable(typeof(string))]
[JsonSerializable(typeof(string[]))]
internal partial class EngineJsonContext : JsonSerializerContext;
