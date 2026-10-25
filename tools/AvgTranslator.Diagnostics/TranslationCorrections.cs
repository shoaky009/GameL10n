using System.Text.Json;
using AvgTranslator.Core.Pipeline;
using AvgTranslator.Core.Storage;
using AvgTranslator.Core.Translation;

internal sealed record TranslationCorrection(string Id, string SourceText, string Context, string PreviousTranslation, string Translation);
internal sealed record TranslationCorrectionSet(string TargetLanguage, IReadOnlyList<TranslationCorrection> Corrections);

// Applies reviewed corrections only to the exact source and previous response. Never overwrites
// manual edits or later translations; it uses the same memory keys as the regular pipeline.
internal static class TranslationCorrections
{
    public static async Task<int> RunAsync(string projectDirectory, string correctionsPath, bool apply)
    {
        try
        {
            projectDirectory = Path.GetFullPath(projectDirectory);
            var correctionSet = await LocalFiles.ReadJsonAsync<TranslationCorrectionSet>(correctionsPath)
                ?? throw new InvalidDataException("缺少校正记录。");
            var corrections = correctionSet.Corrections;
            if (corrections.Count is < 1 or > 100 || corrections.Select(c => c.Id).Distinct().Count() != corrections.Count)
                throw new InvalidDataException("校正记录包含重复 ID 或超出单次限制。");
            await using var taskLock = apply ? new FileStream(Path.Combine(projectDirectory, ".task.lock"),
                FileMode.OpenOrCreate, FileAccess.ReadWrite, FileShare.None) : null;
            var project = await LocalFiles.ReadJsonAsync<GameProject>(Path.Combine(projectDirectory, "game.json"))
                ?? throw new InvalidDataException("项目不存在。");
            if (correctionSet.TargetLanguage != project.Settings.TargetLanguage)
                throw new InvalidDataException("校正记录的目标语言与项目设置不一致。");
            var snapshot = await LocalFiles.ReadJsonAsync<SourceSnapshot>(Path.Combine(projectDirectory, "source.json"))
                ?? throw new InvalidDataException("原文记录不存在。");
            var fingerprint = LocalFiles.HashText(JsonSerializer.Serialize(snapshot.Files.OrderBy(f => f.RelativePath)));
            var extraction = await new ExtractionStore(projectDirectory).LoadAsync(fingerprint, snapshot.AdapterId, snapshot.AdapterVersion)
                ?? throw new InvalidDataException("提取缓存不可用，未改动译文。");
            var units = extraction.Units.ToDictionary(u => u.Id);
            var terms = new Dictionary<string, string>();
            foreach (var line in project.Settings.Terminology.Split('\n', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries))
            {
                var pair = line.Split('=', 2, StringSplitOptions.TrimEntries);
                if (pair.Length != 2 || pair.Any(string.IsNullOrWhiteSpace)) throw new InvalidDataException("术语格式必须为原文=译文。");
                terms.Add(pair[0], pair[1]);
            }
            var policy = new TranslationPolicy(project.Settings.TargetLanguage, project.Settings.Style, terms);
            var directory = Path.Combine(projectDirectory, "memory");
            var memory = new TranslationMemory(directory);
            var edits = await TranslationEdits.LoadAsync(projectDirectory);
            var pending = new List<MemoryEntry>();
            foreach (var fix in corrections)
            {
                if (!units.TryGetValue(fix.Id, out var unit) || unit.SourceText != fix.SourceText || unit.Context != fix.Context)
                    throw new InvalidDataException("校正记录与当前原文或字段不一致：" + fix.Id);
                var issues = TextValidation.Validate(unit, fix.Translation);
                if (issues.Count > 0) throw new InvalidDataException(string.Join("；", issues.Select(i => i.Message)));
                var key = memory.Key(unit, policy);
                var shard = await LocalFiles.ReadJsonAsync<MemoryShard>(Path.Combine(directory, key[..2] + ".json"));
                var existing = shard?.Entries.GetValueOrDefault(key);
                if (edits.Find(unit, policy.TargetLanguage) is not null
                    || existing is not null && existing.Translation != fix.PreviousTranslation)
                    continue;
                pending.Add(new(key, unit.SourceText, unit.Context, fix.Translation, "reviewed-fragment-correction", DateTimeOffset.UtcNow));
            }
            if (apply) await memory.StoreBatchAsync(pending);
            Console.WriteLine($"已核对 {corrections.Count} 条原文和控制标记；{(apply ? "已校正" : "可校正")} {pending.Count} 条译文，其余保留现有修改。API 调用为 0。");
            if (!apply) Console.WriteLine("仅检查，项目未写入。");
            return 0;
        }
        catch (Exception error) { Console.Error.WriteLine(error); return 1; }
    }
}
