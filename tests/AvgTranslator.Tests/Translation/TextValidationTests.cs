using AvgTranslator.Core;
using AvgTranslator.Core.Translation;

namespace AvgTranslator.Tests.Translation;

public sealed class TextValidationTests
{
    [Theory]
    [InlineData("Deal (xx0x1) damage <Armor>", "造成(xx0x1)伤害 <Armor>")]
    [InlineData("Gain (#x0) and (#-x0); (#x0x1)", "获得(#x0)与(#-x0)；(#x0x1)")]
    [InlineData("(*)<i>(+x0%x1) Damage <g>((+x0%) currently)</g></i>", "(*)<i>(+x0%x1)伤害 <g>（当前(+x0%)）</g></i>")]
    [InlineData("Bonus (while active)", "加成（生效期间）")]
    public void PreservesDynamicNumericTemplatesWhileAllowingParenthesizedProse(string source, string translation) =>
        Assert.Empty(TextValidation.Validate(Unit(source), translation));

    [Theory]
    [InlineData("Deal (xx0x1) damage", "造成（xx0x1）伤害")]
    [InlineData("(+x0%x1) Damage", "(+x0%)伤害")]
    [InlineData("(#x0%) increased", "(x0%)提高")]
    [InlineData("(*) first (*) second", "(*) 第一条 第二条")]
    public void RejectsChangedDynamicMarkers(string source, string translation) =>
        Assert.Contains(TextValidation.Validate(Unit(source), translation), issue => issue.Code == "placeholder-mismatch");

    [Fact]
    public void SendsWholeDynamicTemplatesAsProtectedMarkers() =>
        Assert.Equal(new[] { "(*)", "(+x0%x1)", "(+x0%)", "(xx0x1)" },
            TextValidation.ProtectedMarkers("(*) (+x0%x1) Damage ((+x0%) currently); (xx0x1)"));

    [Theory]
    [InlineData("Would you like to stay\nfor the night?\n<color=grey>[Will reroll tavern visitors.]</color>",
        "您想在这里\n住一晚吗？\n<color=grey>[将重新随机生成酒馆访客。]</color>")]
    [InlineData("Would you like to stay\nfor the night? (5 Gold<sprite name=\"Gold\">)\n<color=grey>[Will reroll tavern visitors.]</color>",
        "您想在这里\n住一晚吗？（5金币<sprite name=\"Gold\">）\n<color=grey>[将重新随机生成酒馆访客。]</color>")]
    [InlineData("*You gained [[50% reduced]] chance of\ncurse<sprite name=\"Curse\"> taking effect.*",
        "*诅咒<sprite name=\"Curse\">生效的几率\n[[降低50%]]。*")]
    [InlineData("[[+25% increased damage with non-crits]]", "[[非暴击伤害提高25%]]")]
    [InlineData("[[+10% movement and attack speed]]", "[[移动速度和攻击速度增加10%]]")]
    [InlineData("[Adds +1 Evil<sprite name=\"Evil\">]", "[增加1点邪恶<sprite name=\"Evil\">]")]
    [InlineData("[name], [Will reroll visitors.]", "[将重新随机生成访客。]，[name]")]
    [InlineData("50% damage; 25% increased speed", "50%伤害；速度增加25%")]
    [InlineData("Value % d; %1$s", "数值% d；%1$s")]
    [InlineData("Instead of ((losing)) everything, [[Half]] remains.", "不会((全部失去))，而是留下[[一半]]。")]
    [InlineData("(*)<i>You can wear an Offhand with Two-Handed Weapons.</i>\n(*)<i>Two-Handed Weapons [[also]] count as One-Handed.</i>",
        "(*)<i>使用双手武器时可以装备副手物品。</i>\n(*)<i>双手武器[[也]]视为单手武器。</i>")]
    [InlineData("You have [[Infinite]] Gold<sprite name=\"Gold\">.\n<color=grey>(Assumes 1000 Gold<sprite name=\"Gold\"> for the purpose of Effects that scale with Gold<sprite name=\"Gold\">.)</color>",
        "你拥有[[无限]]金币<sprite name=\"Gold\">。\n<color=grey>（对于随金币<sprite name=\"Gold\">数量变化的效果，视为拥有1000金币<sprite name=\"Gold\">。）</color>")]
    [InlineData("While on full Stamina<Stamina>, your Hitbox is [[smaller]].", "耐力<Stamina>全满时，你的受击范围[[更小]]。")]
    [InlineData("[[Half]] your Strength also applies to Dexterity Scaling from Ranged Weapons.", "你力量的[[一半]]也计入远程武器的敏捷加成。")]
    [InlineData("Inflicts [[permanent]] Charm to an Enemy.", "对一名敌人施加[[永久]]魅惑。")]
    [InlineData("When Stamina<Stamina> is drained, instead of ((emptying)) Stamina Recovery<Stamina> progress, the Stamina Recovery<Stamina> progress is [[halved]].",
        "耐力<Stamina>耗尽时，不会((清空))耐力恢复<Stamina>进度，而是将耐力恢复<Stamina>进度[[减半]]。")]
    public void AllowsTranslatedBracketProseAndPercentages(string source, string translated) =>
        Assert.Empty(TextValidation.Validate(Unit(source), translated));

