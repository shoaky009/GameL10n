using System.Diagnostics;
using System.Text.Json;
using System.Text.Json.Nodes;
using GameL10n.Core.Isolation;
using GameL10n.Core.Translation;

namespace GameL10n.Core.Storage;

public sealed record DataTransferProgress(int Files, long Bytes);

/// <summary>Copies a quiescent data root. The caller holds both root locks until activation finishes.</summary>
public static class DataDirectoryTransfer
{
    private const string StageName = ".relocation";
    private const string ReceiptName = ".relocation.json";
    private sealed record Receipt(string Source, string[] Entries);

    public static bool Same(string left, string right) => string.Equals(
        Path.TrimEndingDirectorySeparator(Path.GetFullPath(left)),
        Path.TrimEndingDirectorySeparator(Path.GetFullPath(right)), StringComparison.OrdinalIgnoreCase);

    public static bool Contains(string root, string path)
    {
        var fullRoot = Path.GetFullPath(root);
        if (!Path.EndsInDirectorySeparator(fullRoot)) fullRoot += Path.DirectorySeparatorChar;
        return Same(root, path) || Path.GetFullPath(path).StartsWith(fullRoot, StringComparison.OrdinalIgnoreCase);
    }

    public static void ValidateDestination(string source, string destination, IEnumerable<string> gameDirectories)
    {
        WorkspaceFiles.RejectLinkAncestors(source);
        WorkspaceFiles.RejectLinkAncestors(destination);
        if (Contains(source, destination) || Contains(destination, source))
            throw new InvalidOperationException("新旧数据目录不能相同或互相包含，请选择独立目录。");
        if (gameDirectories.Any(game => Contains(game, destination) || Contains(destination, game)))
            throw new InvalidOperationException("数据目录与游戏安装目录不能相同或互相包含。");
    }

    public static FileStream AcquireLock(string directory, bool allowIncompleteTransfer = false)
    {
        WorkspaceFiles.RejectLinkAncestors(directory);
        Directory.CreateDirectory(directory);
        var handle = new FileStream(LocalFiles.UnderRoot(directory, ".app.lock"), FileMode.OpenOrCreate, FileAccess.ReadWrite, FileShare.None);
        try
        {
            if (!allowIncompleteTransfer && File.Exists(Path.Combine(directory, ReceiptName)))
                throw new InvalidDataException("这个目录的数据迁移尚未完成。请从原数据目录重新选择“迁移并切换”，完成后再使用。");
            return handle;
        }
        catch { handle.Dispose(); throw; }
    }

