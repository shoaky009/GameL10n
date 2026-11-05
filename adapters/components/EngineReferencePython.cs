using System;
using System.Diagnostics;
using System.IO;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;
using System.Threading.Tasks;

internal static class EngineReferencePython
{
    public static int Run(string relativeScript, string[] args, Func<string, string> builtinParser)
    {
        using var input = new StreamReader(Console.OpenStandardInput(), new UTF8Encoding(false, true));
        var request = input.ReadToEnd();
        if (args.Length == 1 && args[0] == "--builtin")
        {
            using var output = new StreamWriter(Console.OpenStandardOutput(), new UTF8Encoding(false));
            output.Write(builtinParser(request));
            return 0;
        }
        if (args.Length != 0) throw new InvalidDataException("Unknown adapter entry arguments.");
        var json = JsonNode.Parse(request)!.AsObject();
        var command = new JsonArray(Environment.ProcessPath!);
        if (Path.GetFileNameWithoutExtension(Environment.ProcessPath!).Equals("dotnet", StringComparison.OrdinalIgnoreCase))
            command.Add(typeof(EngineReferencePython).Assembly.Location);
        command.Add("--builtin");
        json["builtinCommand"] = command;
        return Run(relativeScript, json.ToJsonString());
    }
    public static int Run(string relativeScript)
    {
        using var input = new StreamReader(Console.OpenStandardInput(), new UTF8Encoding(false, true));
        var request = input.ReadToEnd();
        return Run(relativeScript, request);
    }
    public static int Run(string relativeScript, string request, bool forwardOutput = true)
    {
        using var json = JsonDocument.Parse(request);
        var paths = json.RootElement.GetProperty("toolPaths");
        var directory = json.RootElement.TryGetProperty("adapterDirectory", out var declared) && declared.ValueKind == JsonValueKind.String
            ? declared.GetString()! : AppContext.BaseDirectory;
        var root = Path.GetFullPath(directory).TrimEnd(Path.DirectorySeparatorChar) + Path.DirectorySeparatorChar;
        var script = Path.GetFullPath(Path.Combine(root, relativeScript));
        if (!script.StartsWith(root, StringComparison.OrdinalIgnoreCase))
            throw new InvalidDataException("Python entry point must be inside the adapter package.");
        var start = new ProcessStartInfo(paths.GetProperty("python").GetString()!)
        {
            UseShellExecute = false, CreateNoWindow = true,
            RedirectStandardInput = true, RedirectStandardOutput = true, RedirectStandardError = true,
            StandardInputEncoding = new UTF8Encoding(false), StandardOutputEncoding = Encoding.UTF8,
            StandardErrorEncoding = Encoding.UTF8
        };
        foreach (var arg in new[] { "-I", "-B", "-X", "utf8", "-c", "import sys,runpy,os; sys.path.insert(0,sys.argv.pop(1)); p=sys.argv.pop(1); sys.path.append(os.path.dirname(p)); sys.argv[0]=p; runpy.run_path(p,run_name='__main__')",
            paths.GetProperty("engineLibrary").GetString()!, script }) start.ArgumentList.Add(arg);
        using var process = Process.Start(start)!;
        var stdout = process.StandardOutput.BaseStream.CopyToAsync(forwardOutput ? Console.OpenStandardOutput() : Console.OpenStandardError());
        var stderr = process.StandardError.BaseStream.CopyToAsync(Console.OpenStandardError());
        process.StandardInput.Write(request);
        process.StandardInput.Close();
        Task.WhenAll(stdout, stderr, process.WaitForExitAsync()).GetAwaiter().GetResult();
        return process.ExitCode;
    }
}
