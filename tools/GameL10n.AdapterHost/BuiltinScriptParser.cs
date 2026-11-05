using System.Diagnostics;
using System.Text;
using System.Text.Json.Nodes;

internal static class BuiltinScriptParser
{
    private static string[] command = [];
    public static string Format { get; private set; } = "";

    public static void Configure(JsonObject request, string format)
    {
        Format = format;
        var resolvedCommand = request["toolPaths"]?["vnTextPatchCommand"]
            ?? JsonNode.Parse(Environment.GetEnvironmentVariable("GAMEL10N_VNTEXTPATCH_COMMAND") ?? "null");
        command = resolvedCommand?.AsArray()
            .Select(value => value!.GetValue<string>()).ToArray() ?? [];
        if (command.Length is < 1 or > 16 || command.Sum(value => value.Length) > 32 * 1024
            || !Path.IsPathFullyQualified(command[0]) || !File.Exists(command[0]))
            throw new InvalidDataException("缺少已解析的固定版本脚本工具命令，请使用完整发行包加载适配器。");
    }

    public static string ExecuteJson(string input) => ExecuteAsync(input).GetAwaiter().GetResult();

    private static async Task<string> ExecuteAsync(string input)
    {
        var request = JsonNode.Parse(input)!.AsObject();
        if (request["protocolVersion"]?.GetValue<int>() != 1)
            throw new InvalidDataException("不支持的脚本解析器协议版本。");
        if (request["operation"]?.GetValue<string>() == "probe")
            throw new InvalidDataException("probe 只能使用文件头或有界范围读取，不能调用整文件脚本解析器。");
        if (command.Length == 0) throw new InvalidDataException("脚本工具尚未配置。");
        var scratch = Path.Combine(Directory.GetCurrentDirectory(), "parser-" + Guid.NewGuid().ToString("N"));
        var source = request["sourceDirectory"]?.GetValue<string>()
            ?? throw new InvalidDataException("缺少脚本资源目录。");
        RequireSeparate(source, scratch);
        Directory.CreateDirectory(scratch);
        try
        {
            var responseFile = Path.Combine(scratch, "response.json");
            foreach (var name in new[] { "sourceDirectory", "outputDirectory", "translationsFile" })
                if (request[name]?.GetValue<string>() is { } value) request[name] = Path.GetFullPath(value);
            request["format"] = Format;
            // Intermediate template calls must never overwrite the adapter's final response.
            request["responseFile"] = responseFile;
            request.Remove("toolPaths");
            var start = new ProcessStartInfo(command[0])
            {
                WorkingDirectory = scratch, UseShellExecute = false, CreateNoWindow = true,
                RedirectStandardInput = true, RedirectStandardOutput = true, RedirectStandardError = true,
                StandardInputEncoding = new UTF8Encoding(false), StandardOutputEncoding = Encoding.UTF8, StandardErrorEncoding = Encoding.UTF8
            };
            foreach (var arg in command.Skip(1)) start.ArgumentList.Add(arg);
            if (Path.GetFileNameWithoutExtension(command[0]).Equals("dotnet", StringComparison.OrdinalIgnoreCase))
                start.Environment["DOTNET_ROOT"] = Path.GetDirectoryName(command[0]);
            using var process = new Process { StartInfo = start };
            if (!process.Start()) throw new InvalidDataException("固定版本的脚本工具未能启动。");
            using var lifetime = new CancellationTokenSource(TimeSpan.FromMinutes(10));
            void Stop()
            {
                try { if (!process.HasExited) process.Kill(entireProcessTree: true); }
                catch (Exception error) when (error is InvalidOperationException or System.ComponentModel.Win32Exception) { Debug.WriteLine(error); }
            }
            using var registration = lifetime.Token.Register(Stop);
            try
            {
                var output = ReadAsync(process.StandardOutput, lifetime);
                var error = ReadAsync(process.StandardError, lifetime);
                await Task.WhenAll(output, error, WriteAsync(), process.WaitForExitAsync(lifetime.Token));
                if (!string.IsNullOrWhiteSpace(error.Result)) Console.Error.WriteLine(error.Result);
                if (!File.Exists(responseFile))
                    throw new InvalidDataException("脚本工具未返回响应文件（退出码 " + process.ExitCode + "）。" + Clip(error.Result));
                if ((File.GetAttributes(responseFile) & FileAttributes.ReparsePoint) != 0
                    || new FileInfo(responseFile).Length > 512L * 1024 * 1024)
                    throw new InvalidDataException("脚本工具的响应文件无效或超过大小限制。");
                var result = await File.ReadAllTextAsync(responseFile, Encoding.UTF8, lifetime.Token);
                var response = JsonNode.Parse(result)?.AsObject() ?? throw new InvalidDataException("脚本工具返回了空结果。");
                if (response["protocolVersion"]?.GetValue<int>() != 1 || response["success"] is null)
                    throw new InvalidDataException("脚本工具返回了不兼容的协议结果。");
                if (process.ExitCode != 0 && response["success"]!.GetValue<bool>())
                    throw new InvalidDataException("脚本工具异常退出（退出码 " + process.ExitCode + "）。" + Clip(error.Result));
                return result;

                async Task WriteAsync()
                {
                    try
                    {
                        await process.StandardInput.WriteAsync(request.ToJsonString().AsMemory(), lifetime.Token);
                        process.StandardInput.Close();
                    }
                    catch { lifetime.Cancel(); throw; }
                }
            }
            catch (OperationCanceledException error) when (lifetime.IsCancellationRequested)
            { throw new InvalidDataException("脚本工具运行超时或输出超过限制，进程已停止。", error); }
            finally
            {
                Stop();
                try { await process.WaitForExitAsync().WaitAsync(TimeSpan.FromSeconds(5)); }
                catch (Exception error) when (error is InvalidOperationException or TimeoutException) { Debug.WriteLine(error); }
            }
        }
        finally
        {
            try { Directory.Delete(scratch, recursive: true); }
            catch (Exception error) when (error is IOException or UnauthorizedAccessException) { Console.Error.WriteLine(error); }
        }
    }

    private static async Task<string> ReadAsync(StreamReader reader, CancellationTokenSource lifetime)
    {
        var result = new StringBuilder();
        var buffer = new char[4096];
        long bytes = 0;
        try
        {
            int count;
            while ((count = await reader.ReadAsync(buffer, lifetime.Token)) != 0)
            {
                bytes += Encoding.UTF8.GetByteCount(buffer.AsSpan(0, count));
                if (bytes > 1024 * 1024) throw new InvalidDataException("脚本工具标准输出或诊断超过大小限制。");
                result.Append(buffer, 0, count);
            }
            return result.ToString();
        }
        catch { lifetime.Cancel(); throw; }
    }

    private static string Clip(string value) => value.Length <= 4000 ? value : value[^4000..];

    private static void RequireSeparate(string source, string scratch)
    {
        var root = Path.GetFullPath(source).TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar)
            + Path.DirectorySeparatorChar;
        var comparison = OperatingSystem.IsWindows() ? StringComparison.OrdinalIgnoreCase : StringComparison.Ordinal;
        if ((scratch.TrimEnd(Path.DirectorySeparatorChar) + Path.DirectorySeparatorChar).StartsWith(root, comparison))
            throw new InvalidDataException("脚本工具必须在独立工作目录运行，不能在原始游戏目录创建临时文件。");
    }
}