    public static async Task CopyAsync(string source, string destination, IProgress<DataTransferProgress>? progress = null,
        CancellationToken ct = default)
    {
        ValidateDestination(source, destination, []);
        var stage = LocalFiles.UnderRoot(destination, StageName);
        var receiptPath = LocalFiles.UnderRoot(destination, ReceiptName);
        var receipt = await LocalFiles.ReadJsonAsync<Receipt>(receiptPath, ct);
        if (receipt is not null)
        {
            if (!Same(source, receipt.Source) || receipt.Entries is null || receipt.Entries.Any(name =>
                    name != Path.GetFileName(name) || name is "." or ".." || IsRootControl(name)))
                throw new InvalidDataException("目标目录有其他来源的迁移记录，请选择空目录。");
            CheckDestination(destination, receipt.Entries);
        }
        else CheckDestination(destination, []);

        var entries = Directory.EnumerateFileSystemEntries(source).Select(Path.GetFileName)
            .OfType<string>().Where(name => !IsRootControl(name) && name != ".migration").ToArray();
        await LocalFiles.WriteJsonAsync(receiptPath, new Receipt(Path.GetFullPath(source),
            entries.Concat(receipt?.Entries ?? []).Distinct(StringComparer.OrdinalIgnoreCase).ToArray()), ct);
        var skipped = await FindRedundantPathsAsync(source, ct);
        var files = 0;
        long bytes = 0;
        var timer = Stopwatch.StartNew();
        foreach (var name in entries) await CopyEntryAsync(LocalFiles.UnderRoot(source, name), LocalFiles.UnderRoot(destination, name));
        await NormalizeProjectPathsAsync(source, destination, ct);
        ct.ThrowIfCancellationRequested();
        // Remove only disposable copies from older interrupted transfers. The source is untouched.
        if (receipt is not null)
        {
            foreach (var relative in skipped)
            {
                var redundant = LocalFiles.UnderRoot(destination, relative);
                if (Directory.Exists(redundant)) ClearStage(redundant);
                else if (File.Exists(redundant))
                {
                    File.SetAttributes(redundant, File.GetAttributes(redundant) & ~FileAttributes.ReadOnly);
                    File.Delete(redundant);
                }
            }
            if (Directory.Exists(stage)) ClearStage(stage);
        }
        File.Delete(receiptPath); // Only a complete copy may be opened by the application.
        progress?.Report(new(files, bytes));

        async Task CopyEntryAsync(string from, string to)
        {
            ct.ThrowIfCancellationRequested();
            WorkspaceFiles.RejectReparsePoints(from);
            if (skipped.Contains(Path.GetRelativePath(source, from).Replace('\\', '/'))) return;
            if (Directory.Exists(from))
            {
                Directory.CreateDirectory(to);
                foreach (var child in Directory.EnumerateFileSystemEntries(from))
                {
                    if (Path.GetFileName(child) is ".task.lock" or ".memory.lock" or ".app.lock") continue;
                    await CopyEntryAsync(child, LocalFiles.UnderRoot(to, Path.GetFileName(child)));
                }
                return;
            }
            var hash = await LocalFiles.HashFileAsync(from, ct);
            if (!File.Exists(to) || await LocalFiles.HashFileAsync(to, ct) != hash)
            {
                var temporary = to + ".copy-" + Guid.NewGuid().ToString("N") + ".tmp";
                try
                {
                    await using (var input = new FileStream(from, FileMode.Open, FileAccess.Read, FileShare.Read, 65536, FileOptions.Asynchronous))
                    await using (var output = new FileStream(temporary, FileMode.CreateNew, FileAccess.Write, FileShare.None, 65536, FileOptions.Asynchronous))
                    {
                        await input.CopyToAsync(output, ct);
                        await output.FlushAsync(ct);
                        output.Flush(true);
                    }
                    if (await LocalFiles.HashFileAsync(temporary, ct) != hash || await LocalFiles.HashFileAsync(from, ct) != hash)
                        throw new IOException("复制期间原文件发生变化，原目录仍可使用：" + Path.GetFileName(from));
                    if (File.Exists(to)) File.SetAttributes(to, File.GetAttributes(to) & ~FileAttributes.ReadOnly);
                    File.Move(temporary, to, overwrite: true);
                }
                finally { if (File.Exists(temporary)) File.Delete(temporary); }
            }
            bytes += new FileInfo(from).Length;
            files++;
            if (timer.ElapsedMilliseconds >= 200) { progress?.Report(new(files, bytes)); timer.Restart(); }
        }
    }

    private static bool IsRootControl(string name) => name is ".app.lock" or StageName or ReceiptName
        || name.Equals("location.json", StringComparison.OrdinalIgnoreCase)
        || name.StartsWith("location.json.", StringComparison.OrdinalIgnoreCase);

