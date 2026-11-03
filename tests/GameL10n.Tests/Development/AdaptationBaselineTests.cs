using System.Text.Json;
using GameL10n.Core;
using GameL10n.Core.Development;

namespace GameL10n.Tests.Development;

public sealed class AdaptationBaselineTests
{
    private static readonly TranslationUnit Original = new("vntp:stable", "Data/message.dat", "string:1",
        "原文", "Kaguya · Data/message.dat");

    [Theory]
    [InlineData("context")]
    [InlineData("speaker")]
    [InlineData("kind")]
    [InlineData("protectedTokens")]
    public void RepairRejectsChangesThatInvalidateCommittedTranslationIdentity(string field)
    {
        var changed = field switch
        {
            "context" => Original with { Context = "dialogue 槽 0；引用 1 次" },
            "speaker" => Original with { Speaker = "name:1" },
            "kind" => Original with { Kind = "choice" },
            _ => Original with { ProtectedTokens = ["原文"] }
        };
        var error = Assert.Throws<GeneratedAdapterException>(() =>
            GeneratedAdapterVerifier.RequireCompatibleBaseline([Original], new([changed], [])));
        Assert.Contains(field, error.Message);
        Assert.Contains("inspect_existing_text", error.Message);
    }

    [Fact]
    public void RepairDoesNotMatchRepeatedTextByItsContentAfterIdsHaveChanged()
    {
        var second = Original with { Id = "vntp:second", Locator = "string:2" };
        var renamed = Original with { Id = "Data/message.dat|dialogue:0" };
        var error = Assert.Throws<GeneratedAdapterException>(() =>
            GeneratedAdapterVerifier.RequireCompatibleBaseline([Original, second], new([renamed, second], [])));
        Assert.Contains("1 / 2", error.Message);
        Assert.Contains(Original.Id, error.Message);
    }

    [Fact]
    public void RepairRetainsAlreadyImplementedAdditionalTextAndAcceptsReloadedBaseline()
    {
        var reloaded = JsonSerializer.Deserialize<TranslationUnit>(JsonSerializer.Serialize(Original))!;
        var extra = new TranslationUnit("menu:save", "Start.exe", "menu:1", "保存", "原生菜单", Kind: "ui");
        GeneratedAdapterVerifier.RequireCompatibleBaseline([reloaded], new([extra, Original with { ProtectedTokens = [] }], []));
    }
}
