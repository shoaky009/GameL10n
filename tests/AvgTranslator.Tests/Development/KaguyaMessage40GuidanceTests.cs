using System.Text;
using System.Text.Json;
using AvgTranslator.Core.Development;

namespace AvgTranslator.Tests.Development;

public sealed class KaguyaMessage40GuidanceTests : IDisposable
{
    private readonly string root = Path.Combine(Path.GetTempPath(), "avg-kaguya-guidance-" + Guid.NewGuid().ToString("N"));
    private static readonly JsonSerializerOptions Json = new(JsonSerializerDefaults.Web);

    [Fact]
    public void AddressHintsRequireBothExactRuntimeHashAndMessage40Signature()
    {
        var known = JsonSerializer.SerializeToElement(KaguyaMessage40Guidance.MatchRuntime(KaguyaMessage40Guidance.GraphicsSha256.ToLowerInvariant(), true), Json);
        Assert.Equal("0x3AD8A", known.GetProperty("crtIsmbbleadCallRva").GetString());
        Assert.Equal("not-tested", known.GetProperty("runtimeStatus").GetString());
        Assert.Null(KaguyaMessage40Guidance.MatchRuntime(new string('0', 64), true));
        Assert.Null(KaguyaMessage40Guidance.MatchRuntime(KaguyaMessage40Guidance.GraphicsSha256, false));
    }

    [Fact]
    public async Task InspectionUsesOnlySuppliedMessageSamplesAndTheirAdjacentRuntime()
    {
        string scripts = Directory.CreateDirectory(Path.Combine(root, "Data", "Scripts")).FullName;
        await File.WriteAllBytesAsync(Path.Combine(scripts, "message.dat"), "[SCR-MESSAGE]ver4.0"u8.ToArray());
        string libraries = Directory.CreateDirectory(Path.Combine(root, "Data", "Lib")).FullName;
        await File.WriteAllBytesAsync(Path.Combine(libraries, "Graphics.dll"), Encoding.ASCII.GetBytes("unknown runtime"));
        string other = Directory.CreateDirectory(Path.Combine(root, "unrelated")).FullName;
        await File.WriteAllBytesAsync(Path.Combine(other, "Graphics.dll"), Encoding.ASCII.GetBytes("must not be inspected"));
        var inspected = JsonSerializer.SerializeToElement(KaguyaMessage40Guidance.Inspect(root, ["Data/Scripts/message.dat"]), Json);
        Assert.Equal("message40-only", inspected.GetProperty("status").GetString());
        Assert.Equal("Data/Lib/Graphics.dll", Assert.Single(inspected.GetProperty("graphicsLibraries").EnumerateArray()).GetProperty("path").GetString());
        Assert.Equal(JsonValueKind.Null, inspected.GetProperty("knownRuntime").ValueKind);
        Assert.DoesNotContain("0x3AD8A", inspected.GetRawText());

        var noSamples = JsonSerializer.SerializeToElement(KaguyaMessage40Guidance.Inspect(root, []), Json);
        Assert.Empty(noSamples.GetProperty("graphicsLibraries").EnumerateArray());
        await File.WriteAllTextAsync(Path.Combine(scripts, "message.dat"), "not MESSAGE v4.0");
        var wrongSignature = JsonSerializer.SerializeToElement(KaguyaMessage40Guidance.Inspect(root, ["Data/Scripts/message.dat"]), Json);
        Assert.Empty(wrongSignature.GetProperty("graphicsLibraries").EnumerateArray());
        Assert.Equal(JsonValueKind.Null, wrongSignature.GetProperty("knownRuntime").ValueKind);
    }

    public void Dispose()
    {
        if (Directory.Exists(root)) Directory.Delete(root, true);
    }
}
