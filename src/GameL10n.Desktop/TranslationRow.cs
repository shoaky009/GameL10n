using System.ComponentModel;
using GameL10n.Core;
using GameL10n.Core.Translation;

namespace GameL10n.Desktop;

internal sealed class TranslationRow(TranslationUnit unit, int number, IGameAdapter adapter) : INotifyPropertyChanged
{
    public TranslationUnit Unit { get; } = unit;
    public int Number { get; } = number;
    public string Signature { get; } = TranslationEdits.Signature(unit);
    public string? ModelText { get; set; }
    public string? TranslationFailure { get; set; }
    public string? Failure => Text is null ? TranslationFailure : null;
    public TranslationEdit? Edit { get; set; }
    public TranslationEdit? PreviousEdit { get; set; }
    public bool Selected { get; set; }
    public string? TextHash { get; private set; }
    public string? Text => Edit?.Text ?? ModelText;
    public bool Reviewed => Edit?.Reviewed == true && Issues.Count == 0;
    public IReadOnlyList<ValidationIssue> Issues { get; private set; } = [];
    public string Source => Unit.SourceText;
    public string Preview => Text ?? "尚未翻译";
    public string Location => $"{Number:N0} · {Unit.ResourcePath} · {Unit.Locator}";
    public string Kind => Unit.Kind switch
    {
        "name" => "角色名", "choice" => "选项", "dialogue" => "对白 / 旁白",
        "ui" => "菜单 / 界面", "item" => "物品 / 装备", "skill" => "技能 / 天赋", "system" => "提示 / 说明", _ => Unit.Kind
    };
    public string Status => Failure is not null ? "翻译失败" : Issues.Count > 0 ? "格式问题" : Reviewed ? "已校对"
        : Edit is { IsGenerated: false } ? "已编辑" : Text is null ? "未翻译" : "待校对";
    public string Metadata => $"{Kind}{(Unit.Speaker is { Length: > 0 } speaker ? " · " + speaker : "")} · {Status}";
    public void Refresh()
    {
        TextHash = Text is null ? null : GameL10n.Core.Storage.LocalFiles.HashText(Text);
        Issues = Text is null ? [] : TextValidation.Validate(Unit, Text).Concat(adapter.ValidateTranslation(Unit, Text)).ToArray();
        foreach (var name in new[] { nameof(Preview), nameof(Status), nameof(Metadata), nameof(Selected) })
            PropertyChanged?.Invoke(this, new(name));
    }
    public event PropertyChangedEventHandler? PropertyChanged;
}
