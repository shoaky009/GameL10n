using System.Text.Json;
using GameL10n.Core;
using GameL10n.Core.Development;
using GameL10n.Core.Pipeline;
using GameL10n.Core.Storage;
using GameL10n.Core.Translation;
using GameL10n.Core.Serialization;

internal sealed record UnitRename(string PreviousId, string CurrentId);
internal sealed record CoverageUpdateManifest(string AdapterId, string PreviousVersion, string SourceFingerprint,
    IReadOnlyDictionary<string, string> SourceFiles, TextCoverage Coverage, IReadOnlyList<UnitRename> Renames,
    IReadOnlyList<string>? AddedSourceFiles = null, IReadOnlyDictionary<string, string>? AddedResources = null,
    IReadOnlyList<string>? ResourcePatterns = null, IReadOnlyList<AdapterReleaseFile>? ReleaseFiles = null);

// Deterministic update of reviewed adapter sources. Uses the existing original snapshot and
// extraction/memory contracts; no model, game writes, installation or new original backups.
internal static class AdapterCoverageUpdate
{
    public static async Task<int> RunAsync(string projectDirectory, string workDirectory, string updateDirectory,
        string dotnetRoot, bool apply)
    {
        try
        {
            projectDirectory = Path.GetFullPath(projectDirectory);
            workDirectory = Path.GetFullPath(workDirectory);
            updateDirectory = Path.GetFullPath(updateDirectory);
            var manifest = await Read<CoverageUpdateManifest>(Path.Combine(updateDirectory, "update.json"));
            manifest.Coverage.Validate();
            var project = await Read<GameProject>(Path.Combine(projectDirectory, "game.json"));
            if (Path.GetFileName(projectDirectory) != project.Id || project.AdapterId != manifest.AdapterId)
                throw new InvalidDataException("更新不适用于此项目的适配器。");
            var dataDirectory = Directory.GetParent(projectDirectory)!.Parent!.FullName;
            var current = Path.Combine(projectDirectory, "adaptation", "current");
            var state = await Read<AdaptationSession>(Path.Combine(current, "session.json"));
            if (state.GameId != project.Id || state.PendingCalls.Count > 0 || state.RunningToolId is not null)
                throw new InvalidDataException("适配会话尚有未完成操作，请先暂停并保存任务。");
            if (workDirectory.StartsWith(projectDirectory + Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase)
                || workDirectory.Equals(projectDirectory, StringComparison.OrdinalIgnoreCase))
                throw new InvalidDataException("更新验证目录必须位于项目目录之外。");
            await using var projectLock = apply ? new FileStream(Path.Combine(projectDirectory, ".task.lock"),
                FileMode.OpenOrCreate, FileAccess.ReadWrite, FileShare.None) : null;
            var snapshot = await Read<SourceSnapshot>(Path.Combine(projectDirectory, "source.json"));
            var addedResources = manifest.AddedResources ?? new Dictionary<string, string>();
            var baseFiles = snapshot.Files.Where(f => !addedResources.ContainsKey(f.RelativePath)).ToArray();
            if (LocalFiles.HashText(JsonSerializer.Serialize(baseFiles.OrderBy(f => f.RelativePath))) != manifest.SourceFingerprint)
                throw new InvalidDataException("原文版本与已验证的字段布局不一致，未修改项目。");
            var source = LocalFiles.UnderRoot(projectDirectory, snapshot.Directory);
            foreach (var file in snapshot.Files)
                if (await LocalFiles.HashFileAsync(LocalFiles.UnderRoot(source, file.RelativePath)) != file.Sha256)
                    throw new InvalidDataException("原文快照已改变：" + file.RelativePath);

            var updatedFiles = baseFiles.Concat(addedResources.Select(p => new SourceFile(p.Key, p.Value))).OrderBy(f => f.RelativePath).ToArray();
            var updatedFingerprint = LocalFiles.HashText(JsonSerializer.Serialize(updatedFiles));
            var updatedSource = Path.Combine(workDirectory, "source");
            foreach (var file in updatedFiles)
            {
                var original = snapshot.Files.Any(f => f.RelativePath == file.RelativePath)
                    ? LocalFiles.UnderRoot(source, file.RelativePath)
                    : LocalFiles.UnderRoot(Path.Combine(current, "resources"), file.RelativePath);
                if (await LocalFiles.HashFileAsync(original) != file.Sha256)
                    throw new InvalidDataException("新增原文资源与审核版本不一致：" + file.RelativePath);
                await CopyVerified(original, LocalFiles.UnderRoot(updatedSource, file.RelativePath), file.Sha256);
            }

            var sourceDirectory = Path.Combine(current, "src");
            var candidate = Path.Combine(workDirectory, "src");
            // Require the exact reviewed inputs, allowing already-applied overlay files for safe retries.
            // Do not overwrite newer investigation code with an older repair.
            var actualFiles = Directory.GetFiles(sourceDirectory, "*", SearchOption.AllDirectories)
                .Select(f => Path.GetRelativePath(sourceDirectory, f).Replace('\\', '/')).Order().ToArray();
            var addedSources = manifest.AddedSourceFiles ?? [];
            if (!actualFiles.Except(addedSources).Order().SequenceEqual(manifest.SourceFiles.Keys.Order()))
                throw new InvalidDataException("适配源码文件集合已变化，未覆盖后续开发成果。");
            foreach (var (relative, expected) in manifest.SourceFiles)
            {
                var original = LocalFiles.UnderRoot(sourceDirectory, relative);
                var replacement = LocalFiles.UnderRoot(updateDirectory, relative);
                var hash = await LocalFiles.HashFileAsync(original);
                if (hash != expected && (!File.Exists(replacement) || hash != await LocalFiles.HashFileAsync(replacement)))
                    throw new InvalidDataException("适配源码已变化，未覆盖：" + relative);
                var target = LocalFiles.UnderRoot(candidate, relative);
                Directory.CreateDirectory(Path.GetDirectoryName(target)!);
                File.Copy(File.Exists(replacement) ? replacement : original, target, true);
            }
            foreach (var relative in addedSources)
            {
                var replacement = LocalFiles.UnderRoot(updateDirectory, relative);
                var original = LocalFiles.UnderRoot(sourceDirectory, relative);
                if (File.Exists(original) && await LocalFiles.HashFileAsync(original) != await LocalFiles.HashFileAsync(replacement))
                    throw new InvalidDataException("新增源码已存在其他修改，未覆盖：" + relative);
                var target = LocalFiles.UnderRoot(candidate, relative);
                Directory.CreateDirectory(Path.GetDirectoryName(target)!);
                File.Copy(replacement, target, true);
            }
            var previous = state.VerifiedPackage ?? throw new InvalidDataException("缺少已发布适配器的编译信息。");
            var toolchain = new PinnedDotNetToolchain(dotnetRoot, "10.0.402", "10.0.13");
            var runner = new AdapterProcessRunner();
            var compiler = new GeneratedAdapterCompiler(toolchain, runner);
            Console.WriteLine("正在编译已验证字段更新，不调用模型。");
            var package = await compiler.CompileWorkspaceAsync(candidate, Path.Combine(workDirectory, "build"),
                manifest.ResourcePatterns ?? previous.ResourcePatterns!, family: previous.Family,
                releaseFiles: manifest.ReleaseFiles ?? (previous.ReleaseFiles is { } existing
                    ? existing.Select(file => file.Path.StartsWith("src/", StringComparison.Ordinal) ? file.Path[4..] : file.Path)
                    : manifest.SourceFiles.Keys).Concat(addedSources).Distinct(StringComparer.OrdinalIgnoreCase).Select(path =>
                {
                    var packagedPath = path.StartsWith("tools/", StringComparison.Ordinal) ? path : "src/" + path;
                    if (previous.ReleaseFiles?.FirstOrDefault(file => file.Path == packagedPath) is { } selected) return selected;
                    if (packagedPath.StartsWith("src/", StringComparison.Ordinal))
                        return new AdapterReleaseFile(packagedPath, "source", "已审核更新清单中的解析实现");
                    var name = Path.GetFileName(path);
                    var role = path.EndsWith(".dll", StringComparison.OrdinalIgnoreCase) ? "runtime"
                        : name.Contains("LICENSE", StringComparison.OrdinalIgnoreCase) || name.Contains("NOTICE", StringComparison.OrdinalIgnoreCase)
                            || name == "sources.json" ? "license" : null;
                    return role is null ? throw new InvalidDataException("更新清单需要通过 releaseFiles 声明此依赖的用途：" + path)
                        : new AdapterReleaseFile(path, role, "已审核更新清单中的解析依赖或来源声明");
                }).ToArray());
            var adapter = new GeneratedGameAdapter(package, toolchain, runner, Path.Combine(workDirectory, "run"));
            Console.WriteLine("正在检查独立原文样本、原字节往返、中文变长回填及局部试译。");
            var verification = await new GeneratedAdapterVerifier().VerifyAsync(adapter, updatedSource,
                Path.Combine(workDirectory, "verification"), sourceIsSnapshot: true, coverage: manifest.Coverage);
            if (!verification.Passed) throw new InvalidDataException(string.Join("\n", verification.Diagnostics));
            var extraction = await adapter.ExtractAsync(updatedSource);
            var probe = await adapter.ProbeAsync(updatedSource) ?? throw new InvalidDataException("更新未能识别原文快照。");
            if (!probe.RequiredFiles.ToHashSet(StringComparer.Ordinal).SetEquals(updatedFiles.Select(f => f.RelativePath)))
                throw new InvalidDataException("更新适配器未完整识别原文资源。");
            var old = await new ExtractionStore(projectDirectory).LoadAsync(manifest.SourceFingerprint, manifest.AdapterId, manifest.PreviousVersion)
                ?? throw new InvalidDataException("原版本提取缓存不存在，不能可靠迁移字段标识。");
            var aliases = ResolveRenames(old, extraction, manifest.Renames);
            var newUnits = extraction.Units.ToDictionary(u => u.Id);
            foreach (var unit in old.Units.Where(u => !manifest.Renames.Any(r => r.PreviousId == u.Id)))
                if (!newUnits.TryGetValue(unit.Id, out var updated) || unit.SourceText != updated.SourceText
                    || unit.Context != updated.Context || unit.Kind != updated.Kind || unit.Speaker != updated.Speaker
                    || unit.ResourcePath != updated.ResourcePath || unit.Locator != updated.Locator
                    || !(unit.ProtectedTokens ?? []).Order().SequenceEqual((updated.ProtectedTokens ?? []).Order()))
                    throw new InvalidDataException("更新意外改变已有文本，未修改项目：" + unit.Id);
            var policy = Policy(project);
            var entries = new Dictionary<string, MemoryEntry>(StringComparer.Ordinal);
            var memoryDirectory = Path.Combine(projectDirectory, "memory");
            foreach (var file in Directory.EnumerateFiles(memoryDirectory, "*.json")
                .Where(f => Path.GetFileNameWithoutExtension(f) is { Length: 2 } prefix && prefix.All(Uri.IsHexDigit)))
                foreach (var pair in (await Read<MemoryShard>(file)).Entries) entries[pair.Key] = pair.Value;
            var memory = new TranslationMemory(memoryDirectory);
            var renamedEntries = new List<MemoryEntry>();
            foreach (var (before, after) in aliases)
                if (!entries.ContainsKey(memory.Key(after, policy)) && entries.TryGetValue(memory.Key(before, policy), out var entry)
                    && entry.SourceText == after.SourceText && TextValidation.Validate(after, entry.Translation).Count == 0)
                    renamedEntries.Add(entry with { Key = memory.Key(after, policy), Context = after.Context });
            foreach (var entry in renamedEntries) entries[entry.Key] = entry;
            int reusable = extraction.Units.Count(u => entries.TryGetValue(memory.Key(u, policy), out var entry)
                && entry.SourceText == u.SourceText && entry.Context == u.Context && TextValidation.Validate(u, entry.Translation).Count == 0);
            var edits = await TranslationEdits.LoadAsync(projectDirectory);
            var migratedEdits = RenameEdits(edits.Entries, aliases);
            var migratedCandidates = RenameEdits(edits.Candidates ?? new Dictionary<string, TranslationEdit>(), aliases);
            var selectionPath = Path.Combine(projectDirectory, "trial-selection.json");
            var selection = await LocalFiles.ReadJsonAsync<string[]>(selectionPath, DiagnosticJson.TypeInfo<string[]>(DiagnosticJson.Storage));
            Console.WriteLine($"提取 {old.Units.Count} → {extraction.Units.Count} 条；可复用 {reusable} 条缓存，校正 {aliases.Count} 个字段标识。API 调用为 0。");
            if (!apply)
            {
                Console.WriteLine("验证通过；项目未修改。使用 -Apply 将更新写入原项目。");
                return 0;
            }

            // Commit reusable data first. Old immutable packages/extraction stay available if interrupted;
            // rerunning the same update finishes the remaining small state writes.
            await memory.StoreBatchAsync(renamedEntries);
            if (migratedEdits.Count != edits.Entries.Count || migratedCandidates.Count != (edits.Candidates?.Count ?? 0))
                await (edits with { Entries = migratedEdits, Candidates = migratedCandidates }).SaveAsync(projectDirectory);
            var renameIds = aliases.ToDictionary(a => a.Before.Id, a => a.After.Id);
            if (snapshot.AdapterVersion == manifest.PreviousVersion && selection is not null && selection.Any(renameIds.ContainsKey))
                await LocalFiles.WriteJsonAsync(selectionPath, selection.Select(id => renameIds.GetValueOrDefault(id, id)).Distinct().ToArray(), DiagnosticJson.TypeInfo<string[]>(DiagnosticJson.Storage));
            var committedSource = Path.Combine(projectDirectory, "sources", updatedFingerprint);
            foreach (var file in updatedFiles)
                await CopyVerified(LocalFiles.UnderRoot(updatedSource, file.RelativePath),
                    LocalFiles.UnderRoot(committedSource, file.RelativePath), file.Sha256);
            await new ExtractionStore(projectDirectory).StoreAsync(updatedFingerprint, package.Id, package.Version, extraction);
            var store = new LocalAdapterPackageStore(Path.Combine(dataDirectory, "extensions"), Path.Combine(projectDirectory, "adaptation"));
            var published = await store.PublishAndActivateAsync(project.Id, package, verification.Diagnostics);
            await store.RecordCompatibilityAsync(published.Family!, new(project.Id, project.Name, probe.Description,
                published.Version, "resource-roundtrip-passed"));
            foreach (var relative in manifest.SourceFiles.Keys.Concat(addedSources).Where(r => File.Exists(LocalFiles.UnderRoot(updateDirectory, r))))
            {
                var target = LocalFiles.UnderRoot(sourceDirectory, relative);
                Directory.CreateDirectory(Path.GetDirectoryName(target)!);
                File.Copy(LocalFiles.UnderRoot(candidate, relative), target, true);
            }
            state.TextCoverage = manifest.Coverage;
            state.VerifiedCoverageFingerprint = manifest.Coverage.Fingerprint();
            state.ReleaseFiles = package.ReleaseFiles;
            state.VerifiedPackage = published;
            state.VerificationRevision = GeneratedAdapterVerifier.Revision;
            state.VerificationDiagnostics = verification.Diagnostics.ToArray();
            state.LastStatus = AdaptationStatus.Activated;
            state.Stage = "部分文字可试译";
            state.Blocker = null;
            state.Summary = $"已补入验证过的文字字段，现有 {extraction.Units.Count} 条文本。已有译文保留，新增文本可继续翻译；仍有部分界面待覆盖。";
            state.NextAction = "继续翻译新增文本并生成补丁。未覆盖项：" + string.Join("；", manifest.Coverage.Pending);
            state.Notes = state.Checkpoint = state.Summary + "\n" + string.Join("\n", extraction.Warnings)
                + "\n新增字段的代表原文见 TextCoverage.Samples；沿用现有 Unicode/TMP 回填，继续处理 TextCoverage.Pending。";
            // The old conversation remains available, but the next continuation must see the reviewed update first.
            state.Messages.Add(new AgentMessage("user", state.Notes));
            await LocalFiles.WriteJsonAsync(Path.Combine(current, "session.json"), state, DiagnosticJson.TypeInfo<AdaptationSession>(DiagnosticJson.Storage));
            await LocalFiles.WriteJsonAsync(Path.Combine(projectDirectory, "source.json"), snapshot with
                { Directory = Path.GetRelativePath(projectDirectory, committedSource), Files = updatedFiles,
                    AdapterId = published.Id, AdapterVersion = published.Version, Probe = probe }, DiagnosticJson.TypeInfo<SourceSnapshot>(DiagnosticJson.Storage));
            await new GameProjectStore(dataDirectory).SaveAsync(project with
                { AdapterId = published.Id, AdapterVersion = published.Version, UpdatedAt = DateTimeOffset.UtcNow });
            Console.WriteLine("项目已更新。打开项目即可翻译新增文本，无需重新智能识别；游戏安装目录未修改。");
            return 0;
        }
        catch (Exception error) { Console.Error.WriteLine(error); return 1; }
    }

