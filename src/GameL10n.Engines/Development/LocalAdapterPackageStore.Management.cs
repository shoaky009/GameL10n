using System.Text.Json;
using GameL10n.Core.Adapters;
using GameL10n.Core.Isolation;
using GameL10n.Core.Storage;
using GameL10n.Core.Serialization;

namespace GameL10n.Core.Development;

public sealed partial class LocalAdapterPackageStore
{
    /// <summary>Read once per candidate-selection operation. Existing project pins ignore this setting.</summary>
    public IReadOnlySet<string> ReadDisabledFamilyIds() => ReadLibraryState().Families
        .Where(pair => !pair.Value.Enabled).Select(pair => pair.Key).ToHashSet(StringComparer.Ordinal);

    public Task SetFamilyEnabledAsync(string adapterId, bool enabled, CancellationToken cancellationToken = default) =>
        SetFamiliesEnabledAsync([adapterId], enabled, cancellationToken);

    public async Task SetFamiliesEnabledAsync(IReadOnlyCollection<string> adapterIds, bool enabled,
        CancellationToken cancellationToken = default)
    {
        var ids = adapterIds.Distinct(StringComparer.Ordinal).ToArray();
        foreach (var adapterId in ids) ValidateFamilyId(adapterId);
        if (ids.Length == 0) return;
        await activationLock.WaitAsync(cancellationToken);
        try
        {
            var state = ReadLibraryState();
            foreach (var adapterId in ids)
            {
                var family = state.Families.GetValueOrDefault(adapterId) ?? new FamilyState();
                state.Families[adapterId] = family with { Enabled = enabled };
            }
            await WriteLibraryStateAsync(state, cancellationToken);
        }
        finally { activationLock.Release(); }
    }

    public string GetVersionDirectory(string adapterId, string version) => PackagePath(adapterId, version);

    /// <summary>Releases pins after an explicit project-data deletion, retaining its published engine capability.</summary>
    public async Task ForgetGameAsync(string gameId, CancellationToken cancellationToken = default)
    {
        await activationLock.WaitAsync(cancellationToken);
        try
        {
            var directory = GamePath(gameId);
            var active = WorkspaceFiles.Resolve(directory, "active.json");
            var previous = WorkspaceFiles.Resolve(directory, "previous.json");
            if (File.Exists(active))
            {
                var activation = JsonSerializer.Deserialize<Activation>(WorkspaceFiles.ReadBounded(directory, "active.json", 4096), EngineJson.TypeInfo<Activation>(Json))
                    ?? throw new InvalidDataException("无效的引擎激活记录。");
                ValidateReferenceIdentity(activation.Id, activation.Version);
                if (Directory.Exists(PackagePath(activation.Id, activation.Version)))
                {
                    var state = ReadLibraryState();
                    var family = state.Families.GetValueOrDefault(activation.Id) ?? new FamilyState();
                    state.Families[activation.Id] = family with
                    {
                        Releases = (family.Releases ?? []).Where(item => item.Version != activation.Version)
                            .Append((family.Releases ?? []).FirstOrDefault(item => item.Version == activation.Version) ?? activation).ToArray()
                    };
                    await WriteLibraryStateAsync(state, cancellationToken);
                }
            }
            cancellationToken.ThrowIfCancellationRequested();
            if (File.Exists(active)) File.Delete(active);
            if (File.Exists(previous)) File.Delete(previous);
            if (Directory.Exists(directory) && !Directory.EnumerateFileSystemEntries(directory).Any()) Directory.Delete(directory);
        }
        finally { activationLock.Release(); }
    }

