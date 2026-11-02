using AvgTranslator.Core;
using AvgTranslator.Core.Development;
using AvgTranslator.Core.Translation;

namespace AvgTranslator.Tests.Development;

public sealed class VerificationTranslationTextTests
{
    [Theory]
    [InlineData("【？？？】", "name")]
    [InlineData("[font face=serif]【Alice】[resetfont]", "name")]
    [InlineData("<color=red>Hello {player}</color>", "dialogue")]
    [InlineData("[[Danger ahead]] %s", "dialogue")]
    public void SamplesPreserveFormattingAndChangeVisibleText(string source, string kind)
    {
        var unit = new TranslationUnit("1", "script", "line:1", source, "", Kind: kind)
            { ProtectedTokens = ["【", "】", "[font face=serif]", "[resetfont]"] };
        var translated = VerificationTranslationText.Create(unit, 19415);
        Assert.NotEqual(source, translated);
        Assert.Empty(TextValidation.Validate(unit, translated));
        Assert.NotEqual(translated, VerificationTranslationText.Create(unit, 19416));
        Assert.NotEqual(translated, VerificationTranslationText.Create(unit, 19415, trial: true));
        if (kind == "name")
        {
            Assert.Equal(source.IndexOf('【'), translated.IndexOf('【'));
            Assert.True(translated.IndexOf('】') - translated.IndexOf('【') < 12);
        }
        else if (source.StartsWith('<')) Assert.StartsWith("<color=red>", translated);
    }

    [Fact]
    public void ProtectedOnlyTextIsNotPretendedToBeTranslated()
    {
        var unit = new TranslationUnit("1", "script", "line:1", "[wait]{player}<br>", "");
        Assert.Equal(unit.SourceText, VerificationTranslationText.Create(unit, 0));
        Assert.Equal(unit.SourceText, VerificationTranslationText.Create(unit, 0, trial: true));
    }
}
