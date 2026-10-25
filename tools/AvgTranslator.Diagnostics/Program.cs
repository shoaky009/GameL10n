using System.Net;
using System.Net.Sockets;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using AvgTranslator.Core.Adapters;
using AvgTranslator.Core.Development;
using AvgTranslator.Core.Isolation;
using AvgTranslator.Core.Storage;

if (args.Length < 3 || args[0] is not ("verify-darkblue" or "sandbox"))
{
    Console.Error.WriteLine("Usage: verify-darkblue <game-directory> <output-directory> | sandbox <portable-dotnet-root> <output-directory>");
    return 1;
}
var output = Path.GetFullPath(args[2]);
var input = Path.GetFullPath(args[1]);
if (output.StartsWith(input.TrimEnd(Path.DirectorySeparatorChar) + Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase) || output.Equals(input, StringComparison.OrdinalIgnoreCase))
    throw new ArgumentException("Diagnostics output must be outside the input directory.");
Directory.CreateDirectory(output);
if (args[0] == "verify-darkblue")
{
    var adapter = new Sfa16AosV2Adapter();
    var initialHash = await LocalFiles.HashFileAsync(Path.Combine(input, "scr.aos"));
    var probe = await adapter.ProbeAsync(input) ?? throw new InvalidDataException("Unsupported game executable or archive.");
    var extracted = await adapter.ExtractAsync(input);
    var original = extracted.Units.ToDictionary(u => u.Id, u => u.SourceText);
    var roundtrip = Path.Combine(output, "roundtrip-" + Guid.NewGuid().ToString("N"));
    await adapter.BuildPatchAsync(input, original, roundtrip);
    var unchanged = initialHash == await LocalFiles.HashFileAsync(Path.Combine(roundtrip, "scr.aos"));
    if (!unchanged) throw new InvalidDataException("No-op build changed bytes.");
    Encoding.RegisterProvider(CodePagesEncodingProvider.Instance);
    var chinese = Encoding.GetEncoding(936, EncoderFallback.ExceptionFallback, DecoderFallback.ExceptionFallback);
    var translations = original.ToDictionary();
    var substitutions = 0;
    foreach (var unit in extracted.Units)
    {
        try { _ = chinese.GetByteCount(unit.SourceText); }
        catch (EncoderFallbackException) { translations[unit.Id] = "测试"; substitutions++; }
    }
    var sample = extracted.Units.First(u => u.Kind == "dialogue" && !u.SourceText.Contains('\\') && !u.SourceText.Contains('@'));
    translations[sample.Id] = "这是中文显示测试。";
    var patch = Path.Combine(output, "changed-" + Guid.NewGuid().ToString("N"));
    await adapter.BuildPatchAsync(input, translations, patch);
    var validation = await adapter.ValidatePatchAsync(input, patch);
    var recovered = await adapter.ExtractAsync(patch);
    var textRecovered = recovered.Units.Single(u => u.ResourcePath == sample.ResourcePath && u.Locator == sample.Locator).SourceText == translations[sample.Id];
    var report = new
    {
        probe.EngineId, adapter.Version, units = extracted.Units.Count,
        unitKinds = extracted.Units.GroupBy(u => u.Kind).ToDictionary(g => g.Key, g => g.Count()),
        originalHash = initialHash, originalUnchanged = initialHash == await LocalFiles.HashFileAsync(Path.Combine(input, "scr.aos")),
        roundtripIdentical = unchanged, validation, textRecovered, cp936Substitutions = substitutions,
        note = "Synthetic Chinese test text only; this is not a translated game or a runtime display test."
    };
    await LocalFiles.WriteJsonAsync(Path.Combine(output, "darkblue-report.json"), report);
    Console.WriteLine(JsonSerializer.Serialize(report, LocalFiles.JsonOptions));
    return validation.Passed && textRecovered ? 0 : 1;
}

