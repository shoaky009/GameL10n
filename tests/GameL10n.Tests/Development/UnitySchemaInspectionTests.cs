using System.Text;
using System.Text.Json;
using GameL10n.Core.Development;

namespace GameL10n.Tests.Development;

public sealed class UnitySchemaInspectionTests : IDisposable
{
    private readonly string directory = Path.Combine(Path.GetTempPath(), "gamel10n-unity-schema-" + Guid.NewGuid().ToString("N"));
    private static readonly JsonSerializerOptions Json = new(JsonSerializerDefaults.Web);

    public UnitySchemaInspectionTests() => Directory.CreateDirectory(directory);

    [Fact]
    public async Task LiteralSearchReturnsStableIndicesAndValidatesPoolBoundsWithoutClassifyingKeys()
    {
        var strings = new[] { "MASTERY_LEVEL", "Mastery\n{0}/30", "Welcome traveller!" };
        var pool = strings.SelectMany(Encoding.UTF8.GetBytes).ToArray();
        var bytes = new byte[280 + pool.Length];
        void W(int offset, int value) => BitConverter.GetBytes(value).CopyTo(bytes, offset);
        W(0, unchecked((int)0xFAB11BAF)); W(4, 31); W(8, 256); W(12, 24); W(16, 280); W(20, pool.Length);
        int at = 0;
        for (int i = 0; i < strings.Length; i++)
        {
            int length = Encoding.UTF8.GetByteCount(strings[i]); W(256 + i * 8, length); W(260 + i * 8, at); at += length;
        }
        pool.CopyTo(bytes, 280);
        var path = Path.Combine(directory, "literals.dat");
        await File.WriteAllBytesAsync(path, bytes);
        var first = JsonSerializer.SerializeToElement(await UnitySchemaInspection.InspectAsync(path, "mastery", 0, 1, mode: "literals"), Json);
        Assert.Equal(2, first.GetProperty("matched").GetInt32());
        Assert.Equal(1, first.GetProperty("nextOffset").GetInt32());
        Assert.Equal("MASTERY_LEVEL", first.GetProperty("literals")[0].GetProperty("text").GetString());
        var second = JsonSerializer.SerializeToElement(await UnitySchemaInspection.InspectAsync(path, "mastery", 1, 1, mode: "literals"), Json);
        Assert.Equal(1, second.GetProperty("literals")[0].GetProperty("index").GetInt32());
        Assert.Equal(JsonValueKind.Null, second.GetProperty("nextOffset").ValueKind);
        Assert.Equal(bytes, await File.ReadAllBytesAsync(path));
        W(276, int.MaxValue);
        await File.WriteAllBytesAsync(path, bytes);
        await Assert.ThrowsAsync<InvalidDataException>(() => UnitySchemaInspection.InspectAsync(path, "mastery", 0, 1, mode: "literals"));
    }

    [Fact]
    public async Task MetadataResolvesDeclaredStructMembersButDoesNotInventArrayLayouts()
    {
        var path = Metadata();
        var before = await File.ReadAllBytesAsync(path);
        var result = JsonSerializer.SerializeToElement(await UnitySchemaInspection.InspectAsync(path, "Skill,WeightEffect", 0, 4), Json);
        var types = result.GetProperty("types").EnumerateArray().ToArray();
        Assert.Equal(2, types.Length);
        var skill = types.Single(type => type.GetProperty("name").GetString() == "Fixture.Skill");
        Assert.Equal(2, skill.GetProperty("methodCount").GetInt32());
        var textMethod = Assert.Single(skill.GetProperty("textMethods").EnumerateArray());
        Assert.Equal("GetTooltipText", textMethod.GetProperty("name").GetString());
        Assert.Equal(0x06000001, textMethod.GetProperty("token").GetInt32());
        var array = skill.GetProperty("fields")[0];
        Assert.Equal(999, array.GetProperty("typeIndex").GetInt32());
        Assert.Equal(JsonValueKind.Null, array.GetProperty("type").ValueKind);
        var weight = types.Single(type => type.GetProperty("name").GetString() == "Fixture.WeightEffect");
        Assert.Equal("struct", weight.GetProperty("kind").GetString());
        Assert.Equal(new[] { "Fixture.Action", "Fixture.Metric" }, weight.GetProperty("fields").EnumerateArray().Select(field => field.GetProperty("type").GetString()));
        Assert.Equal(2, result.GetProperty("relatedTypes").GetArrayLength());
        Assert.Equal(before, await File.ReadAllBytesAsync(path));
    }

    [Fact]
    public async Task UnknownVersionsAndBrokenTablesAreNotGuessed()
    {
        var path = Metadata();
        var bytes = await File.ReadAllBytesAsync(path);
        BitConverter.GetBytes(29).CopyTo(bytes, 4);
        await File.WriteAllBytesAsync(path, bytes);
        var unsupported = JsonSerializer.SerializeToElement(await UnitySchemaInspection.InspectAsync(path, "Skill", 0, 4), Json);
        Assert.Equal("unsupported-version", unsupported.GetProperty("status").GetString());
        BitConverter.GetBytes(31).CopyTo(bytes, 4);
        BitConverter.GetBytes(int.MaxValue).CopyTo(bytes, 164);
        await File.WriteAllBytesAsync(path, bytes);
        await Assert.ThrowsAsync<InvalidDataException>(() => UnitySchemaInspection.InspectAsync(path, "Skill", 0, 4));
    }

