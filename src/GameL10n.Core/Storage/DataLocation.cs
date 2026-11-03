using System.Text.Json;

namespace GameL10n.Core.Storage;

public static class DataLocation
{
    public static string DefaultDirectory => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "GameL10n");
    private static string LocationFile => Path.Combine(DefaultDirectory, "location.json");
    private static string? EnvironmentOverride => Environment.GetEnvironmentVariable("GAMEL10N_DATA") is { Length: > 0 } configured
        ? configured : Environment.GetEnvironmentVariable("AVG_TRANSLATOR_DATA");
    public static bool IsEnvironmentOverride => EnvironmentOverride is { Length: > 0 };

    public static string Resolve() => Resolve(DefaultDirectory, Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "AvgTranslator"), EnvironmentOverride);

    internal static string Resolve(string defaultDirectory, string legacyDirectory, string? configuredDirectory = null)
    {
        if (configuredDirectory is { Length: > 0 } configured)
            return Path.GetFullPath(configured);
        var root = File.Exists(Path.Combine(defaultDirectory, "location.json"))
            || File.Exists(Path.Combine(defaultDirectory, "location.json.bak"))
            || File.Exists(Path.Combine(defaultDirectory, "settings.json"))
            || Directory.Exists(Path.Combine(defaultDirectory, "games"))
            || !Directory.Exists(legacyDirectory) ? defaultDirectory : legacyDirectory;
        var location = Path.Combine(root, "location.json");
        Exception? failure = null;
        foreach (var file in new[] { location, location + ".bak" })
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
        return root;
    }

    public static Task SaveAsync(string directory)
    {
        directory = Path.GetFullPath(directory);
        if (IsEnvironmentOverride)
        {
            // Keep development/portable sessions isolated from the user's default location file.
            Environment.SetEnvironmentVariable("GAMEL10N_DATA", directory);
            return Task.CompletedTask;
        }
        return LocalFiles.WriteJsonAsync(LocationFile, new { dataDirectory = directory });
    }
}
