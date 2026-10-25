using System.Text.Json;
using System.Text.Encodings.Web;
using AvgTranslator.Core.Storage;

namespace AvgTranslator.Core.Development;

// Samples come from an independent resource/screenshot investigation, not from re-reading
// the adapter's own output. Roundtrip consistency cannot detect a shared reader/writer offset bug.
public sealed record TextCoverageSample(string ResourcePath, string Locator, string SourceText);

public sealed record TextCoverageSampleCorrection(TextCoverageSample Original, TextCoverageSample Replacement,
    string Reason, string Evidence)
{
    public void Validate()
    {
        if (Original is null || Replacement is null || string.IsNullOrWhiteSpace(Reason) || Reason.Length > 600
            || string.IsNullOrWhiteSpace(Evidence) || Evidence.Length > 2000)
            throw new InvalidDataException("样本纠正需要明确的原样本、替代样本、原因和独立资源证据。");
        new TextCoverage([Original], []).Validate();
        new TextCoverage([Replacement], []).Validate();
    }
}

public sealed record TextCoverage(IReadOnlyList<TextCoverageSample> Samples, IReadOnlyList<string> Pending)
{
    public IReadOnlyList<string> RuntimeChecks { get; init; } = [];
    public IReadOnlyList<TextCoverageSampleCorrection> Corrections { get; init; } = [];
    private static readonly JsonSerializerOptions ContextJson = new(JsonSerializerDefaults.Web)
        { Encoder = JavaScriptEncoder.UnsafeRelaxedJsonEscaping };
    public static TextCoverage Empty { get; } = new([], []);
    public string Fingerprint() => LocalFiles.HashText(JsonSerializer.Serialize(Samples));

    public void Validate()
    {
        if (Samples is null || Pending is null || RuntimeChecks is null || Corrections is null || Corrections.Count > 8
            || Samples.Count > 64 || Pending.Count > 24 || RuntimeChecks.Count > 24
            || Samples.Any(s => s is null || string.IsNullOrWhiteSpace(s.ResourcePath) || s.ResourcePath.Length > 512
                || string.IsNullOrWhiteSpace(s.Locator) || s.Locator.Length > 512
                || string.IsNullOrWhiteSpace(s.SourceText) || s.SourceText.Length > 2000)
            || Pending.Any(s => string.IsNullOrWhiteSpace(s) || s.Length > 600)
            || RuntimeChecks.Any(s => string.IsNullOrWhiteSpace(s) || s.Length > 600)
            || Samples.Sum(s => s.ResourcePath.Length + s.Locator.Length + s.SourceText.Length) > 16000
            || Pending.Sum(s => s.Length) > 6000
            || RuntimeChecks.Sum(s => s.Length) > 6000
            || Samples.Select(s => (Normalize(s.ResourcePath), s.Locator)).Distinct().Count() != Samples.Count)
            throw new InvalidDataException("文字覆盖记录无效：最多 64 个原文定位样本、24 项已知遗漏和 24 项运行验证，样本位置不能重复。");
        foreach (var correction in Corrections)
            (correction ?? throw new InvalidDataException("样本纠正记录不能为空。")).Validate();
    }

    public TextCoverage CorrectSample(TextCoverageSampleCorrection correction)
    {
        Validate();
        correction.Validate();
        static bool SameLocation(TextCoverageSample left, TextCoverageSample right) =>
            Normalize(left.ResourcePath) == Normalize(right.ResourcePath) && left.Locator == right.Locator;
        if (Corrections.Contains(correction) && Samples.Any(sample => SameLocation(sample, correction.Replacement)
                && sample.SourceText == correction.Replacement.SourceText)) return this;
        var original = Samples.FirstOrDefault(sample => SameLocation(sample, correction.Original));
        if (original is null || original.SourceText != correction.Original.SourceText)
            throw new InvalidDataException("待纠正样本已变化或不存在，未修改验收记录。请核对当前记录。");
        var target = Samples.FirstOrDefault(sample => SameLocation(sample, correction.Replacement));
        if (target is not null && target != original && target.SourceText != correction.Replacement.SourceText)
            throw new InvalidDataException("替代位置已有不同的样本，未覆盖其他验收记录。");
        var samples = Samples.Where(sample => sample != original).ToList();
        if (target is null || target == original) samples.Add(correction.Replacement);
        var updated = this with { Samples = samples, Corrections = Corrections.TakeLast(7).Append(correction).ToArray() };
        updated.Validate();
        return updated;
    }

    public IReadOnlyList<string> Check(ExtractionResult extraction)
    {
        Validate();
        if (Samples.Count == 0) return [];
        var locations = extraction.Units.ToLookup(u => (Normalize(u.ResourcePath), u.Locator));
        return Samples.Where(s => !locations[(Normalize(s.ResourcePath), s.Locator)].Any(u => u.SourceText == s.SourceText))
            .Select(s => "提取结果与保留样本不符：" + s.ResourcePath + "（" + s.Locator
                + "）。核对实际资源、位置与原文；可能是漏提，也可能是样本标注错误。只有独立证据确认误标时，才能用 coverageSampleCorrections 更正并重新完整验收。")
            .ToArray();
    }

    public string Context() => Samples.Count == 0 && Pending.Count == 0 && RuntimeChecks.Count == 0 ? ""
        : "\n独立保留的验收记录（checkpoint 不会清除）：pending 是已知漏提文字；runtimeChecks 是尚未完成的启动或显示验证，不能据此认定漏提文字。"
            + "旧 pending 不会自动删除，请按已有证据显式分类；已知遗漏必须披露，具体处理范围遵循当前任务目标。\n"
            + "样本必须来自实际资源的独立定位，不能把游戏名称或说明书标题猜作脚本文本。确认样本误标后，提供原样本、真实替代样本、原因与证据进行 coverageSampleCorrections；不能仅因适配器未命中而纠正。\n"
            + JsonSerializer.Serialize(this, ContextJson);

    private static string Normalize(string value) => value.Replace('\\', '/');
}
