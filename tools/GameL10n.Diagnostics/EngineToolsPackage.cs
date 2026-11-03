using System.Text.Json;
using GameL10n.Core.Development;
using GameL10n.Core.Storage;

internal static class EngineToolsPackage
{
    public static async Task<int> RunAsync(string cacheDirectory, string outputDirectory)
    {
        try
        {
            var cache = Path.GetFullPath(cacheDirectory);
            var output = Path.GetFullPath(outputDirectory);
            if (cache.Equals(output, StringComparison.OrdinalIgnoreCase)
                || output.StartsWith(cache + Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase)
                || cache.StartsWith(output + Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase))
                throw new ArgumentException("工具缓存与发行目录必须独立。");
            // Network preparation is a packaging action, never part of normal intelligent adaptation.
            var toolkit = new EngineReferenceToolkit(cache, reportProgress: (stage, message) => Console.WriteLine(stage + "：" + message),
                bundledDirectory: Path.Combine(cache, ".no-bundle"), allowDownloads: true);
            var pin = await toolkit.EnsureReferenceAsync(null, default);
            var python = await toolkit.EnsurePythonAsync(pin, null, default);
            Directory.CreateDirectory(output);
            var reference = Path.Combine(output, "references", pin.Revision);
            Directory.CreateDirectory(reference);
            File.Copy(toolkit.LibraryPath(pin), Path.Combine(reference, "library.zip"), true);
            var runtime = Path.GetDirectoryName(python.Paths.Python)!;
            using var receipt = JsonDocument.Parse(await File.ReadAllTextAsync(Path.Combine(runtime, "runtime.json")));
            var runtimeFiles = receipt.RootElement.GetProperty("files").EnumerateObject().Select(file => file.Name).Append("runtime.json");
            foreach (var file in runtimeFiles)
            {
                var target = LocalFiles.UnderRoot(output, Path.Combine("python", EngineReferenceToolkit.PythonVersion, file));
                Directory.CreateDirectory(Path.GetDirectoryName(target)!);
                File.Copy(LocalFiles.UnderRoot(runtime, file), target, true);
            }
            await LocalFiles.WriteJsonAsync(Path.Combine(output, "current.json"), pin);
            var verification = new EngineReferenceToolkit(output, bundledDirectory: Path.Combine(output, ".no-bundle"));
            await verification.EnsureReferenceAsync(pin, default);
            await verification.EnsurePythonAsync(pin, python.Dependency, default);
            Console.WriteLine($"已打包 GalTransl {pin.Revision} 与 Python {EngineReferenceToolkit.PythonVersion}；运行时使用本地固定版本。");
            return 0;
        }
        catch (Exception error) { Console.Error.WriteLine("共享引擎工具打包失败：" + error.Message); return 1; }
    }
}
