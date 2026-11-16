global using File = GameL10n.VnTextPatch.InputFile;
global using Directory = GameL10n.VnTextPatch.InputDirectory;

using System.IO.Enumeration;
using System.Text;
using System.Text.Json;
using Microsoft.Win32.SafeHandles;

namespace GameL10n.VnTextPatch;

// The parser keeps path-based APIs; input streams own read-only handles supplied by the Rust host.
internal static class InputChannel
{
    private static readonly JsonDocument? Settings = Environment.GetEnvironmentVariable("GAMEL10N_INPUT_HANDLES") is { } path
        ? JsonDocument.Parse(System.IO.File.ReadAllText(path)) : null;
    private static readonly object Mutex = new();
    private static readonly string[] Roots = Settings?.RootElement.GetProperty("roots").EnumerateArray()
        .Select(p => Path.GetFullPath(p.GetString()!).TrimEnd('\\') + "\\").ToArray() ?? [];
    private static readonly FileStream? Requests = Pipe("request", FileAccess.Write);
    private static readonly FileStream? Responses = Pipe("response", FileAccess.Read);
    private static readonly FileStream? Gate = Settings is null ? null
        : System.IO.File.Open(Settings.RootElement.GetProperty("lock").GetString()!, FileMode.Open, FileAccess.ReadWrite, FileShare.ReadWrite);

    private static FileStream? Pipe(string key, FileAccess access) => Settings is null ? null
        : new FileStream(new SafeFileHandle((nint)Settings.RootElement.GetProperty(key).GetInt64(), true), access);

