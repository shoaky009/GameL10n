using System.Diagnostics;
using System.Security.Cryptography;
using System.Text;
using System.Text.RegularExpressions;
using AvgTranslator.Core;
using AvgTranslator.Core.Adapters;
using AvgTranslator.Core.Development;
using AvgTranslator.Core.Patching;
using AvgTranslator.Core.Pipeline;
using AvgTranslator.Core.Storage;
using AvgTranslator.Core.Translation;

// Repairs the generated launch entry with the application's original-source, cache and installer contracts.
internal static class QlieRepair
{
    private const string AdapterId = "engine.qlie-filepack3-systemfont";

    public static async Task<int> RunAsync(string projectDirectory, string workDirectory,
        string adapterSource, string dotnetRoot, bool install)
    {
        try
        {
            EnsureDesktopClosed();
            projectDirectory = Path.GetFullPath(projectDirectory);
            workDirectory = Path.GetFullPath(workDirectory);
            adapterSource = Path.GetFullPath(adapterSource);
            var project = await LocalFiles.ReadJsonAsync<GameProject>(Path.Combine(projectDirectory, "game.json"))
                ?? throw new InvalidDataException("项目不存在。");
            if (project.AdapterId != AdapterId)
                throw new InvalidDataException("此修复仅支持 engine.qlie-filepack3-systemfont 适配器项目。");
            if (!Path.GetFileName(projectDirectory).Equals(project.Id, StringComparison.OrdinalIgnoreCase))
                throw new InvalidDataException("项目 ID 与目录不一致。");
            if (install) PatchInstaller.EnsureGameClosed(project.GameDirectory);
            var dataDirectory = Directory.GetParent(projectDirectory)!.Parent!.FullName;
            var adaptation = Path.Combine(projectDirectory, "adaptation", "current");
            var statePath = Path.Combine(adaptation, "session.json");
            var previousState = await LocalFiles.ReadJsonAsync<AdaptationSession>(statePath);
            if (previousState?.VerifiedPackage is { } previous
                && (previous.Id != AdapterId || previous.Family?.AdapterId != AdapterId))
                throw new InvalidDataException("开发记录不属于 QLIE systemfont 适配器。");
            foreach (var name in new[] { "QlieLauncher.exe", "NotoSansSC-VF.ttf", "NotoSerifSC-VF.ttf",
                "NotoSans-LICENSE.txt", "NotoSerif-LICENSE.txt", "FONT-DERIVATION.txt" })
                if (!File.Exists(Path.Combine(adapterSource, "tools", name)))
                    throw new FileNotFoundException("修复源码缺少所需工具或字体：" + name);

            Directory.CreateDirectory(workDirectory);
            var toolchain = new PinnedDotNetToolchain(dotnetRoot, "10.0.402", "10.0.13");
            var runner = new AdapterProcessRunner();
            var family = new AdapterFamily("engine", "qlie-filepack3-systemfont", "QLIE FilePackVer3.0 + system_fix");
            Console.WriteLine("正在编译 QLIE 适配器并检查独立启动入口。");
            var package = await new GeneratedAdapterCompiler(toolchain, runner).CompileWorkspaceAsync(adapterSource,
                Path.Combine(workDirectory, "build"),
                ["**/data0.pack", "**/*.fkey", "**/*.exe", "**/config.txt", "**/*.exe.le.config", "**/qlie-l10n/**"],
                family: family,
                releaseFiles: new[] { "QlieArchive.cs", "QlieEncoding.cs", "QlieScript.cs", "QlieKey.cs", "QlieCli.cs",
                    "QlieFont.cs", "QlieCodec.cs", "QlieMersenneTwister.cs" }
                    .Select(path => new AdapterReleaseFile("src/" + path, "source", "QLIE 封包、脚本、编码与字体实现"))
                    .Concat(new[] { "QlieLauncher.exe", "NotoSansSC-VF.ttf", "NotoSerifSC-VF.ttf" }
                        .Select(path => new AdapterReleaseFile("tools/" + path, "asset", "生成汉化启动器与字库所需素材")))
                    .Concat(new[] { "NotoSans-LICENSE.txt", "NotoSerif-LICENSE.txt", "FONT-DERIVATION.txt" }
                        .Select(path => new AdapterReleaseFile("tools/" + path, "license", "字体许可与衍生说明"))).ToArray());
            var adapter = new GeneratedGameAdapter(package, toolchain, runner, Path.Combine(workDirectory, "run"));
            var pipeline = new TranslationPipeline(dataDirectory);
            // The engine can rewrite live config.txt after a failed launch. Reuse verified originals instead of promoting it to source.
            var snapshot = await LocalFiles.ReadJsonAsync<SourceSnapshot>(Path.Combine(projectDirectory, "source.json"))
                ?? throw new InvalidDataException("未找到项目的原始资源快照。");
            if (snapshot.AdapterId != AdapterId || snapshot.Files.Count == 0)
                throw new InvalidDataException("原始资源快照不属于 QLIE systemfont 适配器。");
            var sourceDirectory = LocalFiles.UnderRoot(projectDirectory, snapshot.Directory);
            foreach (var source in snapshot.Files)
                if (await LocalFiles.HashFileAsync(LocalFiles.UnderRoot(sourceDirectory, source.RelativePath)) != source.Sha256)
                    throw new InvalidDataException("原始资源快照校验失败：" + source.RelativePath);
            var probe = await adapter.ProbeAsync(sourceDirectory)
                ?? throw new InvalidDataException("当前适配器无法识别原始资源快照。");
            var originals = snapshot.Files.Select(source => source.RelativePath.Replace('\\', '/'))
                .ToHashSet(StringComparer.OrdinalIgnoreCase);
            if (probe.RequiredFiles.Any(path => !originals.Contains(path.Replace('\\', '/'))))
                throw new InvalidDataException("原始资源快照缺少当前适配器需要的文件。");
            var extraction = await adapter.ExtractAsync(sourceDirectory);
            var session = new GameSession(project.Id, project.GameDirectory, projectDirectory, sourceDirectory,
                probe, snapshot.Files, extraction, adapter);
            var ids = await LocalFiles.ReadJsonAsync<string[]>(Path.Combine(projectDirectory, "trial-selection.json"))
                ?? throw new InvalidDataException("请先在应用中选择已完成翻译的试译文本。");
            if (ids.Length == 0 || ids.Distinct(StringComparer.Ordinal).Count() != ids.Length)
                throw new InvalidDataException("试译选区为空或包含重复文本。");
            var terms = new Dictionary<string, string>(StringComparer.Ordinal);
            foreach (var line in project.Settings.Terminology.Split('\n', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries))
            {
                int separator = line.IndexOf('=');
                if (separator < 1 || separator == line.Length - 1)
                    throw new InvalidDataException("术语格式必须为原文=译文。");
                terms.Add(line[..separator].Trim(), line[(separator + 1)..].Trim());
            }
            var policy = new TranslationPolicy(project.Settings.TargetLanguage, project.Settings.Style, terms);
            var memory = new TranslationMemory(Path.Combine(projectDirectory, "memory"), Path.Combine(dataDirectory, "memory", project.Id));
            var units = session.Extraction.Units.ToDictionary(unit => unit.Id);
            foreach (var id in ids)
                if (!units.TryGetValue(id, out var unit) || await memory.FindAsync(unit, policy) is null)
                    throw new InvalidDataException("试译选区中存在未缓存的译文，请选择已经翻译的文本：" + id);

            Console.WriteLine("正在验证原文回填、中文回填和启动程序结构。");
            var verification = await new GeneratedAdapterVerifier().VerifyAsync(adapter, session.SourceDirectory,
                Path.Combine(workDirectory, "verification"), sourceIsSnapshot: true);
            if (!verification.Passed)
                throw new InvalidDataException(string.Join(Environment.NewLine, verification.Diagnostics));
            var result = await pipeline.RunAsync(session, new CacheOnlyProvider(), policy, new RunOptions(TrialUnitIds: ids));
            if (result.TranslationUsage.TotalTokens != 0 || result.CacheHits != ids.Length)
                throw new InvalidDataException("修复必须完全复用已有译文。");
            if (result.LaunchExecutable != "启动汉化.exe")
                throw new InvalidDataException("修复补丁未声明独立汉化启动程序。");

            EnsureDesktopClosed();
            // Hold the existing project lock while replacing its active package and resumable development record.
            await using var projectLock = new FileStream(Path.Combine(projectDirectory, ".task.lock"),
                FileMode.OpenOrCreate, FileAccess.ReadWrite, FileShare.None);
            var published = await new LocalAdapterPackageStore(Path.Combine(dataDirectory, "extensions"))
                .PublishAndActivateAsync(project.Id, package, verification.Diagnostics);
            await new GameProjectStore(dataDirectory).SaveAsync(project with
            {
                AdapterId = published.Id, AdapterVersion = published.Version, UpdatedAt = DateTimeOffset.UtcNow
            });
            await UpdateDevelopmentAsync(adaptation, adapterSource, project, session, published, verification.Diagnostics);
            if (install)
            {
                PatchInstaller.EnsureGameClosed(project.GameDirectory);
                var installer = new PatchInstaller(projectDirectory);
                await RecoverKnownFontResetAsync(session, result.PatchDirectory);
                await installer.InstallAsync(project.GameDirectory, result.PatchDirectory);
                Console.WriteLine("修复补丁已安装。请启动：" + Path.Combine(project.GameDirectory, result.LaunchExecutable));
            }
            else Console.WriteLine("修复补丁已生成：" + result.PatchDirectory);
            Console.WriteLine($"复用 {result.CacheHits} 条译文，API 调用为 0；原 EXE 保留。实际游戏启动和显示尚待确认。");
            return 0;
        }
        catch (Exception error)
        {
            Console.Error.WriteLine(error);
            return 1;
        }
    }

