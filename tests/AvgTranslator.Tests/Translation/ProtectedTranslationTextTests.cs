using AvgTranslator.Core;
using AvgTranslator.Core.Translation;

namespace AvgTranslator.Tests.Translation;

public sealed class ProtectedTranslationTextTests
{
    [Fact]
    public void AdapterMarkersAreMaskedAndValidatedOnlyForTheDeclaringUnit()
    {
        const string source = "Every (rate) seconds: (actionName)(br)(rate)";
        var unit = new TranslationUnit("1", "resources.assets", "field", source, "")
            { ProtectedTokens = ["(rate)", "(actionName)", "(br)"] };
        var plan = new ProtectedTranslationText(source, "{v", 0, unit.ProtectedTokens);
        Assert.Equal("Every {v0_0} seconds: {v0_1}{v0_2}{v0_0}", plan.Text);
        var translated = plan.Restore("每{v0_0}秒：{v0_1}{v0_2}{v0_0}");
        Assert.Empty(TextValidation.Validate(unit, translated));
        Assert.Contains(TextValidation.Validate(unit, "每秒触发"), i => i.Code == "placeholder-mismatch");
        Assert.Empty(TextValidation.Validate(unit with { ProtectedTokens = null }, "每秒触发"));
    }

    [Fact]
    public void DeclaredMarkerContainingCommonSyntaxIsProtectedOnce()
    {
        const string source = "Apply (rate:<color=red>{0}</color>)";
        var plan = new ProtectedTranslationText(source, "{v", 0, ["(rate:<color=red>{0}</color>)"]);
        Assert.Equal("Apply {v0_0}", plan.Text);
        Assert.Equal(source, plan.Restore(plan.Text));
    }

    [Fact]
    public void MarkerDeclarationsInvalidateOnlyAffectedMemoryAndEdits()
    {
        var unit = new TranslationUnit("1", "file", "field", "Every (rate) seconds", "context");
        var memory = new TranslationMemory("unused-memory");
        var policy = new TranslationPolicy("zh-CN", "natural", new Dictionary<string, string>());
        var oldKey = AvgTranslator.Core.Storage.LocalFiles.HashText(System.Text.Json.JsonSerializer.Serialize(new {
            unit.SourceText, unit.Context, unit.Speaker, unit.Kind,
            policy.TargetLanguage, policy.Style, policy.Version, Terms = Array.Empty<string[]>()
        }));
        Assert.Equal(oldKey, memory.Key(unit, policy));
        Assert.Equal(oldKey, memory.Key(unit with { ProtectedTokens = [] }, policy));
        var protectedUnit = unit with { ProtectedTokens = ["(rate)"] };
        Assert.NotEqual(oldKey, memory.Key(protectedUnit, policy));
        Assert.NotEqual(TranslationEdits.Signature(unit), TranslationEdits.Signature(protectedUnit));
        Assert.Equal(memory.Key(protectedUnit, policy), memory.Key(unit with { ProtectedTokens = ["(rate)", "(rate)"] }, policy));
    }

    [Theory]
    [InlineData("Gain (+x0%) Stats from Dexterity and")]
    [InlineData("(*)<i>(+x0%x1) Damage <g>((+x0%) currently)</g></i>")]
    [InlineData("[player.name!s] says [<color=red>Visible prose</color>] [[Infinite]] {value}")]
    [InlineData("[command \"<tag>\"] %02d %% 25% increased speed\\n")]
    public void RoundtripPreservesNestedControlSyntax(string source)
    {
        var plan = new ProtectedTranslationText(source, source.Contains("{v", StringComparison.Ordinal) ? "{vv" : "{v", 0);
        Assert.Equal(source, plan.Restore(plan.Text));
        Assert.Empty(TextValidation.Validate(new("id", "file", "line", source, ""), plan.Restore(plan.Text)));
    }

    [Fact]
    public void RestoresNumbersAndIconsWithoutModelKnowingTheirSyntax()
    {
        const string source = "You gain (x0) Luck<sprite name=\"Luck\"> at Level [[1]],";
        var plan = new ProtectedTranslationText(source, "{v", 0);
        Assert.Equal("You gain {v0_0} Luck{v0_1} at Level {v0_2},", plan.Text);
        var translation = plan.Restore("获得{v0_0}幸运{v0_1}的等级为{v0_2}、");
        Assert.Equal("获得(x0)幸运<sprite name=\"Luck\">的等级为[[1]]、", translation);
        Assert.Empty(TextValidation.Validate(new("id", "file", "line", source, ""), translation));
    }

    [Theory]
    [InlineData("从敏捷和")]
    [InlineData("获得{v1_0}属性，来源为敏捷和")]
    [InlineData("获得{v0_0}{v0_0}属性，来源为敏捷和")]
    public void DoesNotInventMissingValuesOrAcceptOtherUnitsTokens(string translation)
    {
        const string source = "Gain (+x0%) Stats from Dexterity and";
        var plan = new ProtectedTranslationText(source, "{v", 0);
        Assert.Contains(TextValidation.Validate(new("id", "file", "line", source, ""), plan.Restore(translation)),
            issue => issue.Code == "placeholder-mismatch");
    }
}
