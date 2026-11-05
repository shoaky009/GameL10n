using System.Text.Json;
using GameL10n.Core.Isolation;
using System.Text.Json.Nodes;
using GameL10n.Core.Serialization;

namespace GameL10n.Core.Development;

public sealed partial class AdaptationTools
{
    private async Task<object> RunEngineComponentAsync(JsonElement args, CancellationToken ct)
    {
        var id = Text(args, "id");
        var component = EngineArchiveComponents.Find(id);
        var operation = Text(args, "operation");
        if (!component.Operations.Contains(operation)) throw new InvalidDataException("不支持的封包组件操作。");
        var filter = Text(args, "filter", "");
        if (id == "archive.xp3" && filter != "none")
            throw new InvalidDataException("请先确认 XP3 未使用过滤器，再明确指定 filter=none。");
        var archiveRelative = Text(args, "archive").Replace('\\', '/');
        if (!archiveRelative.StartsWith("resources/", StringComparison.Ordinal) && !archiveRelative.StartsWith("work/", StringComparison.Ordinal))
            throw new InvalidDataException("封包组件只读取 resources/ 或 work/ 中的副本。");
        var archive = WorkspaceFiles.Resolve(workspace, archiveRelative);
        if (!File.Exists(archive)) throw new FileNotFoundException("请先暂存原始封包。", archiveRelative);
        var maximumBytes = id == "archive.rpa3" ? EngineArchiveComponents.RpaMaximumArchiveBytes : 512L * 1024 * 1024;
        if (new FileInfo(archive).Length > maximumBytes) throw new InvalidDataException($"封包超过组件的 {maximumBytes / (1024 * 1024):N0} MiB 输入限制。");
        var outputRelative = Text(args, "output").Replace('\\', '/');
        if (!outputRelative.StartsWith("work/", StringComparison.Ordinal)) throw new InvalidDataException("组件输出必须是 work/ 下的新目录。");
        var output = WorkspaceFiles.Resolve(workspace, outputRelative);
        if (File.Exists(output) || Directory.Exists(output)) throw new InvalidDataException("组件输出目录已存在，请读取已有 manifest.json 或选择新的目录。");
        if (archive.StartsWith(output + Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase))
            throw new InvalidDataException("组件输入和输出目录不能重叠。");
        Dictionary<string, string>? replacements = null;
        if (args.TryGetProperty("replacements", out var mapping))
        {
            if (operation != "rebuild" || mapping.ValueKind != JsonValueKind.Object || mapping.EnumerateObject().Count() > 128)
                throw new InvalidDataException("replacements 仅用于 rebuild，最多 128 项。");
            replacements = new(StringComparer.Ordinal);
            foreach (var item in mapping.EnumerateObject())
            {
                var relative = item.Value.GetString()?.Replace('\\', '/') ?? "";
                if (!relative.StartsWith("work/", StringComparison.Ordinal)) throw new InvalidDataException("替换文件必须在 work/ 中。");
                var path = WorkspaceFiles.Resolve(workspace, relative);
                if (!File.Exists(path)) throw new FileNotFoundException("替换文件不存在。", relative);
                if (path.StartsWith(output + Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase))
                    throw new InvalidDataException("替换文件和输出目录不能重叠。");
                replacements.Add(item.Name, path);
            }
        }
        var members = args.TryGetProperty("members", out _) ? Strings(args, "members") : null;
        var extensions = args.TryGetProperty("extensions", out _) ? Strings(args, "extensions") : null;
        var prepared = await Development.PythonAsync(ct);
        if (prepared.Dependency.Reference.Kind != "galtransl") throw new InvalidDataException("此组件需要 GalTransl 共享工具库。");
        var response = await new EngineArchiveComponentClient(runtime.Runner).InvokeAsync(prepared.Paths,
            new(1, id, operation, archive, Path.Combine(output, "manifest.json"), Path.Combine(output, "files"),
                Path.Combine(output, "rebuilt", Path.GetFileName(archive)), filter, members, extensions, replacements,
                Boolean(args, "verifyContents", false)), workspace, ct);
        var success = response.GetProperty("success").GetBoolean();
        return new JsonObject
        {
            ["success"] = success,
            ["status"] = success ? "passed" : "failed",
            ["component"] = id,
            ["version"] = component.Version,
            ["revision"] = prepared.Dependency.Reference.Revision,
            ["manifest"] = outputRelative + "/manifest.json",
            ["error"] = success ? null : response.GetProperty("error").GetString(),
            ["entryCount"] = success ? response.GetProperty("entryCount").GetInt32() : (int?)null,
            ["selectedCount"] = success ? response.GetProperty("selectedCount").GetInt32() : (int?)null,
            ["inputBytes"] = success ? response.GetProperty("inputBytes").GetInt64() : (long?)null,
            ["selectedBytes"] = response.TryGetProperty("selectedBytes", out var selectedBytes) ? selectedBytes.GetInt64() : (long?)null,
            ["readBytes"] = response.TryGetProperty("readBytes", out var readBytes) ? readBytes.GetInt64() : (long?)null,
            ["allContentsVerified"] = response.TryGetProperty("allContentsVerified", out var allVerified) ? allVerified.GetBoolean() : (bool?)null,
            ["entries"] = EngineJson.Node(success ? response.GetProperty("entries").EnumerateArray().Take(12).Select(item => item.Clone()).ToArray() : [], EngineJson.Web),
            ["extractedDirectory"] = success && operation == "extract" ? outputRelative + "/files" : null,
            ["rebuiltArchive"] = success && operation == "rebuild" ? outputRelative + "/rebuilt/" + Path.GetFileName(archive) : null,
            ["byteIdentical"] = success && operation == "rebuild" ? response.GetProperty("byteIdentical").GetBoolean() : (bool?)null,
            ["runtimeStatus"] = "not-tested"
        };
    }
}
