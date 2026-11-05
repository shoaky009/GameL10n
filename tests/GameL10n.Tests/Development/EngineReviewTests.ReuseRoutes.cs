using System.Text.Json;
using GameL10n.Core.Adapters;
using GameL10n.Core.Development;
using GameL10n.Core.Storage;

namespace GameL10n.Tests.Development;

public sealed partial class EngineReviewTests
{
    [Fact]
    public async Task KnowledgeOffersConcreteReuseCallsWithoutOpeningPackagePayloadsOrUnrelatedComponents()
    {
        var family = new AdapterFamily("engine", "kirikiri-xp3-adler-filter", "KiriKiri XP3 / Adler filter");
        var entry = await Store.SaveAsync(Entry() with { Family = family, Aliases = ["KiriKiri"] }, null);
        var packages = new LocalAdapterPackageStore(Path.Combine(root, "extensions"));
        var build = Path.Combine(root, "build");
        Directory.CreateDirectory(build);
        await File.WriteAllTextAsync(Path.Combine(build, "Program.cs"), "// reference source");
        await File.WriteAllTextAsync(Path.Combine(build, "adapter.dll"), "reference payload");
        await File.WriteAllTextAsync(Path.Combine(build, "adapter.runtimeconfig.json"), "{}");
        var published = await packages.PublishAndActivateAsync("source-game", new(family.AdapterId, "reference-1", build,
            ["script.ks"], await LocalFiles.HashFileAsync(Path.Combine(build, "Program.cs")),
            await LocalFiles.HashFileAsync(Path.Combine(build, "adapter.dll")), family), []);
        File.Delete(Path.Combine(published.DirectoryPath, "adapter.dll"));
        var project = await ProjectAsync();
        var tools = new AdaptationTools(new("next-game", project, []), new(), Path.Combine(root, "next-work"),
            new(new(Path.Combine(root, "unused"), "10.0.402", "10.0.13"), new NeverRun()), packages);
        using var response = JsonDocument.Parse(await tools.ExecuteAsync(new("knowledge", "inspect_engine_knowledge",
            JsonSerializer.Serialize(new { id = entry.Id }, Json)), default));
        var reuse = response.RootElement.GetProperty("result").GetProperty("reuseOptions");
        var components = reuse.GetProperty("components").EnumerateArray().ToArray();
        Assert.Contains(components, component => component.GetProperty("id").GetString() == "xp3-adler");
        Assert.DoesNotContain(components, component => component.GetProperty("id").GetString() is "kaguya-tblstr" or "sjis-tunnel");
        var copy = Assert.Single(reuse.GetProperty("publishedAdapters").EnumerateArray());
        Assert.True(copy.GetProperty("metadataOnly").GetBoolean());
        Assert.Equal("copy_adapter", copy.GetProperty("copyCall").GetProperty("tool").GetString());
        Assert.Equal("reference-1", copy.GetProperty("copyCall").GetProperty("arguments").GetProperty("version").GetString());
        Assert.NotEmpty(reuse.GetProperty("builtInParsers").EnumerateArray());
        var assets = new AdapterComponentAssets();
        Assert.Empty(assets.Related(["Unity", "Renpy", "Yuris"]));
        Assert.Contains(assets.Related(["KaGuYa / message.dat"]), component =>
            JsonSerializer.SerializeToElement(component, Json).GetProperty("id").GetString() == "kaguya-tblstr");
    }
}