    private static async Task RecoverKnownFontResetAsync(GameSession session, string patchDirectory)
    {
        var installer = new PatchInstaller(session.ProjectDirectory);
        using var installLock = await installer.OpenInstallationAsync(session.GameDirectory);
        var journalPath = installer.JournalPath;
        if (!File.Exists(journalPath)) return;
        var journalHash = await LocalFiles.HashFileAsync(journalPath);
        var journal = await LocalFiles.ReadJsonAsync<InstalledJournal>(journalPath)
            ?? throw new InvalidDataException("安装记录为空，本次修复未安装。");
        if (journal.SchemaVersion != 1 || journal.Entries is null)
            throw new InvalidDataException("安装记录无效，本次修复未安装。");
        var entries = journal.Entries.ToDictionary(entry => entry.RelativePath.Replace('\\', '/'), StringComparer.OrdinalIgnoreCase);
        var manifest = await LocalFiles.ReadJsonAsync<PatchManifest>(Path.Combine(patchDirectory, PatchInstaller.ManifestFilename))
            ?? throw new InvalidDataException("修复补丁缺少清单。");
        if (manifest.SchemaVersion != 1 || manifest.Files is null)
            throw new InvalidDataException("修复补丁清单无效。");
        var configs = session.Sources.Where(source => Path.GetFileName(source.RelativePath)
            .Equals("config.txt", StringComparison.OrdinalIgnoreCase)).Select(source => source.RelativePath.Replace('\\', '/'))
            .ToHashSet(StringComparer.OrdinalIgnoreCase);
        var recoveries = new List<(string Path, string LiveHash, string InstalledHash, byte[] Bytes)>();
        foreach (var file in manifest.Files)
        {
            var relative = file.RelativePath.Replace('\\', '/');
            var target = LocalFiles.UnderRoot(session.GameDirectory, relative);
            var current = File.Exists(target) ? await LocalFiles.HashFileAsync(target) : null;
            entries.TryGetValue(relative, out var entry);
            if (file.ExpectedOriginalSha256 is { } expected && !SameHash(entry?.OriginalSha256 ?? current, expected))
                throw new InvalidDataException("游戏原始资源与补丁不符，本次修复未安装：" + relative);
            if (entry is null || SameHash(current, entry.InstalledSha256)) continue;
            // The normal installer reconciles interrupted copies/restores from their recorded hashes.
            // This helper only repairs font settings rewritten by the game; it must not block that recovery.
            if ((entry.PendingSha256 is not null &&
                 (SameHash(current, entry.PendingSha256) || SameHash(current, entry.OriginalSha256))) ||
                (entry.Restoring && SameHash(current, entry.OriginalSha256))) continue;
            if (!configs.Contains(relative) || current is null)
                throw new InvalidDataException("已有补丁文件发生其他修改，本次修复未安装：" + relative);
            var live = await File.ReadAllBytesAsync(target);
            if (!SameHash(Hash(live), current)) throw new IOException("检查期间游戏配置发生变化，请关闭游戏后重试。");
            var prior = await FindInstalledConfigAsync(session.ProjectDirectory, relative, entry.InstalledSha256!);
            var restored = RestoreFontNames(live, prior);
            if (!SameHash(Hash(restored), entry.InstalledSha256))
                throw new InvalidDataException("游戏配置除已知字体重置外还有其他变化，本次修复未安装：" + relative);
            recoveries.Add((target, current, entry.InstalledSha256!, restored));
        }
        foreach (var recovery in recoveries)
        {
            var temporary = Path.Combine(Path.GetDirectoryName(recovery.Path)!, ".qlie-font-reset-" + Guid.NewGuid().ToString("N") + ".tmp");
            try
            {
                await using (var stream = new FileStream(temporary, FileMode.CreateNew, FileAccess.Write, FileShare.None,
                    4096, FileOptions.Asynchronous | FileOptions.WriteThrough))
                {
                    await stream.WriteAsync(recovery.Bytes);
                    await stream.FlushAsync();
                    stream.Flush(true);
                }
                if (!SameHash(await LocalFiles.HashFileAsync(temporary), recovery.InstalledHash)
                    || !SameHash(await LocalFiles.HashFileAsync(recovery.Path), recovery.LiveHash)
                    || !SameHash(await LocalFiles.HashFileAsync(journalPath), journalHash))
                    throw new IOException("检查期间配置或安装记录发生变化，本次未覆盖配置。");
                File.Move(temporary, recovery.Path, true);
                Console.WriteLine("已恢复游戏重置的七个汉化字体名称，其他配置字节保持不变。");
            }
            finally { if (File.Exists(temporary)) File.Delete(temporary); }
        }
    }

