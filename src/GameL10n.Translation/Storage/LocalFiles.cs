using System.Security.Cryptography;
using System.Text;
using System.Text.Json;

namespace GameL10n.Core.Storage;

public static class LocalFiles
{
    public static JsonSerializerOptions JsonOptions { get; } = new(JsonSerializerDefaults.Web)
    {
        WriteIndented = true
    };

    public static string HashText(string text) => Convert.ToHexString(SHA256.HashData(Encoding.UTF8.GetBytes(text))).ToLowerInvariant();

    public static async Task<string> HashFileAsync(string path, CancellationToken ct = default)
    {
        await using var stream = File.OpenRead(path);
        return Convert.ToHexString(await SHA256.HashDataAsync(stream, ct)).ToLowerInvariant();
    }

    public static async Task<T?> ReadJsonAsync<T>(string path, CancellationToken ct = default)
    {
        if (!File.Exists(path)) return default;
        await using var stream = File.OpenRead(path);
        return await JsonSerializer.DeserializeAsync<T>(stream, JsonOptions, ct);
    }

    public static async Task<T?> ReadRecoverableJsonAsync<T>(string path, CancellationToken ct = default,
        Func<T, bool>? validate = null)
    {
        async Task<T?> ReadValidAsync(string candidate)
        {
            var value = await ReadJsonAsync<T>(candidate, ct);
            if (value is null || (validate is not null && !validate(value)))
                throw new InvalidDataException("持久化数据校验失败：" + Path.GetFileName(candidate));
            return value;
        }
        if (!File.Exists(path) && !File.Exists(path + ".bak")) return default;
        try { return await ReadValidAsync(path); }
        catch (Exception error) when (error is JsonException or InvalidDataException or FileNotFoundException)
        {
            if (!File.Exists(path + ".bak")) throw;
            var recovered = await ReadValidAsync(path + ".bak");
            // Repair the primary without rotating its damaged bytes over the good backup.
            await WriteJsonAsync(path, recovered, ct);
            return recovered;
        }
    }

    public static async Task WriteJsonAsync<T>(string path, T value, CancellationToken ct = default)
        => await WriteAtomicJsonAsync(path, value, false, ct);

    public static async Task WriteRecoverableJsonAsync<T>(string path, T value, CancellationToken ct = default)
        => await WriteAtomicJsonAsync(path, value, true, ct);

    private static async Task WriteAtomicJsonAsync<T>(string path, T value, bool keepBackup, CancellationToken ct)
    {
        Directory.CreateDirectory(Path.GetDirectoryName(Path.GetFullPath(path))!);
        var temporary = path + "." + Guid.NewGuid().ToString("N") + ".tmp";
        try
        {
            await using (var stream = new FileStream(temporary, FileMode.CreateNew, FileAccess.Write, FileShare.None,
                65536, FileOptions.WriteThrough | FileOptions.Asynchronous))
            {
                await JsonSerializer.SerializeAsync(stream, value, JsonOptions, ct);
                await stream.FlushAsync(ct);
                stream.Flush(true);
            }
            ct.ThrowIfCancellationRequested();
            if (keepBackup && File.Exists(path)) File.Replace(temporary, path, path + ".bak", true);
            else File.Move(temporary, path, true);
        }
        finally { if (File.Exists(temporary)) File.Delete(temporary); }
    }

    public static string UnderRoot(string root, string relativePath)
    {
        if (string.IsNullOrWhiteSpace(relativePath) || Path.IsPathRooted(relativePath) || relativePath.Contains(':'))
            throw new InvalidDataException("文件路径必须是相对路径。");
        var fullRoot = Path.GetFullPath(root).TrimEnd(Path.DirectorySeparatorChar) + Path.DirectorySeparatorChar;
        var fullPath = Path.GetFullPath(Path.Combine(fullRoot, relativePath));
        if (!fullPath.StartsWith(fullRoot, StringComparison.OrdinalIgnoreCase))
            throw new InvalidDataException("文件路径超出工作目录。");
        var current = fullPath;
        while (current.Length >= fullRoot.TrimEnd(Path.DirectorySeparatorChar).Length)
        {
            if ((File.Exists(current) || Directory.Exists(current)) &&
                (File.GetAttributes(current) & FileAttributes.ReparsePoint) != 0)
                throw new InvalidDataException("不支持通过目录链接或符号链接访问资源。");
            current = Path.GetDirectoryName(current) ?? "";
        }
        return fullPath;
    }
}