    /// <summary>Deletes a version, its compatibility records and global pins unless a running task uses it. Project data is retained.</summary>
    public Task<EngineLibraryDeleteResult> DeleteVersionAsync(string adapterId, string version,
        IReadOnlyList<EngineLibraryReference>? runtimeReferences = null, CancellationToken cancellationToken = default)
    {
        var running = runtimeReferences?.ToArray() ?? [];
        return Task.Run(async () =>
        {
            await activationLock.WaitAsync(cancellationToken);
            var deleted = false;
            try
            {
                var path = PackagePath(adapterId, version);
                ValidateFamilyId(adapterId);
                var used = running.Where(item => item.Kind == EngineLibraryReferenceKind.Running
                    && item.AdapterId == adapterId && item.Version == version).Distinct().ToArray();
                if (used.Length != 0) return new EngineLibraryDeleteResult(false, used, []);
                // Validate every descendant before recursive deletion so a junction cannot redirect traversal.
                if (Directory.Exists(path)) ValidatePackageTree(path, cancellationToken);
                var state = ReadLibraryState();
                var compatibility = ReadCompatibilityVersionRemoval(adapterId, version);
                var pins = ReadVersionPins(adapterId, version);
                cancellationToken.ThrowIfCancellationRequested();
                if (Directory.Exists(path)) Directory.Delete(path, recursive: true);
                deleted = true;
                // After removing files, finish the small metadata writes even if the caller cancels.
                DeleteEmptyDirectory(Path.GetDirectoryName(path)!);
                await RemoveCompatibilityVersionAsync(compatibility, CancellationToken.None);
                foreach (var pin in pins)
                {
                    File.Delete(pin);
                    DeleteEmptyDirectory(Path.GetDirectoryName(pin)!);
                }
                if (state.Families.TryGetValue(adapterId, out var family))
                {
                    var remaining = family with
                    {
                        Releases = (family.Releases ?? []).Where(item => item.Version != version).ToArray(),
                        ExcludedVersions = (family.ExcludedVersions ?? []).Where(item => item != version).ToArray()
                    };
                    if (remaining.Releases is { Count: 0 } && remaining.ExcludedVersions is { Count: 0 }
                        && !Directory.Exists(Path.GetDirectoryName(path)!) && !Directory.Exists(compatibility.Directory))
                        state.Families.Remove(adapterId);
                    else state.Families[adapterId] = remaining;
                    await LocalFiles.WriteJsonAsync(WorkspaceFiles.Resolve(RootDirectory, "library-state.json"), state, EngineJson.TypeInfo<LibraryState>(EngineJson.Storage), CancellationToken.None);
                }
                await UpdateLibraryIndexAsync(index =>
                {
                    ApplyIndexState(index, state);
                    if (!index.Families.TryGetValue(adapterId, out var indexedFamily)) return;
                    indexedFamily.Versions.Remove(version);
                    indexedFamily.Games.RemoveAll(game => game.AdapterVersion == version);
                    if (indexedFamily.Versions.Count == 0 && indexedFamily.Games.Count == 0) index.Families.Remove(adapterId);
                }, CancellationToken.None);
                return new EngineLibraryDeleteResult(true, [], []);
            }
            catch (Exception error) when (IsLibraryReadError(error))
            { return new EngineLibraryDeleteResult(deleted, [], [(deleted
                ? "版本文件已删除，但关联记录或索引清理未完成，请重试："
                : "无法完成引擎版本删除，请解决问题后重试：") + error.Message]); }
            finally { activationLock.Release(); }
        }, cancellationToken);
    }

    private List<string> ReadVersionPins(string adapterId, string version)
    {
        var pins = new List<string>();
        var root = WorkspaceFiles.Resolve(RootDirectory, "games");
        if (!Directory.Exists(root)) return pins;
        foreach (var child in Directory.EnumerateDirectories(root))
        {
            var directory = WorkspaceFiles.Resolve(root, Path.GetFileName(child));
            foreach (var name in new[] { "active.json", "previous.json" })
            {
                var path = WorkspaceFiles.Resolve(directory, name);
                if (!File.Exists(path)) continue;
                var contents = WorkspaceFiles.ReadBounded(directory, name, 4096);
                if (!System.Text.Encoding.UTF8.GetString(contents).Contains(adapterId, StringComparison.Ordinal)) continue;
                var activation = JsonSerializer.Deserialize<Activation>(contents, EngineJson.TypeInfo<Activation>(Json))
                    ?? throw new InvalidDataException("无效的引擎激活记录：" + path);
                if (activation.Id == adapterId && activation.Version == version) pins.Add(path);
            }
        }
        return pins;
    }

    private static void DeleteEmptyDirectory(string directory)
    {
        WorkspaceFiles.RejectLinkAncestors(directory);
        if (Directory.Exists(directory) && !Directory.EnumerateFileSystemEntries(directory).Any()) Directory.Delete(directory);
    }