var toolchain = new PinnedDotNetToolchain(input, "10.0.402", "10.0.13");
var runner = new WindowsAppContainerProcessRunner();
var compiler = new GeneratedAdapterCompiler(toolchain, runner);
var outside = Path.Combine(output, "outside-secret.txt");
await File.WriteAllTextAsync(outside, "sandbox containment check");
var outsideWrite = Path.Combine(output, "outside-write.txt");
using var listener = new TcpListener(IPAddress.Loopback, 0);
listener.Start();
var port = ((IPEndPoint)listener.LocalEndpoint).Port;
var source = """
using System;
using System.IO;
using System.Diagnostics;
using System.Net;
using System.Net.Sockets;
using System.Text.Json;
using System.Collections.Generic;
var result = new Dictionary<string,string>();
try { File.ReadAllText(OUTSIDE); result["outsideRead"]="allowed"; } catch (UnauthorizedAccessException) { result["outsideRead"]="denied"; }
try { File.WriteAllText(WRITE,"bad"); result["outsideWrite"]="allowed"; } catch (UnauthorizedAccessException) { result["outsideWrite"]="denied"; }
try { File.WriteAllText("inside.txt","okay"); result["insideWrite"]="allowed"; } catch (Exception) { result["insideWrite"]="denied"; }
try { using var socket=new Socket(AddressFamily.InterNetwork,SocketType.Stream,ProtocolType.Tcp); socket.Connect(IPAddress.Loopback,PORT); result["network"]="allowed"; } catch (SocketException e) { result["network"]=e.SocketErrorCode==SocketError.AccessDenied ? "denied" : "inconclusive:"+e.SocketErrorCode; }
try { using var child=Process.Start(new ProcessStartInfo(DOTNET,"--info"){UseShellExecute=false,CreateNoWindow=true}); result["childProcess"]="allowed"; } catch (System.ComponentModel.Win32Exception) { result["childProcess"]="denied"; }
Console.WriteLine(JsonSerializer.Serialize(result));
""".Replace("OUTSIDE", JsonSerializer.Serialize(outside)).Replace("WRITE", JsonSerializer.Serialize(outsideWrite))
    .Replace("PORT", port.ToString()).Replace("DOTNET", JsonSerializer.Serialize(toolchain.ExecutablePath));
try
{
    var package = await compiler.CompileAsync(source, Path.Combine(output, "compile-" + Guid.NewGuid().ToString("N")), ["sample.dat"]);
    var workspace = Path.Combine(output, "run-" + Guid.NewGuid().ToString("N"));
    Directory.CreateDirectory(workspace);
    var execution = await runner.RunAsync(new IsolatedProcessRequest(toolchain.ExecutablePath,
        [Path.Combine(package.DirectoryPath, "adapter.dll")], workspace, [toolchain.RootDirectory, package.DirectoryPath]), default);
    var checks = JsonSerializer.Deserialize<Dictionary<string, string>>(execution.StandardOutput);
    var passed = execution.ExitCode == 0 && checks is not null && checks.GetValueOrDefault("insideWrite") == "allowed"
        && new[] { "outsideRead", "outsideWrite", "network", "childProcess" }.All(key => checks.GetValueOrDefault(key) == "denied")
        && !File.Exists(outsideWrite);
    await LocalFiles.WriteJsonAsync(Path.Combine(output, "sandbox-report.json"), new { status = passed ? "passed" : "failed", execution.ExitCode, checks });
    Console.WriteLine(passed ? "Containment checks passed." : "Containment checks failed or inconclusive.");
    return passed ? 0 : 1;
}
catch (IsolationUnavailableException error)
{
    await LocalFiles.WriteJsonAsync(Path.Combine(output, "sandbox-report.json"), new { status = "unavailable", reason = error.Message, generatedCodeExecuted = false });
    Console.WriteLine("Sandbox unavailable: " + error.Message);
    return 2;
}
