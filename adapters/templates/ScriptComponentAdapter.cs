using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;
using GameL10n.AdapterHost;

// Standalone protocol entry point. Source algorithms stay in the pinned components.
internal static class ScriptComponentAdapter
{
    private const string Warning = "仅处理配置选中的源脚本；编译脚本、游戏启动、字体及中文显示尚未实机验证。";
    private static readonly JsonSerializerOptions Json = new(AdapterHostJson.Web);

    public static int Main()
    {
        Console.InputEncoding = new UTF8Encoding(false);
        Console.OutputEncoding = new UTF8Encoding(false);
        var input = Console.In.ReadToEnd();
        var response = JsonNode.Parse(ExecuteJson(input, AppContext.BaseDirectory))!.AsObject();
        JsonNode? request = null;
        try { request = JsonNode.Parse(input); } catch (JsonException) { }
        if (request?["responseFile"]?.GetValue<string>() is { } path) File.WriteAllText(path, response.ToJsonString(), new UTF8Encoding(false));
        else Console.WriteLine(response.ToJsonString());
        return response["success"]!.GetValue<bool>() ? 0 : 1;
    }

    public static string ExecuteJson(string input, string adapterDirectory)
    {
        JsonObject? request = null;
        JsonObject response;
        try
        {
            request = JsonNode.Parse(input)!.AsObject();
            if (request["protocolVersion"]?.GetValue<int>() != 1) throw new InvalidDataException("不支持的协议版本。");
            var config = JsonSerializer.Deserialize<Config>(File.ReadAllText(Path.Combine(adapterDirectory, "tools", "adapter-template.json")), AdapterHostJson.TypeInfo<Config>(Json))
                ?? throw new InvalidDataException("缺少模板配置。");
            if (config.SchemaVersion != 1 || config.TemplateVersion != 1 || config.Format != BuiltinScriptParser.Format
                || config.ScriptExtensions.Length == 0) throw new InvalidDataException("模板与组件配置版本不一致。");
            var parser = request["toolPaths"]?["vnTextPatch"]?.GetValue<string>();
            if (string.IsNullOrWhiteSpace(parser)) throw new InvalidDataException("缺少固定版本的共享脚本解析器。");
            Environment.SetEnvironmentVariable("GAMEL10N_VNTEXTPATCH", parser);
            using var adapter = new Invocation(request, config);
            response = adapter.Run();
        }
        catch (Exception error)
        {
            Console.Error.WriteLine(error);
            response = new() { ["protocolVersion"] = 1, ["success"] = false,
                ["error"] = "组件模板处理失败：" + error.GetBaseException().Message };
        }
        return response.ToJsonString();
    }

    internal sealed record Config(int SchemaVersion, string TemplateId, int TemplateVersion, string Format,
        string? ArchiveComponent, string? Filter, string[] ScriptExtensions, string[] ResourcePatterns);
    private sealed record Container(string Resource, string Directory, string[] Files, JsonArray Members, JsonArray Units);

    private sealed class Invocation(JsonObject request, Config config) : IDisposable
    {
        private readonly string scratch = Path.Combine(Directory.GetCurrentDirectory(), "template-" + Guid.NewGuid().ToString("N"));
        private int componentCalls;

        public JsonObject Run()
        {
            var source = Text(request, "sourceDirectory");
            var files = request["resourceFiles"]!.AsArray().Select(node => node!.GetValue<string>().Replace('\\', '/')).Order(StringComparer.Ordinal).ToArray();
            if (files.Length == 0 || files.Distinct(StringComparer.OrdinalIgnoreCase).Count() != files.Length)
                throw new InvalidDataException("资源列表为空或有重复路径。");
            foreach (var file in files) _ = Resolve(source, file);
            var operation = Text(request, "operation");
            if (operation == "probe") return Probe(source, files);
            if (config.ArchiveComponent is null)
            {
                var response = JsonNode.Parse(BuiltinScriptParser.ExecuteJson(request.ToJsonString()))!.AsObject();
                foreach (var key in new[] { "extraction", "build" })
                    if (response[key] is JsonObject result) result["warnings"]!.AsArray().Add(Warning);
                return response;
            }
            Directory.CreateDirectory(scratch);
            var original = Load(source, files, "original");
            return operation switch
            {
                "extract" => Success("extraction", new JsonObject
                {
                    ["units"] = AdapterHostJson.Node((original.SelectMany(container => ExternalUnits(container))).ToArray(), AdapterHostJson.Web),
                    ["warnings"] = AdapterHostJson.Node(new[] { Warning }, AdapterHostJson.Web)
                }),
                "build" => Build(source, files, original),
                "validate" => Validate(files, original),
                _ => throw new InvalidDataException("不支持的适配操作。")
            };
        }