    private LibraryState ReadLibraryState()
    {
        var path = WorkspaceFiles.Resolve(RootDirectory, "library-state.json");
        if (!File.Exists(path)) return new();
        var state = JsonSerializer.Deserialize<LibraryState>(WorkspaceFiles.ReadBounded(RootDirectory, "library-state.json", 16 * 1024 * 1024), EngineJson.TypeInfo<LibraryState>(Json));
        if (state is null || state.SchemaVersion != 1 || state.Families is null)
            throw new InvalidDataException("引擎库状态格式无效。");
        foreach (var (id, family) in state.Families)
        {
            ValidateFamilyId(id);
            if (family is null) throw new InvalidDataException("引擎库启停记录无效。");
            foreach (var release in family.Releases ?? [])
            {
                if (release is null || release.Id != id) throw new InvalidDataException("引擎库发布版本记录无效。");
                if (!PackagePart().IsMatch(release.Version)) throw new InvalidDataException("引擎库版本标识无效。");
            }
            foreach (var version in family.ExcludedVersions ?? [])
                if (!PackagePart().IsMatch(version)) throw new InvalidDataException("引擎库版本标识无效。");
        }
        return state;
    }

    private async Task WriteLibraryStateAsync(LibraryState state, CancellationToken cancellationToken,
        GeneratedAdapterPackage? published = null)
    {
        await LocalFiles.WriteJsonAsync(WorkspaceFiles.Resolve(RootDirectory, "library-state.json"), state, EngineJson.TypeInfo<LibraryState>(EngineJson.Storage), cancellationToken);
        // Complete the metadata commit even if the originating task is cancelled after the authoritative write.
        await UpdateLibraryIndexAsync(index => ApplyIndexState(index, state, published), CancellationToken.None);
    }

    private async Task RecordReleaseAsync(Activation activation, GeneratedAdapterPackage published, CancellationToken cancellationToken)
    {
        var state = ReadLibraryState();
        var family = state.Families.GetValueOrDefault(activation.Id) ?? new FamilyState();
        state.Families[activation.Id] = family with
        {
            Releases = (family.Releases ?? []).Where(item => item.Version != activation.Version)
                .Append((family.Releases ?? []).FirstOrDefault(item => item.Version == activation.Version) ?? activation).ToArray(),
            ExcludedVersions = (family.ExcludedVersions ?? []).Where(version => version != activation.Version).ToArray()
        };
        await WriteLibraryStateAsync(state, cancellationToken, published);
    }

    private List<Activation> ReadCandidateActivations(LibraryState state, ICollection<string>? diagnostics)
    {
        var activations = new List<Activation>();
        foreach (var directory in SafeDirectories(WorkspaceFiles.Resolve(RootDirectory, "games"), diagnostics))
        {
            try
            {
                if (!File.Exists(WorkspaceFiles.Resolve(directory, "active.json"))) continue;
                var activation = JsonSerializer.Deserialize<Activation>(WorkspaceFiles.ReadBounded(directory, "active.json", 4096), EngineJson.TypeInfo<Activation>(Json))
                    ?? throw new InvalidDataException("无效的激活记录。");
                _ = PackagePath(activation.Id, activation.Version);
                activations.Add(activation);
            }
            catch (Exception error) when (IsLibraryReadError(error))
            { diagnostics?.Add("无法读取引擎激活记录：" + error.Message); }
        }
        activations.AddRange(state.Families.Values.SelectMany(family => family.Releases ?? []));
        var excluded = state.Families.ToDictionary(pair => pair.Key,
            pair => (pair.Value.ExcludedVersions ?? []).ToHashSet(StringComparer.Ordinal), StringComparer.Ordinal);
        return activations.Where(item => (state.Families.GetValueOrDefault(item.Id)?.Enabled ?? true)
                && !(excluded.GetValueOrDefault(item.Id)?.Contains(item.Version) ?? false))
            .OrderByDescending(item => item.ActivatedAt).DistinctBy(item => (item.Id, item.Version)).ToList();
    }

