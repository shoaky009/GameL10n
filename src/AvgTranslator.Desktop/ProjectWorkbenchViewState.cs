namespace AvgTranslator.Desktop;

internal readonly record struct WorkbenchTextFilter(string Query, string? ResourcePath, string Kind, string Status, bool OnlySelected)
{
    public bool MatchesAll => Query.Length == 0 && ResourcePath is null && Kind == "all" && Status == "all" && !OnlySelected;
}

internal sealed class ProjectWorkbenchViewState
{
    public string SearchText { get; set; } = "";
    public string? ResourcePath { get; set; }
    public string Kind { get; set; } = "all";
    public string Status { get; set; } = "all";
    public bool OnlySelected { get; set; }
    public int Page { get; set; }
    public double ScrollOffset { get; set; }
    public bool RevealActiveText { get; set; } = true;
    public long ContentRevision { get; set; }
    public TranslationRow[]? Rows { get; set; }
    public string? Policy { get; set; }
    public string[]? ResourcePaths { get; set; }
    public TranslationRow[]? FilteredRows { get; set; }
    public WorkbenchTextFilter? FilteredOptions { get; set; }
    public long FilteredRevision { get; set; } = -1;
    public string? Summary { get; set; }
    public long SummaryRevision { get; set; } = -1;

    public WorkbenchTextFilter Filter => new(SearchText.Trim(), ResourcePath, Kind, Status, OnlySelected);

    public void ForgetRows()
    {
        Rows = null;
        Policy = null;
        ResourcePaths = null;
        FilteredRows = null;
        FilteredOptions = null;
        FilteredRevision = -1;
        Summary = null;
        SummaryRevision = -1;
    }
}
