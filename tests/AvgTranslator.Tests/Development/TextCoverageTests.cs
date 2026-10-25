using AvgTranslator.Core;
using AvgTranslator.Core.Development;
using System.Text.Json;

namespace AvgTranslator.Tests.Development;

public sealed class TextCoverageTests
{
    [Fact]
    public void ProvenCorrectionReplacesOnlyTheWrongAnchorAndPreservesOtherChecks()
    {
        var wrong = new TextCoverageSample("text.json", "items/0/title", "Manual title");
        var actual = new TextCoverageSample("text.json", "dialogues/80/text", "Original dialogue");
        var coverage = new TextCoverage([wrong, actual], ["Known menu omission."]) { RuntimeChecks = ["Check font in game."] };
        var correction = new TextCoverageSampleCorrection(wrong, actual, "Title was incorrectly inferred from the manual.",
            "items/0/title is an internal key; dialogues/80/text independently contains the display text.");
        var updated = coverage.CorrectSample(correction);
        Assert.Equal(actual, Assert.Single(updated.Samples));
        Assert.Single(updated.Corrections);
        Assert.Equal(coverage.Pending, updated.Pending);
        Assert.Equal(coverage.RuntimeChecks, updated.RuntimeChecks);
        Assert.NotEqual(coverage.Fingerprint(), updated.Fingerprint());
        Assert.Equal(updated, updated.CorrectSample(correction));
        Assert.Empty(updated.Check(new([new("dialogue", actual.ResourcePath, actual.Locator, actual.SourceText, "")], [])));
        var restored = JsonSerializer.Deserialize<TextCoverage>(JsonSerializer.Serialize(updated))!;
        Assert.Equal(correction, Assert.Single(restored.Corrections));
    }

    [Fact]
    public void CorrectionRejectsStaleAnchorsAndConflictingTargetsWithoutDiscardingChecks()
    {
        var original = new TextCoverageSample("text.json", "dialogues/0/text", "Original");
        var other = new TextCoverageSample("text.json", "dialogues/1/text", "Other");
        var coverage = new TextCoverage([original, other], []);
        var correction = new TextCoverageSampleCorrection(original, other, "Wrong location.", "Independent byte evidence.");
        Assert.Throws<InvalidDataException>(() => coverage.CorrectSample(correction with { Original = original with { SourceText = "Stale" } }));
        Assert.Throws<InvalidDataException>(() => coverage.CorrectSample(correction with { Replacement = other with { SourceText = "Different" } }));
        Assert.Throws<InvalidDataException>(() => coverage.CorrectSample(correction with { Evidence = "" }));
        Assert.Equal(2, coverage.Samples.Count);
        Assert.Empty(coverage.Corrections);
    }

    [Fact]
    public void RejectsSelfConsistentButMislabelledFields()
    {
        var coverage = new TextCoverage([new("data/resources.assets", "CollectionItem/description", "Progress beyond floor 1.")], []);
        var wrong = new ExtractionResult([new("1", "data/resources.assets", "CollectionItem/description", "Ranger Class", "")], []);
        Assert.Single(coverage.Check(wrong));
        var corrected = new ExtractionResult([wrong.Units[0] with { SourceText = "Progress beyond floor 1." }], []);
        Assert.Empty(coverage.Check(corrected));
    }

    [Fact]
    public void SameTextAtAnotherLocationDoesNotSatisfySample()
    {
        var coverage = new TextCoverage([new("data/resources.assets", "Item/title", "Knight")], ["Trace runtime labels."]);
        Assert.Single(coverage.Check(new([new("1", "data/resources.assets", "Class/title", "Knight", "")], [])));
        Assert.Contains("Trace runtime labels.", coverage.Context());
        Assert.Equal(coverage.Fingerprint(), (coverage with { Pending = [] }).Fingerprint());
    }

    [Fact]
    public void RuntimeChecksRemainSeparateFromKnownMissingCoverageAndDoNotChangeSampleVerification()
    {
        var legacy = JsonSerializer.Deserialize<TextCoverage>("""{"samples":[],"pending":["Known missing menu."]}""",
            new JsonSerializerOptions(JsonSerializerDefaults.Web))!;
        Assert.Empty(legacy.RuntimeChecks);
        var coverage = legacy with { RuntimeChecks = ["Confirm font selection in game."] };
        coverage.Validate();
        Assert.Equal(legacy.Fingerprint(), coverage.Fingerprint());
        Assert.Empty(coverage.Check(new([], [])));
        Assert.Equal("Known missing menu.", Assert.Single(coverage.Pending));
        Assert.Contains("Confirm font selection in game.", coverage.Context());
        Assert.Contains("不能据此认定漏提文字", coverage.Context());
    }
}
