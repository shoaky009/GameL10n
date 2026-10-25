using AvgTranslator.Core;
using AvgTranslator.Core.Adapters;
using AvgTranslator.Core.Development;
using AvgTranslator.Core.Patching;
using AvgTranslator.Core.Pipeline;
using AvgTranslator.Core.Storage;
using AvgTranslator.Core.Translation;

// Uses the application's package, cache, verification and installation contracts.
// No provider is configured: a missing cached translation fails before patch generation.
internal static class System4Repair
{
    public static async Task<int> RunAsync(string projectDirectory, string workDirectory,
        string adapterSource, string dotnetRoot, bool install)
    {
        try
        {
            projectDirectory = Path.GetFullPath(projectDirectory);
            workDirectory = Path.GetFullPath(workDirectory);
            var project = await LocalFiles.ReadJsonAsync<GameProject>(Path.Combine(projectDirectory, "game.json"))
                ?? throw new InvalidDataException("项目不存在。");
            var dataDirectory = Directory.GetParent(projectDirectory)!.Parent!.FullName;
            if (!Path.GetFileName(projectDirectory).Equals(project.Id, StringComparison.OrdinalIgnoreCase))
                throw new InvalidDataException("项目 ID 与目录不一致。");
            Directory.CreateDirectory(workDirectory);
            var toolchain = new PinnedDotNetToolchain(dotnetRoot, "10.0.402", "10.0.13");
            var runner = new AdapterProcessRunner();
            var family = new AdapterFamily("engine", "system4-ain14-msg1-fnl", "System4 AIN v14 MSG1 + FNL");
            var compiler = new GeneratedAdapterCompiler(toolchain, runner);
            Console.WriteLine("正在编译 System4 适配器并验证运行程序版本。");
            var package = await compiler.CompileWorkspaceAsync(adapterSource, Path.Combine(workDirectory, "build"),
                ["**/*.ain", "**/*.fnl", "**/*.exe"], family: family);
            var adapter = new GeneratedGameAdapter(package, toolchain, runner, Path.Combine(workDirectory, "run"));
            var pipeline = new TranslationPipeline(dataDirectory);
            var session = await pipeline.AnalyzeAsync(project.GameDirectory, adapter, projectId: project.Id);
            var ids = await LocalFiles.ReadJsonAsync<string[]>(Path.Combine(projectDirectory, "trial-selection.json"))
                ?? throw new InvalidDataException("请先在应用中选择已完成翻译的试译文本。");
            var terms = new Dictionary<string, string>(StringComparer.Ordinal);
            foreach (var line in project.Settings.Terminology.Split('\n', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries))
            {
                int separator = line.IndexOf('=');
                if (separator < 1 || separator == line.Length - 1) throw new InvalidDataException("术语格式必须为原文=译文。");
                terms.Add(line[..separator].Trim(), line[(separator + 1)..].Trim());
            }
            var policy = new TranslationPolicy(project.Settings.TargetLanguage, project.Settings.Style, terms);
            var memory = new TranslationMemory(Path.Combine(projectDirectory, "memory"));
            var units = session.Extraction.Units.ToDictionary(unit => unit.Id);
            if (ids.Length == 0) throw new InvalidDataException("试译选区为空。");
            foreach (var id in ids)
                if (!units.TryGetValue(id, out var unit) || await memory.FindAsync(unit, policy) is null)
                    throw new InvalidDataException("试译选区中存在未缓存的译文，请选择已经翻译的文本：" + id);
            Console.WriteLine("正在验证原文回填、中文回填及启动程序结构。");
            var verification = await new GeneratedAdapterVerifier().VerifyAsync(adapter, session.SourceDirectory,
                Path.Combine(workDirectory, "verification"), sourceIsSnapshot: true);
            if (!verification.Passed) throw new InvalidDataException(string.Join(Environment.NewLine, verification.Diagnostics));
            var result = await pipeline.RunAsync(session, new CacheOnlyProvider(), policy, new RunOptions(TrialUnitIds: ids));
            if (result.TranslationUsage.TotalTokens != 0 || result.CacheHits != ids.Length)
                throw new InvalidDataException("修复必须完全复用已有译文。");
            var store = new LocalAdapterPackageStore(Path.Combine(dataDirectory, "extensions"));
            var published = await store.PublishAndActivateAsync(project.Id, package, verification.Diagnostics);
            await new GameProjectStore(dataDirectory).SaveAsync(project with
            {
                AdapterId = published.Id, AdapterVersion = published.Version, UpdatedAt = DateTimeOffset.UtcNow
            });
            // Keep a resumed development session on the corrected source, not the previous font-only solution.
            var adaptation = Path.Combine(projectDirectory, "adaptation", "current");
            if (Directory.Exists(adaptation))
            {
                Directory.CreateDirectory(Path.Combine(adaptation, "src"));
                foreach (var file in Directory.EnumerateFiles(adapterSource, "*.cs"))
                    File.Copy(file, Path.Combine(adaptation, "src", Path.GetFileName(file)), true);
                var statePath = Path.Combine(adaptation, "session.json");
                var state = await LocalFiles.ReadJsonAsync<AdaptationSession>(statePath);
                if (state is not null)
                {
                    state.VerifiedPackage = published;
                    state.VerificationDiagnostics = verification.Diagnostics.ToArray();
                    state.Stage = "适配完成";
                    state.LastStatus = AdaptationStatus.Activated;
                    state.NextAction = "安装试译补丁并使用 .zh.exe 检查游戏内显示。";
                    state.Blocker = null;
                    state.Summary = "已修复中文码位识别并采用宋体补字。安装后使用 .zh.exe 启动，游戏内显示仍待确认。";
                    const string runtimeNote = "当前源码采用 System4 AIN v14 MSG1 + FNL；保留 CP932，以 F0–F9 私用码位补全缺字。原运行程序拒绝这些码位，必须同时使用独立 .zh.exe：按完整 .text SHA256 和 PE 布局匹配运行时，修改文字遍历和 FNL 字形索引的 96 处范围检查。与游戏名无关，代码节不同的版本需重新分析。新增字形采用经过 GetTextFace 核实的 SimSun；不再使用 Noto Sans。原文回填、中文回填和 PE 检查通过，试译复用已有缓存；真实游戏内显示尚待确认。STR0、EX、菜单和图片文字仍未覆盖。旧说明中“无需修改 EXE”和“Noto Sans”已失效，后续以当前源码和此记录为准。";
                    state.Checkpoint = runtimeNote;
                    Directory.CreateDirectory(Path.Combine(adaptation, "work"));
                    await File.WriteAllTextAsync(Path.Combine(adaptation, "work", "status.md"), runtimeNote);
                    foreach (var resource in session.Sources)
                    {
                        var destination = LocalFiles.UnderRoot(Path.Combine(adaptation, "resources"), resource.RelativePath);
                        Directory.CreateDirectory(Path.GetDirectoryName(destination)!);
                        File.Copy(LocalFiles.UnderRoot(session.SourceDirectory, resource.RelativePath), destination, true);
                        state.ResourceHashes[resource.RelativePath] = resource.Sha256;
                    }
                    state.Messages.Add(new AgentMessage("user", runtimeNote));
                    await LocalFiles.WriteJsonAsync(statePath, state);
                }
            }
            if (install)
            {
                await new PatchInstaller(projectDirectory).InstallAsync(project.GameDirectory, result.PatchDirectory);
                Console.WriteLine("修复补丁已安装。请启动：" + Path.Combine(project.GameDirectory, result.LaunchExecutable!));
            }
            else Console.WriteLine("修复补丁已生成：" + result.PatchDirectory);
            Console.WriteLine($"复用 {result.CacheHits} 条译文，API 调用为 0；原 EXE 保留。游戏内显示尚待确认。");
            return 0;
        }
        catch (Exception error)
        {
            Console.Error.WriteLine(error);
            return 1;
        }
    }

    private sealed class CacheOnlyProvider : ITranslationProvider
    {
        public string Identity => "local-cache-only";
        public Task<TranslationResponse> TranslateAsync(TranslationRequest request, CancellationToken cancellationToken = default)
            => throw new InvalidOperationException("缺少已缓存的译文，本工具不会调用翻译 API。");
    }
}
