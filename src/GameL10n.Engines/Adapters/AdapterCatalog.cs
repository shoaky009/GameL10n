using System.Text.RegularExpressions;

namespace GameL10n.Core.Adapters;

/// <summary>An engine or resource template, independent of the game that first supplied samples.</summary>
public sealed partial record AdapterFamily(string Kind, string Key, string Name)
{
    public string AdapterId => Kind + "." + Key;

    public void Validate()
    {
        if (Kind is not ("engine" or "template") || string.IsNullOrWhiteSpace(Key) || !KeyPattern().IsMatch(Key)
            || string.IsNullOrWhiteSpace(Name) || Name.Length > 160 || Name.Any(char.IsControl))
            throw new InvalidDataException("适配器必须声明引擎或资源模板的分类、标识与名称，不能使用游戏名称作为适配器名称。");
    }

    [GeneratedRegex("^[a-z0-9][a-z0-9.-]{0,79}$")]
    private static partial Regex KeyPattern();
}

public sealed record GameCompatibility(string GameId, string Title, string Variant, string AdapterVersion,
    IReadOnlyDictionary<string, string> SourceHashes, string StructureStatus, string RuntimeStatus = "not-tested",
    string? PatchDirectory = null);

public sealed record AdapterCatalogEntry(AdapterFamily Family, string Version, IReadOnlyList<GameCompatibility> Games)
{
    public bool Matches(string query) => string.IsNullOrWhiteSpace(query) ||
        new[] { Family.AdapterId, Family.Name }.Concat(Games.SelectMany(game => new[] { game.Title, game.Variant }))
            .Any(value => value.Contains(query.Trim(), StringComparison.OrdinalIgnoreCase));
}