    [Fact]
    public async Task MethodRangesAreValidatedBeforeReadingDeclarations()
    {
        var path = Metadata();
        var bytes = await File.ReadAllBytesAsync(path);
        BitConverter.GetBytes(int.MaxValue).CopyTo(bytes, 256 + 36);
        await File.WriteAllBytesAsync(path, bytes);
        await Assert.ThrowsAsync<InvalidDataException>(() => UnitySchemaInspection.InspectAsync(path, "Skill", 0, 4));
    }

    [Fact]
    public async Task ManagedMetadataShowsArraysInheritanceAndIgnoredFieldsWithoutExecutingTypes()
    {
        var result = JsonSerializer.SerializeToElement(await UnitySchemaInspection.InspectAsync(
            typeof(UnitySchemaInspectionTests).Assembly.Location, "SchemaSkill", 0, 4), Json);
        var skill = Assert.Single(result.GetProperty("types").EnumerateArray());
        Assert.EndsWith("SchemaBase", skill.GetProperty("baseType").GetString());
        var fields = skill.GetProperty("fields").EnumerateArray().ToArray();
        Assert.EndsWith("SchemaWeight[]", fields.Single(field => field.GetProperty("name").GetString() == "Weights").GetProperty("type").GetString());
        Assert.True(fields.Single(field => field.GetProperty("name").GetString() == "Ignored").GetProperty("notSerialized").GetBoolean());
        Assert.True(fields.Single(field => field.GetProperty("name").GetString() == "Constant").GetProperty("isStatic").GetBoolean());
        Assert.Contains(result.GetProperty("relatedTypes").EnumerateArray(), type => type.GetProperty("name").GetString()!.EndsWith("SchemaWeight"));
        var method = Assert.Single(skill.GetProperty("textMethods").EnumerateArray());
        Assert.Equal("GetDescription", method.GetProperty("name").GetString());
        Assert.Equal(typeof(SchemaSkill).GetMethod("GetDescription")!.MetadataToken, method.GetProperty("token").GetInt32());
    }

    // Fixtures use runtime type indices deliberately different from type-definition row numbers.
    private string Metadata()
    {
        var strings = new MemoryStream();
        int S(string value) { var at = (int)strings.Position; strings.Write(Encoding.UTF8.GetBytes(value + "\0")); return at; }
        var ns = S("Fixture");
        var names = new[] { "Skill", "WeightEffect", "Action", "Metric" }.Select(S).ToArray();
        var fieldNames = new[] { "weights", "action", "metric" }.Select(S).ToArray();
        var methodNames = new[] { "GetTooltipText", "ApplyEffect" }.Select(S).ToArray();
        var bytes = new byte[1024 + strings.Length];
        void I(int at, int value) => BitConverter.GetBytes(value).CopyTo(bytes, at);
        void U(int at, ushort value) => BitConverter.GetBytes(value).CopyTo(bytes, at);
        I(0, unchecked((int)0xFAB11BAF)); I(4, 31);
        I(24, 1024); I(28, (int)strings.Length);
        I(48, 832); I(52, methodNames.Length * 36);
        I(96, 768); I(100, 36); I(160, 256); I(164, 4 * 88);
        for (var i = 0; i < 4; i++)
        {
            var row = 256 + i * 88;
            I(row, names[i]); I(row + 4, ns); I(row + 8, 100 + i); I(row + 16, -1);
            I(row + 32, i == 0 ? 0 : 1); U(row + 68, (ushort)(i == 0 ? 1 : i == 1 ? 2 : 0));
            I(row + 36, i == 0 ? 0 : -1); U(row + 64, (ushort)(i == 0 ? methodNames.Length : 0));
            I(row + 80, i == 0 ? 0 : i == 1 ? 1 : 3);
        }
        for (var i = 0; i < 3; i++) { I(768 + i * 12, fieldNames[i]); I(772 + i * 12, i == 0 ? 999 : 101 + i); }
        for (var i = 0; i < methodNames.Length; i++) { I(832 + i * 36, methodNames[i]); I(856 + i * 36, 0x06000001 + i); }
        strings.ToArray().CopyTo(bytes, 1024);
        var path = Path.Combine(directory, "global-metadata.dat");
        File.WriteAllBytes(path, bytes);
        return path;
    }

    public void Dispose() => Directory.Delete(directory, true);
}

internal class SchemaBase { public string Name = ""; }
internal enum SchemaAction { None }
internal enum SchemaMetric { None }
internal struct SchemaWeight
{
    public SchemaAction Action;
    public SchemaMetric Metric;
    public SchemaWeight() { Action = default; Metric = default; }
}
internal class SchemaSkill : SchemaBase
{
    static SchemaSkill() => throw new Exception("Metadata inspection must never execute game code.");
    public SchemaWeight[] Weights = [];
    [NonSerialized] public int Ignored = 0;
    public const int Constant = 1;
    public string GetDescription() => throw new Exception("Metadata inspection must never invoke text methods.");
}
