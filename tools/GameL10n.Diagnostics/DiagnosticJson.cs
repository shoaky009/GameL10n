using System.Text.Json;
using System.Text.Json.Serialization.Metadata;
using GameL10n.Core.Storage;

namespace GameL10n.Core.Serialization;

// Developer-only diagnostics can read ad hoc records; this tool is not shipped with the application.
internal static class DiagnosticJson
{
    public static JsonSerializerOptions Storage { get; } = new(LocalFiles.JsonOptions)
        { TypeInfoResolver = new DefaultJsonTypeInfoResolver() };

    public static JsonTypeInfo<T> TypeInfo<T>(JsonSerializerOptions? options = null) =>
        (JsonTypeInfo<T>)(options ?? Storage).GetTypeInfo(typeof(T));
}
