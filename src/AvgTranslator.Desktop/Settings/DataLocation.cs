using System.Text.Json;

namespace AvgTranslator.Desktop.Settings;

internal static class DataLocation
{
    public static string DefaultDirectory => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "AvgTranslator");
    private static string LocationFile => Path.Combine(DefaultDirectory, "location.json");
    public static bool IsEnvironmentOverride => Environment.GetEnvironmentVariable("AVG_TRANSLATOR_DATA") is { Length: > 0 };

    public static string Resolve()
    {
        if (Environment.GetEnvironmentVariable("AVG_TRANSLATOR_DATA") is { Length: > 0 } configured)
            return Path.GetFullPath(configured);
        Exception? failure = null;
        foreach (var file in new[] { LocationFile, LocationFile + ".bak" })
        {
            if (!File.Exists(file)) continue;
            try
            {
                using var json = JsonDocument.Parse(File.ReadAllText(file));
                if (json.RootElement.ValueKind == JsonValueKind.Object && json.RootElement.TryGetProperty("dataDirectory", out var path)
                    && path.ValueKind == JsonValueKind.String && path.GetString() is { Length: > 0 } value)
                    return Path.GetFullPath(value);
                throw new InvalidDataException("数据位置设置无效。");
            }
            catch (Exception error) when (error is JsonException or InvalidDataException or ArgumentException) { failure = error; }
        }
        if (failure is not null) throw new InvalidDataException("数据位置设置无法读取，请检查 location.json 及其 .bak 副本。", failure);
        return DefaultDirectory;
    }

    public static Task SaveAsync(string directory)
    {
        directory = Path.GetFullPath(directory);
        if (IsEnvironmentOverride)
        {
            // Keep development/portable sessions isolated from the user's default location file.
            Environment.SetEnvironmentVariable("AVG_TRANSLATOR_DATA", directory);
            return Task.CompletedTask;
        }
        return Core.Storage.LocalFiles.WriteJsonAsync(LocationFile, new { dataDirectory = directory });
    }
}
