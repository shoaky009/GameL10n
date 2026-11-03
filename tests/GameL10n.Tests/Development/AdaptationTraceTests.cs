using System.Text.Json;
using GameL10n.Core.Development;

namespace GameL10n.Tests.Development;

public sealed class AdaptationTraceTests
{
    [Fact]
    public void SelectorsRetainLocationWithoutPayloadsOrUrlSecrets()
    {
        var args = JsonSerializer.Serialize(new
        {
            path = "work/probe.py", offset = 512, content = "private-source", stdin = "private-stdin",
            query = "private-dialogue", arguments = new[] { "-Command", "private-command" },
            resourcePatterns = new[] { "*.pak", "*.exe" },
            url = "https://user:password@example.com/source.py?token=secret#fragment"
        });
        var result = JsonSerializer.Serialize(AdaptationTrace.Call(new("call-a", "write_file", args)));
        Assert.Contains("work/probe.py", result);
        Assert.Contains("https://example.com/source.py", result);
        foreach (var value in new[] { "private-source", "private-stdin", "private-dialogue", "private-command", "password", "secret", "fragment" })
            Assert.DoesNotContain(value, result);
        Assert.Contains("argumentFingerprint", result);
        using var json = JsonDocument.Parse(result);
        Assert.Equal(new[] { "*.pak", "*.exe" }, json.RootElement.GetProperty("selectors")
            .GetProperty("resourcePatterns").EnumerateArray().Select(item => item.GetString()));
    }

    [Theory]
    [InlineData("invalid")]
    [InlineData("[]")]
    public void InvalidArgumentsCanStillBeDiagnosed(string args)
    {
        Assert.Contains("malformedArguments", JsonSerializer.Serialize(AdaptationTrace.Call(new("call-a", "read_file", args))));
    }
}