        private JsonObject Probe(string source, string[] files)
        {
            foreach (var file in files)
            {
                var path = Resolve(source, file);
                if (config.ArchiveComponent is null)
                {
                    if (!config.ScriptExtensions.Contains(Path.GetExtension(file).ToLowerInvariant())) return Success("probe", null);
                    continue;
                }
                // Probe receives headers only. Never invoke a full archive parser here.
                using var stream = File.OpenRead(path);
                var bytes = new byte[11];
                var read = stream.ReadAtLeast(bytes, bytes.Length, throwOnEndOfStream: false);
                var match = config.ArchiveComponent switch
                {
                    "archive.rpa3" => read >= 8 && (bytes.AsSpan(0, 8).SequenceEqual("RPA-3.0 "u8) || bytes.AsSpan(0, 8).SequenceEqual("ARC-3.0 "u8)),
                    "archive.xp3" => read == 11 && bytes.AsSpan().SequenceEqual(new byte[] { 0x58, 0x50, 0x33, 0x0d, 0x0a, 0x20, 0x0a, 0x1a, 0x8b, 0x67, 1 }),
                    _ => false
                };
                if (!match) return Success("probe", null);
            }
            return Success("probe", new JsonObject
            {
                ["engineId"] = config.TemplateId,
                ["adapterId"] = config.TemplateId,
                ["adapterVersion"] = "1",
                ["description"] = "组件模板 · " + config.TemplateId,
                ["requiredFiles"] = AdapterHostJson.Node(files, AdapterHostJson.Web),
                ["warnings"] = AdapterHostJson.Node(new[] { Warning }, AdapterHostJson.Web)
            });
        }

        private List<Container> Load(string source, string[] files, string phase)
        {
            var containers = new List<Container>();
            foreach (var file in files)
            {
                var directory = Path.Combine(scratch, phase, containers.Count.ToString("D4"));
                var report = Archive("extract", Resolve(source, file), directory, null, null);
                var scripts = report["extractedFiles"]!.AsArray().Select(node => node!.GetValue<string>()).Order(StringComparer.Ordinal).ToArray();
                var units = scripts.Length == 0 ? new JsonArray()
                    : Parser("extract", directory, scripts)["extraction"]!["units"]!.AsArray();
                containers.Add(new(file, directory, scripts, report["entries"]!.AsArray(), units));
            }
            return containers;
        }

        private IEnumerable<JsonObject> ExternalUnits(Container container)
        {
            foreach (var item in container.Units)
            {
                var unit = item!.DeepClone().AsObject();
                unit["id"] = ExternalId(container.Resource, Text(unit, "id"));
                unit["locator"] = "member:" + JsonSerializer.Serialize(new[] { Text(unit, "resourcePath"), Text(unit, "locator") },AdapterHostJson.TypeInfo<string[]>(AdapterHostJson.Default));
                unit["resourcePath"] = container.Resource;
                unit["context"] = container.Resource + " → " + Text(unit, "context");
                yield return unit;
            }
        }