    private static async Task<byte[]> FindInstalledConfigAsync(string projectDirectory, string relative, string installedHash)
    {
        var patches = Path.Combine(projectDirectory, "patches");
        if (Directory.Exists(patches))
            foreach (var patch in Directory.EnumerateDirectories(patches))
            {
                var candidate = LocalFiles.UnderRoot(patch, relative);
                if (!File.Exists(candidate) || !SameHash(await LocalFiles.HashFileAsync(candidate), installedHash)) continue;
                var bytes = await File.ReadAllBytesAsync(candidate);
                if (SameHash(Hash(bytes), installedHash)) return bytes;
            }
        throw new InvalidDataException("未找到与安装记录一致的旧补丁配置，本次修复未安装。");
    }

    private static byte[] RestoreFontNames(byte[] live, byte[] prior)
    {
        // Latin1 maps bytes one to one, preserving every byte outside the seven selected values.
        var names = new Regex(@"(?m)^(Font\[([0-46-7])\]\.Name=)([^\r\n]*)");
        var expectedMatches = names.Matches(Encoding.Latin1.GetString(prior)).Cast<Match>().ToArray();
        var liveText = Encoding.Latin1.GetString(live);
        var liveMatches = names.Matches(liveText).Cast<Match>().ToArray();
        if (expectedMatches.Length != 7 || liveMatches.Length != 7
            || expectedMatches.Select(match => match.Groups[2].Value).Distinct().Count() != 7
            || liveMatches.Select(match => match.Groups[2].Value).Distinct().Count() != 7
            || expectedMatches.Any(match => !Regex.IsMatch(match.Groups[3].Value, @"^QLie Han (Sans|Serif) [0-9A-F]{8}( Regular)?$")))
            throw new InvalidDataException("字体配置不符合已知重置格式，本次修复未安装。");
        var expected = expectedMatches.ToDictionary(match => match.Groups[2].Value, match => match.Groups[3].Value);
        Encoding.RegisterProvider(CodePagesEncodingProvider.Instance);
        var gothic = Encoding.Latin1.GetString(Encoding.GetEncoding(932).GetBytes("ＭＳ ゴシック"));
        var mincho = Encoding.Latin1.GetString(Encoding.GetEncoding(932).GetBytes("ＭＳ 明朝"));
        return Encoding.Latin1.GetBytes(names.Replace(liveText, match =>
        {
            var value = match.Groups[3].Value;
            var restored = expected[match.Groups[2].Value];
            var originalSerif = value == mincho && restored.StartsWith("QLie Han Serif ", StringComparison.Ordinal);
            // A game left running during an update can save the preceding patch's names when it exits.
            // Only our same-style generated names qualify; the caller still requires an exact full-file hash after repair.
            var previousPatch = Regex.IsMatch(value, restored.StartsWith("QLie Han Serif ", StringComparison.Ordinal)
                ? @"^QLie Han Serif [0-9A-F]{8}( Regular)?$"
                : @"^QLie Han Sans [0-9A-F]{8}( Regular)?$");
            if (value != gothic && value != restored && !originalSerif && !previousPatch)
                throw new InvalidDataException("字体名称含已知重置之外的修改，本次修复未安装。");
            return match.Groups[1].Value + restored;
        }));
    }