    private static DateTimeOffset? ReadPublishedAt(string directory, ICollection<string> diagnostics)
    {
        try
        {
            if (!File.Exists(Path.Combine(directory, "verification.json"))) return null;
            using var document = JsonDocument.Parse(ReadDirectMetadata(directory, "verification.json", 1024 * 1024));
            if (document.RootElement.ValueKind != JsonValueKind.Object) throw new InvalidDataException("发布时间记录格式无效。");
            return document.RootElement.TryGetProperty("verifiedAt", out var value) && value.ValueKind == JsonValueKind.String
                && value.TryGetDateTimeOffset(out var date) ? date : null;
        }
        catch (Exception error) when (IsLibraryReadError(error))
        { diagnostics.Add("无法读取引擎发布时间：" + error.Message); return null; }
    }

    private static List<string> SafeDirectories(string root, ICollection<string>? diagnostics, bool ancestorsChecked = false)
    {
        var result = new List<string>();
        try
        {
            if (!ancestorsChecked) WorkspaceFiles.RejectLinkAncestors(root);
            if (Directory.Exists(root))
                foreach (var directory in Directory.EnumerateDirectories(root))
                {
                    try { WorkspaceFiles.RejectReparsePoints(directory); result.Add(directory); }
                    catch (Exception error) when (IsLibraryReadError(error)) { diagnostics?.Add(directory + "：" + error.Message); }
                }
        }
        catch (Exception error) when (IsLibraryReadError(error)) { diagnostics?.Add(root + "：" + error.Message); }
        return result;
    }

    // Only for fixed names or directly enumerated children of a directory already checked by SafeDirectories.
    // Execution and mutation paths continue to use WorkspaceFiles.Resolve for their complete path checks.
    private static byte[] ReadDirectMetadata(string directory, string fileName, int maximumBytes)
    {
        if (fileName != Path.GetFileName(fileName) || fileName.IndexOfAny(['/', '\\', ':']) >= 0 || fileName is "." or "..")
            throw new InvalidDataException("元数据必须是目录的直接子文件。");
        var path = Path.Combine(directory, fileName);
        WorkspaceFiles.RejectReparsePoints(path);
        using var stream = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read | FileShare.Delete,
            16 * 1024, FileOptions.SequentialScan);
        if (stream.Length > maximumBytes) throw new InvalidDataException("元数据文件超过大小限制。");
        var contents = new byte[checked((int)stream.Length)];
        stream.ReadExactly(contents);
        return contents;
    }

    private static void ValidatePackageTree(string directory, CancellationToken cancellationToken)
    {
        WorkspaceFiles.RejectLinkAncestors(directory);
        foreach (var path in Directory.EnumerateFileSystemEntries(directory))
        {
            cancellationToken.ThrowIfCancellationRequested();
            var safe = WorkspaceFiles.Resolve(directory, Path.GetFileName(path));
            if (Directory.Exists(safe)) ValidatePackageTree(safe, cancellationToken);
        }
    }

    private static bool TryFamily(string id, out AdapterFamily family)
    {
        var parts = id.Split('.', 2);
        family = new(parts[0], parts.Length == 2 ? parts[1] : "", parts.Length == 2 ? parts[1] : id);
        try { family.Validate(); return true; }
        catch (InvalidDataException) { return false; }
    }

    private static void ValidateFamilyId(string adapterId)
    {
        if (!TryFamily(adapterId, out _)) throw new InvalidDataException("无效的引擎或模板标识。");
    }

    private void ValidateReferenceIdentity(string? id, string? version)
    {
        if (string.IsNullOrWhiteSpace(id) || string.IsNullOrWhiteSpace(version))
            throw new InvalidDataException("持久化记录缺少引擎或版本标识。");
        _ = PackagePath(id, version);
    }

    private static bool IsLibraryReadError(Exception error) => error is IOException or InvalidDataException
        or UnauthorizedAccessException or JsonException or GeneratedAdapterException or ArgumentException;

    internal sealed record FamilyState(bool Enabled = true, IReadOnlyList<Activation>? Releases = null,
        IReadOnlyList<string>? ExcludedVersions = null);
    internal sealed record LibraryState
    {
        public int SchemaVersion { get; init; } = 1;
        public Dictionary<string, FamilyState> Families { get; init; } = new(StringComparer.Ordinal);
    }
}