        private JsonObject Build(string source, string[] files, List<Container> containers)
        {
            var translations = request["translationsFile"]?.GetValue<string>() is { } path
                ? JsonSerializer.Deserialize<Dictionary<string, string>>(File.ReadAllText(path), AdapterHostJson.TypeInfo<Dictionary<string, string>>(Json))!
                : request["translations"]?.Deserialize<Dictionary<string, string>>(AdapterHostJson.TypeInfo<Dictionary<string, string>>(Json)) ?? new();
            var ids = containers.SelectMany(container => container.Units.Select(unit => ExternalId(container.Resource, Text(unit!, "id")))).ToHashSet(StringComparer.Ordinal);
            if (translations.Keys.Any(id => !ids.Contains(id))) throw new InvalidDataException("部分译文无法对应当前脚本，请重新提取。");
            var output = Text(request, "outputDirectory");
            foreach (var container in containers)
            {
                var patched = container.Directory + "-patched";
                var replacements = new Dictionary<string, string>(StringComparer.Ordinal);
                if (container.Files.Length > 0)
                {
                    var selected = new Dictionary<string, string>(StringComparer.Ordinal);
                    foreach (var unit in container.Units)
                        if (translations.TryGetValue(ExternalId(container.Resource, Text(unit!, "id")), out var text)) selected.Add(Text(unit!, "id"), text);
                    Parser("build", container.Directory, container.Files, patched, selected);
                    foreach (var member in container.Members)
                    {
                        var name = Text(member!, "name");
                        var relative = name.Replace('\\', '/');
                        if (container.Files.Contains(relative, StringComparer.Ordinal))
                        {
                            var replacement = Resolve(patched, relative);
                            if (Hash(replacement) != Text(member!, "sha256")) replacements.Add(name, replacement);
                        }
                    }
                }
                var target = Resolve(output, container.Resource);
                if (replacements.Count == 0)
                {
                    Directory.CreateDirectory(Path.GetDirectoryName(target)!);
                    File.Copy(Resolve(source, container.Resource), target, false);
                }
                else Archive("rebuild", Resolve(source, container.Resource), null, target, replacements);
            }
            return Success("build", new JsonObject
            {
                ["files"] = AdapterHostJson.Node(files, AdapterHostJson.Web),
                ["warnings"] = AdapterHostJson.Node(new[] { Warning }, AdapterHostJson.Web)
            });
        }

        private JsonObject Validate(string[] files, List<Container> original)
        {
            var patched = Load(Text(request, "outputDirectory"), files, "patched");
            var issues = new JsonArray();
            for (var i = 0; i < original.Count; i++)
            {
                var before = original[i];
                var after = patched[i];
                var members = after.Members.ToDictionary(member => Text(member!, "name"), member => Text(member!, "sha256"), StringComparer.Ordinal);
                if (before.Members.Count != members.Count || before.Members.Any(member => !members.ContainsKey(Text(member!, "name"))))
                    throw new InvalidDataException("补丁改变了封包成员列表：" + before.Resource);
                foreach (var member in before.Members)
                    if (!before.Files.Contains(Text(member!, "name").Replace('\\', '/'), StringComparer.Ordinal)
                        && Text(member!, "sha256") != members[Text(member!, "name")])
                        throw new InvalidDataException("补丁修改了未选择的封包成员：" + Text(member!, "name"));
                if (!before.Files.SequenceEqual(after.Files)) throw new InvalidDataException("补丁改变了脚本资源列表。");
                if (before.Files.Length == 0) continue;
                var validation = Parser("validate", before.Directory, before.Files, after.Directory)["validation"]!;
                foreach (var issue in validation["issues"]!.AsArray()) issues.Add(issue!.DeepClone());
                if (!validation["passed"]!.GetValue<bool>() && validation["issues"]!.AsArray().Count == 0)
                    issues.Add(new JsonObject { ["code"] = "script-validation", ["message"] = "脚本结构未通过验证：" + before.Resource });
            }
            return Success("validation", new JsonObject
            {
                ["passed"] = issues.Count == 0,
                ["issues"] = AdapterHostJson.Node((issues).ToArray(), AdapterHostJson.Web),
                ["runtimeStatus"] = "not-tested"
            });
        }

