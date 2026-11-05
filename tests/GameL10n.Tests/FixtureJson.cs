using System.Text.Json;
using System.Text.Json.Serialization.Metadata;
using GameL10n.Core.Storage;

namespace GameL10n.Tests;

// Fixtures retain reflection so legacy input data is independent of the production JSON contracts.
internal static class FixtureJson
{
    public static JsonSerializerOptions Storage { get; } = new(LocalFiles.JsonOptions)
        { TypeInfoResolver = new DefaultJsonTypeInfoResolver() };

    public static JsonTypeInfo<T> TypeInfo<T>(JsonSerializerOptions? options = null) =>
        (JsonTypeInfo<T>)(options ?? Storage).GetTypeInfo(typeof(T));
}