    private static async Task<T> Read<T>(string path) => await LocalFiles.ReadJsonAsync<T>(path, DiagnosticJson.TypeInfo<T>(DiagnosticJson.Storage))
        ?? throw new InvalidDataException("缺少有效记录：" + path);

    private static async Task CopyVerified(string original, string target, string hash)
    {
        if (File.Exists(target) && await LocalFiles.HashFileAsync(target) == hash) return;
        Directory.CreateDirectory(Path.GetDirectoryName(target)!);
        File.Copy(original, target, true);
        if (await LocalFiles.HashFileAsync(target) != hash) throw new IOException("资源复制校验失败：" + target);
    }

    private static TranslationPolicy Policy(GameProject project) => new(project.Settings.TargetLanguage, project.Settings.Style,
        project.Settings.Terminology.Split('\n', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries)
            .Select(line => line.Split('=', 2, StringSplitOptions.TrimEntries)).ToDictionary(parts => parts[0], parts => parts[1]));

    private static List<(TranslationUnit Before, TranslationUnit After)> ResolveRenames(ExtractionResult old, ExtractionResult current,
        IReadOnlyList<UnitRename> renames)
    {
        var before = old.Units.ToDictionary(u => u.Id);
        var after = current.Units.ToDictionary(u => u.Id);
        return renames.Select(rename =>
        {
            if (!before.TryGetValue(rename.PreviousId, out var a) || !after.TryGetValue(rename.CurrentId, out var b)
                || a.SourceText != b.SourceText || a.ResourcePath != b.ResourcePath || a.Kind != b.Kind || a.Speaker != b.Speaker)
                throw new InvalidDataException("字段迁移的原文/类型不一致，未复用旧译文：" + rename.PreviousId);
            return (a, b);
        }).ToList();
    }

    private static Dictionary<string, TranslationEdit> RenameEdits(IReadOnlyDictionary<string, TranslationEdit> entries,
        List<(TranslationUnit Before, TranslationUnit After)> aliases)
    {
        var result = new Dictionary<string, TranslationEdit>(entries);
        foreach (var (before, after) in aliases)
            if (!result.ContainsKey(after.Id) && entries.TryGetValue(before.Id, out var edit)
                && edit.SourceSignature == TranslationEdits.Signature(before) && TextValidation.Validate(after, edit.Text).Count == 0)
                result[after.Id] = edit with { SourceSignature = TranslationEdits.Signature(after) };
        return result;
    }
}