    private static async Task<HashSet<string>> FindRedundantPathsAsync(string source, CancellationToken ct)
    {
        var skipped = new HashSet<string>(StringComparer.OrdinalIgnoreCase) { ".migration" };
        var games = Path.Combine(source, "games");
        if (!Directory.Exists(games)) return skipped;
        foreach (var project in Directory.EnumerateDirectories(games))
        {
            var id = Path.GetFileName(project);
            if (id.Length != 24 || !id.All(Uri.IsHexDigit)) continue;
            var descriptor = await ReadValidAsync<GameProject>(Path.Combine(project, "game.json"),
                game => game.SchemaVersion == 2 && game.Id == id && game.Settings is not null
                    && !string.IsNullOrWhiteSpace(game.GameDirectory) && Pipeline.TranslationPipeline.GameId(game.GameDirectory) == id);
            if (descriptor is null) continue;
            // A whole-project migration backup is historical; current project files are copied below.
            skipped.Add("games/" + id + ".bak");
            var legacy = Path.Combine(source, "memory", id);
            var memory = Path.Combine(project, "memory");
            if (!Directory.Exists(memory)) continue;
            var translations = new Dictionary<string, MemoryEntry>(StringComparer.Ordinal);
            foreach (var prefix in Directory.EnumerateFiles(memory, "*.json*").Select(Path.GetFileName)
                .Where(name => name is { Length: 7 or 11 } && name[..2].All(Uri.IsHexDigit))
                .Select(name => name![..2]).Distinct(StringComparer.Ordinal))
            {
                var shard = await ReadValidAsync<MemoryShard>(Path.Combine(memory, prefix + ".json"),
                    value => value.SchemaVersion is 1 or 2 && value.Entries is not null && value.Entries.All(pair =>
                        pair.Key.Length == 64 && pair.Key.StartsWith(prefix, StringComparison.Ordinal) && pair.Value is not null
                        && pair.Key == pair.Value.Key && pair.Value.SourceText is not null && pair.Value.Context is not null
                        && pair.Value.Translation is not null));
                if (shard is not null)
                    foreach (var pair in shard.Entries) translations.Add(pair.Key, pair.Value);
            }
            foreach (var file in await Translation.TranslationEngine.FindCommittedResponseFilesAsync(
                Path.Combine(project, "responses"), translations, ct))
            {
                var relative = Path.GetRelativePath(source, file).Replace('\\', '/');
                skipped.Add(relative);
                skipped.Add(relative + ".bak");
            }
            // Check every legacy key, rather than relying on an import flag or a total count.
            // Unknown files stay at the destination; only proven redundant caches are omitted.
            if (Directory.Exists(legacy) && Directory.EnumerateFileSystemEntries(legacy).All(path => File.Exists(path)
                && Path.GetExtension(path).Equals(".json", StringComparison.OrdinalIgnoreCase)
                && translations.ContainsKey(Path.GetFileNameWithoutExtension(path))))
                skipped.Add("memory/" + id);
        }
        return skipped;

        async Task<T?> ReadValidAsync<T>(string path, Func<T, bool> validate)
        {
            foreach (var candidate in new[] { path, path + ".bak" })
            {
                WorkspaceFiles.RejectLinkAncestors(candidate);
                try
                {
                    var value = await LocalFiles.ReadJsonAsync<T>(candidate, ct);
                    if (value is not null && validate(value)) return value;
                }
                catch (JsonException) { }
            }
            return default;
        }
    }

    private static void CheckDestination(string directory, string[] publishedEntries)
    {
        if (Directory.EnumerateFileSystemEntries(directory).Select(Path.GetFileName).Any(name =>
                name is not ".app.lock" and not ReceiptName and not StageName && !publishedEntries.Contains(name, StringComparer.OrdinalIgnoreCase)))
            throw new IOException("目标目录已有数据。迁移需要空目录；若要使用那里的数据，请选择“仅切换”。");
        if (publishedEntries.Length == 0 && Directory.Exists(Path.Combine(directory, StageName)) && !File.Exists(Path.Combine(directory, ReceiptName)))
            throw new IOException("目标目录有无法确认来源的迁移文件，请选择空目录。");
    }

    private static void ClearStage(string stage)
    {
        WorkspaceFiles.RejectLinkAncestors(stage);
        foreach (var entry in Directory.EnumerateFileSystemEntries(stage))
        {
            WorkspaceFiles.RejectReparsePoints(entry);
            if (Directory.Exists(entry)) ClearStage(entry);
            else { File.SetAttributes(entry, File.GetAttributes(entry) & ~FileAttributes.ReadOnly); File.Delete(entry); }
        }
        Directory.Delete(stage);
    }

    private static async Task NormalizeProjectPathsAsync(string source, string stage, CancellationToken ct)
    {
        var games = Path.Combine(stage, "games");
        if (!Directory.Exists(games)) return;
        foreach (var project in Directory.EnumerateDirectories(games))
        {
            var original = Path.Combine(source, "games", Path.GetFileName(project));
            foreach (var name in new[] { "source.json", "source.json.bak", "latest-result.json", "latest-result.json.bak" })
                await NormalizeAsync(Path.Combine(project, name), original, name.StartsWith("source", StringComparison.Ordinal) ? "directory" : "patchDirectory");
            var patches = Path.Combine(project, "patches");
            if (Directory.Exists(patches))
                foreach (var file in Directory.EnumerateFiles(patches, "translation-report.json*", SearchOption.AllDirectories))
                    await NormalizeAsync(file, original, "patchDirectory");
        }

        async Task NormalizeAsync(string file, string original, string property)
        {
            if (!File.Exists(file)) return;
            JsonNode? node;
            try { node = JsonNode.Parse(await File.ReadAllTextAsync(file, ct)); }
            catch (JsonException) { return; } // Preserve damaged primaries and their recovery backups exactly.
            if (node is JsonObject value && value[property] is JsonValue field && field.TryGetValue<string>(out var path)
                && Path.IsPathRooted(path) && Contains(original, path))
            {
                value[property] = Path.GetRelativePath(original, path);
                await LocalFiles.WriteJsonAsync(file, value, ct);
            }
        }
    }
}