    [Theory]
    [InlineData("Hello {name}", "你好")]
    [InlineData("[name] [name]", "[name]")]
    [InlineData("[wait time=500]Hello", "[wait time=100]你好")]
    [InlineData("[wait 500]Hello", "[等待 500]你好")]
    [InlineData("[player.name!s]", "[玩家姓名]")]
    [InlineData("[points + bonus]", "[点数与加成]")]
    [InlineData("[first if flag else last]", "[第一个]")]
    [InlineData("[not unlocked]", "[尚未解锁]")]
    [InlineData("[flag and other]", "[同时成立]")]
    [InlineData("[日本語変数]", "[中文变量]")]
    [InlineData("[name] [Will reroll visitors.]", "[名字] [将重新生成访客。]")]
    [InlineData("[[50% reduced]]", "[降低50%]")]
    [InlineData("[[50% reduced]]", "[降低][50%]")]
    [InlineData("<color=grey>[Will reroll visitors.]</color>", "[将重新生成访客。]")]
    [InlineData("<sprite name=\"Gold\">", "<sprite name=\"金币\">")]
    [InlineData("%s: %02d", "%s：%d")]
    [InlineData(@"one\ntwo", "一二")]
    [InlineData("hello", "你好 {name}")]
    [InlineData("[[player.health]]", "[[玩家生命]]")]
    [InlineData("[[points + bonus]]", "[[点数加成]]")]
    [InlineData("((losing)) [[player.health]]", "((失去)) [[玩家生命]]")]
    [InlineData("[[+1]] heart", "[[+2]]颗心")]
    [InlineData("Instead of ((emptying)), [[halved]].", "不会((清空))，而是[减半]。")]
    [InlineData("Weapons [[also]] count as One-Handed.", "武器[also]（也）视为单手武器。")]
    [InlineData("You have [[Infinite]] Gold.", "你拥有[Infinite]（无限）金币。")]
    [InlineData("Weapons [[also]] count as One-Handed.", "武器[也]视为单手武器。")]
    [InlineData("[[player.health]] and [[smaller]]", "[player.health] 和 [[更小]]")]
    [InlineData("Stamina<Stamina>, Stamina Recovery<Stamina>, Stamina Recovery<Stamina> is [[halved]].",
        "耐力<Stamina>，耐力恢复<Stamina>进度[[减半]]。")]
    public void StillRejectsChangedControlMarkers(string source, string translated) =>
        Assert.Contains(TextValidation.Validate(Unit(source), translated), issue => issue.Code == "placeholder-mismatch");

    [Fact]
    public void ExplainsMissingMarkerAndItsRepeatedCount()
    {
        var issue = Assert.Single(TextValidation.Validate(Unit("{name} {name}"), "{name}"));
        Assert.Contains("缺少或减少了“{name}”", issue.Message);
        Assert.Contains("原文 2 次，译文 1 次", issue.Message);
    }

    [Fact]
    public void SeparatesCompleteBracketExpressionsFromVisibleEmphasis()
    {
        const string source = "[name] [[player.health]] [[+1]] [[also]] [[Infinite]] [[smaller]] [[permanent]] [[10% faster]]";
        Assert.Equal(new[] { "[name]", "[[player.health]]", "[[+1]]" }, TextValidation.ProtectedMarkers(source));
        Assert.Equal(new[] { "[[also]]", "[[Infinite]]", "[[smaller]]", "[[permanent]]", "[[10% faster]]" },
            TextValidation.TranslatableBracketGroups(source));
    }

    [Fact]
    public void ExplainsWhichBracketLayerWasLost()
    {
        var issue = Assert.Single(TextValidation.Validate(Unit("You have [[Infinite]] Gold."), "你拥有[无限]金币。"));
        Assert.Contains("原文 “[[” 1 次", issue.Message);
        Assert.Contains("译文 “[” 1 次", issue.Message);
    }

    private static TranslationUnit Unit(string text) => new("line", "dialogue.json", "line", text, "scene");
}