        private JsonObject Archive(string operation, string archive, string? directory, string? output, Dictionary<string, string>? replacements)
        {
            var responsePath = Path.Combine(scratch, "component-" + componentCalls++ + ".json");
            var input = JsonSerializer.SerializeToNode(new JsonObject
            {
                ["protocolVersion"] = 1,
                ["component"] = config.ArchiveComponent,
                ["operation"] = operation,
                ["archive"] = archive,
                ["responseFile"] = responsePath,
                ["outputDirectory"] = directory,
                ["outputFile"] = output,
                ["filter"] = config.Filter,
                ["extensions"] = AdapterHostJson.Node(config.ScriptExtensions, Json),
                ["replacements"] = AdapterHostJson.Node(replacements, Json)
            }, AdapterHostJson.TypeInfo<JsonObject>(Json))!.AsObject();
            input["toolPaths"] = request["toolPaths"]!.DeepClone();
            input["adapterDirectory"] = request["adapterDirectory"]?.DeepClone();
            var exit = EngineReferencePython.Run("tools/engine_archive.py", input.ToJsonString(), forwardOutput: false);
            if (!File.Exists(responsePath)) throw new InvalidDataException("封包组件未返回结果。");
            var response = JsonNode.Parse(File.ReadAllText(responsePath))!.AsObject();
            if (exit != 0 || response["success"]?.GetValue<bool>() != true)
                throw new InvalidDataException("封包组件未完成操作：" + response["error"]?.GetValue<string>());
            return response;
        }

        private static JsonObject Parser(string operation, string source, string[] files, string? output = null, Dictionary<string, string>? translations = null)
        {
            var input = JsonSerializer.Serialize(new JsonObject
            {
                ["protocolVersion"] = 1,
                ["operation"] = operation,
                ["sourceDirectory"] = source,
                ["resourceFiles"] = AdapterHostJson.Node(files, Json),
                ["outputDirectory"] = output,
                ["translations"] = AdapterHostJson.Node(translations, Json)
            }, AdapterHostJson.TypeInfo<JsonObject>(Json));
            var response = JsonNode.Parse(BuiltinScriptParser.ExecuteJson(input))!.AsObject();
            if (response["success"]?.GetValue<bool>() != true) throw new InvalidDataException(response["error"]?.GetValue<string>() ?? "脚本组件未返回成功结果。");
            return response;
        }

        public void Dispose() { if (Directory.Exists(scratch)) Directory.Delete(scratch, true); }
    }

    private static JsonObject Success(string property, object? value) => new()
        { ["protocolVersion"] = 1, ["success"] = true, [property] = JsonSerializer.SerializeToNode(value, AdapterHostJson.TypeInfo<object?>(Json)) };
    private static string Text(JsonNode node, string key) => node[key]!.GetValue<string>();
    private static string Hash(string file) { using var stream = File.OpenRead(file); return Convert.ToHexStringLower(SHA256.HashData(stream)); }
    private static string ExternalId(string archive, string id) => "component:" + Convert.ToHexStringLower(SHA256.HashData(Encoding.UTF8.GetBytes(archive + "\0" + id)));

    private static string Resolve(string root, string relative)
    {
        relative = relative.Replace('\\', '/');
        if (Path.IsPathRooted(relative) || relative.Contains(':') || relative.Split('/').Any(segment => string.IsNullOrWhiteSpace(segment) || segment is "." or ".."))
            throw new InvalidDataException("资源路径无效。");
        var fullRoot = Path.GetFullPath(root).TrimEnd(Path.DirectorySeparatorChar) + Path.DirectorySeparatorChar;
        var path = Path.GetFullPath(Path.Combine(fullRoot, relative));
        if (!path.StartsWith(fullRoot, StringComparison.OrdinalIgnoreCase)) throw new InvalidDataException("资源路径越界。");
        for (string? part = path; part is not null; part = Path.GetDirectoryName(part))
            if ((File.Exists(part) || Directory.Exists(part)) && (File.GetAttributes(part) & FileAttributes.ReparsePoint) != 0)
                throw new InvalidDataException("资源路径包含链接。");
        return path;
    }
}
