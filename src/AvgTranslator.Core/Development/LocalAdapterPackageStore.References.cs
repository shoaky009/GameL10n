using System.Text.Json;
using AvgTranslator.Core.Isolation;

namespace AvgTranslator.Core.Development;

public sealed partial class LocalAdapterPackageStore
{
    private async Task<List<EngineLibraryReference>> ReadReferencesAsync(string dataDirectory,
        ICollection<string> diagnostics, CancellationToken cancellationToken)
    {
        var result = new List<EngineLibraryReference>();
        var titles = new Dictionary<string, (string Id, string? Title)>(StringComparer.Ordinal);
        var projects = WorkspaceFiles.Resolve(Path.GetFullPath(dataDirectory), "games");
        foreach (var project in SafeDirectories(projects, diagnostics))
        {
            cancellationToken.ThrowIfCancellationRequested();
            var gameId = Path.GetFileName(project);
            string? title = null;
            try
            {
                var descriptor = await ReadIdentityAsync(project, "game.json", cancellationToken);
                title = descriptor?.Name;
                if (descriptor is not null) AddReference(descriptor, EngineLibraryReferenceKind.Project, project, "game.json", optional: true);
                titles[Path.GetFileName(GamePath(gameId))] = (gameId, title);
            }
            catch (Exception error) when (IsLibraryReadError(error))
            { diagnostics.Add(project + "/game.json：" + error.Message); }

            await ReadFile(project, "game.json.bak", EngineLibraryReferenceKind.Project, optional: true);
            foreach (var name in new[] { "source.json", "source.json.bak" })
                await ReadFile(project, name, EngineLibraryReferenceKind.Source);
            foreach (var name in new[] { "job.json", "job.json.bak" })
                await ReadFile(project, name, EngineLibraryReferenceKind.Job);
            foreach (var task in SafeDirectories(WorkspaceFiles.Resolve(project, "tasks"), diagnostics))
                foreach (var name in new[] { "job.json", "job.json.bak" })
                    await ReadFile(task, name, EngineLibraryReferenceKind.Job);
            foreach (var cache in SafeDirectories(WorkspaceFiles.Resolve(project, "extraction"), diagnostics))
            {
                await ReadFile(cache, "manifest.json", EngineLibraryReferenceKind.Extraction);
                await ReadFile(cache, "manifest.json.bak", EngineLibraryReferenceKind.Extraction);
            }

            async Task ReadFile(string directory, string name, EngineLibraryReferenceKind kind, bool optional = false)
            {
                try
                {
                    var identity = await ReadIdentityAsync(directory, name, cancellationToken);
                    if (identity is not null) AddReference(identity, kind, directory, name, optional);
                }
                catch (Exception error) when (IsLibraryReadError(error))
                { diagnostics.Add(Path.Combine(directory, name) + "：" + error.Message); }
            }

            void AddReference(ReferenceIdentity identity, EngineLibraryReferenceKind kind, string directory, string name, bool optional)
            {
                if (kind == EngineLibraryReferenceKind.Project && identity.Id != gameId)
                    throw new InvalidDataException("游戏项目标识缺失或与目录不一致。");
                if (optional && identity.AdapterId is null && identity.AdapterVersion is null) return;
                ValidateReferenceIdentity(identity.AdapterId, identity.AdapterVersion);
                result.Add(new(identity.AdapterId!, identity.AdapterVersion!, gameId, title, kind,
                    WorkspaceFiles.Resolve(directory, name)));
            }
        }

        foreach (var directory in SafeDirectories(WorkspaceFiles.Resolve(RootDirectory, "games"), diagnostics))
        {
            foreach (var (name, kind) in new[] { ("active.json", EngineLibraryReferenceKind.Active), ("previous.json", EngineLibraryReferenceKind.Previous) })
            {
                try
                {
                    var path = WorkspaceFiles.Resolve(directory, name);
                    if (!File.Exists(path)) continue;
                    var activation = JsonSerializer.Deserialize<Activation>(WorkspaceFiles.ReadBounded(directory, name, 4096), Json)
                        ?? throw new InvalidDataException("无效的引擎激活记录。");
                    ValidateReferenceIdentity(activation.Id, activation.Version);
                    var title = titles.GetValueOrDefault(Path.GetFileName(directory));
                    result.Add(new(activation.Id, activation.Version, activation.GameId ?? title.Id ?? Path.GetFileName(directory),
                        title.Title, kind, path));
                }
                catch (Exception error) when (IsLibraryReadError(error))
                { diagnostics.Add(Path.Combine(directory, name) + "：" + error.Message); }
            }
        }
        return result;
    }

    private void ValidateReferenceIdentity(string? id, string? version)
    {
        if (string.IsNullOrWhiteSpace(id) || string.IsNullOrWhiteSpace(version))
            throw new InvalidDataException("持久化记录缺少引擎或版本标识，不能确认版本未被使用。");
        _ = PackagePath(id, version);
    }

    private static async Task<ReferenceIdentity?> ReadIdentityAsync(string directory, string file, CancellationToken cancellationToken)
    {
        var path = WorkspaceFiles.Resolve(directory, file);
        if (!File.Exists(path)) return null;
        await using var stream = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read | FileShare.Delete,
            16 * 1024, FileOptions.Asynchronous | FileOptions.SequentialScan);
        // Unknown fields (notably extraction UnitOrder and source files) are streamed past without creating their objects.
        return await JsonSerializer.DeserializeAsync<ReferenceIdentity>(stream, Json, cancellationToken)
            ?? throw new InvalidDataException("持久化引用记录为空。");
    }

    private sealed record ReferenceIdentity(string? AdapterId, string? AdapterVersion, string? Name, string? Id);
}