    private static string Hash(byte[] bytes) => Convert.ToHexStringLower(SHA256.HashData(bytes));
    private static bool SameHash(string? first, string? second) => string.Equals(first, second, StringComparison.OrdinalIgnoreCase);
    private sealed record InstalledJournal(int SchemaVersion, IReadOnlyList<InstalledEntry> Entries);
    private sealed record InstalledEntry(string RelativePath, string? OriginalSha256, string? InstalledSha256,
        string? PendingSha256, bool Restoring);

    private static async Task UpdateDevelopmentAsync(string adaptation, string adapterSource, GameProject project,
        GameSession session, GeneratedAdapterPackage published, IReadOnlyList<string> diagnostics)
    {
        var sources = Path.Combine(adaptation, "src");
        foreach (var file in Directory.EnumerateFiles(adapterSource, "*", SearchOption.AllDirectories))
        {
            var destination = LocalFiles.UnderRoot(sources, Path.GetRelativePath(adapterSource, file));
            Directory.CreateDirectory(Path.GetDirectoryName(destination)!);
            File.Copy(file, destination, true);
        }
        File.Delete(LocalFiles.UnderRoot(sources, "tools/launch.ps1"));

        var resources = Path.Combine(adaptation, "resources");
        var required = session.Sources.Select(source => source.RelativePath.Replace('\\', '/'))
            .ToHashSet(StringComparer.OrdinalIgnoreCase);
        // Remove stale generated sidecars; the continuation must analyze the same originals as verification.
        if (Directory.Exists(resources))
            foreach (var file in Directory.EnumerateFiles(resources, "*", SearchOption.AllDirectories))
            {
                var relative = Path.GetRelativePath(resources, file).Replace('\\', '/');
                if (!required.Contains(relative)) File.Delete(LocalFiles.UnderRoot(resources, relative));
            }
        foreach (var resource in session.Sources)
        {
            var destination = LocalFiles.UnderRoot(resources, resource.RelativePath);
            Directory.CreateDirectory(Path.GetDirectoryName(destination)!);
            File.Copy(LocalFiles.UnderRoot(session.SourceDirectory, resource.RelativePath), destination, true);
        }

        var statePath = Path.Combine(adaptation, "session.json");
        var state = await LocalFiles.ReadJsonAsync<AdaptationSession>(statePath)
            ?? new AdaptationSession { GameId = project.Id, GameDirectory = Path.GetFullPath(project.GameDirectory) };
        const string summary = "已统一汉化字体内部名称与游戏配置，并将字号度量对齐原字体。原文、中文回填和程序结构检查通过，试译完全复用缓存；实际游戏排版尚待确认。";
        const string correction = "当前 QLIE 适配器保留原 EXE，输出独立 启动汉化.exe 并声明 build.launchExecutable，通过本机 Locale Emulator 沿用原 EXE 的日语配置，临时字体保留到游戏退出。system_fix 排除比例字体，七个非外字槽改成 system；槽5独立外字不变。引擎按 elfFullName 匹配配置，Windows 为变体 Regular 实例附加后缀，但 Locale Emulator 的 AdjustFontDataInternal 从 name 表 ID4 重新读取日语完整名称（缺失时用英语）。旧字体 ID4 误写为族名，导致宿主探针通过而转区游戏回退；实际游戏内存与 Locale Emulator 开源实现均印证此差异。生成字体的 ID4 现在与 config.txt、launch.xml 一致为族名加 Regular，ID1/16 保持族名，字体指纹包含元数据版本。启动器检查 name 表及原生枚举，不代表已运行游戏。参考 VNTextPatch-net8 的 QlieScript/SjisTunnelEncoding 及 VNTranslationTools 的 VNTextProxy，可提供脚本复用与另一种运行时字符转换方案，但不能只接入提取器就认为显示可用。原始快照位于 resources/，源码和工具在 src/，不要把游戏回退后的配置提升为原始资源。试译复用缓存且 API 调用为0；真实游戏内显示尚待确认。";
        const string metrics = " 字号检查：此 QLIE 路径传递正的 LOGFONT.lfHeight 和半角 lfWidth；Noto 原始 WinAscent/Descent 总和大于 em，xAvgCharWidth 为全角，导致字符缩小并横向压缩。字体生成器按 MS Gothic/Mincho 的 256 em、220/36 升降部和128平均宽度比例归一化 OS/2 与 hhea 度量，移除冲突的 MVAR；不改变原脚本字号和字形轮廓。使用与引擎一致的字高/字宽请求比较实际尺寸，不能只测负字号且宽度为0的普通 GDI 绘制。";
        var checkpoint = correction + metrics;
        state.ReleaseFiles = published.ReleaseFiles;
        state.VerifiedPackage = published;
        state.VerificationDiagnostics = diagnostics.ToArray();
        state.Stage = "适配完成";
        state.LastStatus = AdaptationStatus.Activated;
        state.NextAction = "安装试译补丁后双击 启动汉化.exe，检查游戏启动、视频和中文显示。";
        state.Blocker = null;
        state.Summary = summary;
        state.Checkpoint = checkpoint;
        state.ProgressNote = new("correction", summary, DateTimeOffset.UtcNow);
        state.LastBreakthrough = null;
        state.ResourceHashes = session.Sources.ToDictionary(source => source.RelativePath, source => source.Sha256,
            StringComparer.OrdinalIgnoreCase);
        state.Messages.Clear();
        state.PendingCalls.Clear();
        state.RunningToolId = null;
        state.Messages.Add(new AgentMessage("user", checkpoint));
        Directory.CreateDirectory(Path.Combine(adaptation, "work"));
        await File.WriteAllTextAsync(Path.Combine(adaptation, "work", "status.md"), checkpoint);
        await LocalFiles.WriteJsonAsync(statePath, state);
    }

    private static void EnsureDesktopClosed()
    {
        var processes = Process.GetProcessesByName("AvgTranslator.Desktop");
        try
        {
            if (processes.Length != 0) throw new InvalidOperationException("请先关闭 AvgTranslator，再修复适配器和试译补丁。");
        }
        finally { foreach (var process in processes) process.Dispose(); }
    }

    private sealed class CacheOnlyProvider : ITranslationProvider
    {
        public string Identity => "local-cache-only";
        public Task<TranslationResponse> TranslateAsync(TranslationRequest request, CancellationToken cancellationToken = default)
            => throw new InvalidOperationException("缺少已缓存的译文，本工具不会调用翻译 API。");
    }
}