    internal static bool Contains(string path)
    {
        if (Settings is null) return false;
        if (path.StartsWith(@"\\?\UNC\", StringComparison.OrdinalIgnoreCase)) path = @"\\" + path[8..];
        else if (path.StartsWith(@"\\?\", StringComparison.Ordinal)) path = path[4..];
        path = Path.GetFullPath(path);
        return Roots.Any(root => (path.TrimEnd('\\') + "\\").StartsWith(root, StringComparison.OrdinalIgnoreCase));
    }

    internal static JsonElement Query(string operation, string path)
    {
        if (!OperatingSystem.IsWindows()) throw new PlatformNotSupportedException("Input handles require Windows.");
        lock (Mutex)
        {
            var deadline = Environment.TickCount64 + 10000;
            while (true)
            {
                try { Gate!.Lock(0, 1); break; }
                catch (IOException error) when ((error.HResult & 0xffff) == 33 && Environment.TickCount64 < deadline)
                { Thread.Sleep(5); }
            }
            try
            {
                using var payload = new MemoryStream();
                using (var writer = new Utf8JsonWriter(payload))
                {
                    writer.WriteStartObject();
                    writer.WriteNumber("pid", Environment.ProcessId);
                    writer.WriteString("op", operation);
                    writer.WriteString("path", Path.GetFullPath(path));
                    writer.WriteEndObject();
                }
                payload.WriteByte(10);
                Requests!.Write(payload.GetBuffer(), 0, checked((int)payload.Length));
                Requests.Flush();
                Span<byte> header = stackalloc byte[4];
                Responses!.ReadExactly(header);
                var bytes = new byte[System.Buffers.Binary.BinaryPrimitives.ReadUInt32LittleEndian(header)];
                Responses.ReadExactly(bytes);
                using var response = JsonDocument.Parse(bytes);
                if (response.RootElement.TryGetProperty("error", out var error))
                {
                    int code = response.RootElement.GetProperty("code").GetInt32();
                    if (code is 2 or 3) throw new FileNotFoundException(error.GetString(), path);
                    throw new UnauthorizedAccessException(error.GetString());
                }
                return response.RootElement.GetProperty("result").Clone();
            }
            finally { Gate.Unlock(0, 1); }
        }
    }

    internal static bool Exists(string? path, bool directory)
    {
        if (path is null) return false;
        try { return Query("stat", path).GetProperty("directory").GetBoolean() == directory; }
        catch (IOException) { return false; }
        catch (UnauthorizedAccessException) { return false; }
    }
}

internal static class InputFile
{
    public static FileStream OpenRead(string path) => Open(path, FileMode.Open, FileAccess.Read);
    public static FileStream Open(string path, FileMode mode, FileAccess access = FileAccess.ReadWrite)
    {
        if (!InputChannel.Contains(path)) return System.IO.File.Open(path, mode, access);
        if (mode != FileMode.Open || access != FileAccess.Read) throw new UnauthorizedAccessException("Input resources are read-only.");
        var handle = new SafeFileHandle((nint)InputChannel.Query("open", path).GetProperty("handle").GetInt64(), true);
        try { return new FileStream(handle, FileAccess.Read); }
        catch { handle.Dispose(); throw; }
    }
    public static byte[] ReadAllBytes(string path)
    {
        if (!InputChannel.Contains(path)) return System.IO.File.ReadAllBytes(path);
        using var input = OpenRead(path);
        var bytes = new byte[checked((int)input.Length)];
        input.ReadExactly(bytes);
        return bytes;
    }
    public static Task<byte[]> ReadAllBytesAsync(string path) => InputChannel.Contains(path)
        ? Task.FromResult(ReadAllBytes(path)) : System.IO.File.ReadAllBytesAsync(path);
    public static string ReadAllText(string path) => ReadAllText(path, Encoding.UTF8);
    public static string ReadAllText(string path, Encoding encoding)
    {
        using var reader = new StreamReader(OpenRead(path), encoding, true);
        return reader.ReadToEnd();
    }
    public static bool Exists(string? path) => path is not null && InputChannel.Contains(path)
        ? InputChannel.Exists(path, false) : System.IO.File.Exists(path);
    public static FileAttributes GetAttributes(string path) => InputChannel.Contains(path)
        ? (FileAttributes)InputChannel.Query("stat", path).GetProperty("attributes").GetUInt32() : System.IO.File.GetAttributes(path);
    public static void Copy(string source, string destination, bool overwrite)
    {
        if (!InputChannel.Contains(source)) { System.IO.File.Copy(source, destination, overwrite); return; }
        using var input = OpenRead(source);
        using var output = System.IO.File.Open(destination, overwrite ? FileMode.Create : FileMode.CreateNew, FileAccess.Write);
        input.CopyTo(output);
    }
    public static FileStream Create(string path) => System.IO.File.Create(path);
    public static void SetAttributes(string path, FileAttributes attributes) => System.IO.File.SetAttributes(path, attributes);
    public static void WriteAllBytes(string path, byte[] bytes) => System.IO.File.WriteAllBytes(path, bytes);
}

internal static class InputDirectory
{
    public static bool Exists(string? path) => path is not null && InputChannel.Contains(path)
        ? InputChannel.Exists(path, true) : System.IO.Directory.Exists(path);
    public static DirectoryInfo CreateDirectory(string path) => System.IO.Directory.CreateDirectory(path);
    public static IEnumerable<string> EnumerateFiles(string path, string pattern, SearchOption option)
    {
        if (!InputChannel.Contains(path)) return System.IO.Directory.EnumerateFiles(path, pattern, option);
        return EnumerateInputs(path, pattern, option);
    }
    private static IEnumerable<string> EnumerateInputs(string path, string pattern, SearchOption option)
    {
        foreach (var entry in InputChannel.Query("list", path).EnumerateArray())
        {
            string name = entry.GetProperty("name").GetString()!;
            string child = Path.Combine(path, name);
            if (entry.GetProperty("stat").GetProperty("directory").GetBoolean())
            {
                if (option == SearchOption.AllDirectories)
                    foreach (string file in EnumerateInputs(child, pattern, option)) yield return file;
            }
            else if (FileSystemName.MatchesSimpleExpression(pattern, name, true)) yield return child;
        }
    }
}
