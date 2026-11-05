using System.Text.Json;
using GameL10n.Core.Development;
using GameL10n.VnTextPatch;

namespace GameL10n.Tests.Development;

internal static class ParserToolFixture
{
    public static readonly string[] Files = ["GameL10n.VnTextPatch.dll", "GameL10n.VnTextPatch.deps.json",
        "GameL10n.VnTextPatch.runtimeconfig.json", "VNTextPatch.LICENSE", "VNTextPatch.NOTICE.md"];

    public static BuiltinAdapterAssets Create(string directory)
    {
        Directory.CreateDirectory(directory);
        var source = Path.GetDirectoryName(typeof(Worker).Assembly.Location)!;
        foreach (var name in Files) File.Copy(Path.Combine(source, name), Path.Combine(directory, name));
        var files = Files.ToDictionary(name => name, name => GeneratedAdapterResources.HashFile(Path.Combine(directory, name)));
        File.WriteAllText(Path.Combine(directory, BuiltinAdapterAssets.ToolManifestFile),
            JsonSerializer.Serialize(new BuiltinParserDependency(2, files, "GameL10n.VnTextPatch.dll", "dotnet"), new JsonSerializerOptions(JsonSerializerDefaults.Web)));
        return new(directory);
    }

    public static PinnedDotNetToolchain Toolchain
    {
        get
        {
            var repo = new DirectoryInfo(AppContext.BaseDirectory);
            while (repo is not null && !File.Exists(Path.Combine(repo.FullName, "global.json"))) repo = repo.Parent;
            return new(Path.Combine(repo!.FullName, ".tools", "dotnet"), "10.0.402", "10.0.13");
        }
    }
}
