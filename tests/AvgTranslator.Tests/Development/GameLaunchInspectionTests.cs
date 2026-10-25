using System.Text;
using System.Text.Json;
using AvgTranslator.Core.Development;

namespace AvgTranslator.Tests.Development;

public sealed class GameLaunchInspectionTests : IDisposable
{
    private readonly string root = Path.Combine(Path.GetTempPath(), "avg-launch-test-" + Guid.NewGuid().ToString("N"));
    public GameLaunchInspectionTests() => Directory.CreateDirectory(root);

    [Fact]
    public async Task JapaneseProfileDecodesErrorAndFindsTheMovieWithoutReadingItsContents()
    {
        await File.WriteAllTextAsync(Path.Combine(root, "original.exe"), "fixture");
        await File.WriteAllTextAsync(Path.Combine(root, "original.exe.le.config"),
            "<LEConfig><Profiles><Profile><Location>ja-JP</Location><RunAsAdmin>false</RunAsAdmin></Profile></Profiles></LEConfig>");
        Encoding.RegisterProvider(CodePagesEncodingProvider.Instance);
        const string error = "FileIO：指定されたファイルが見つかりません：ロゴ.ogv";
        await File.WriteAllBytesAsync(Path.Combine(root, "ERRORLOG.txt"), Encoding.GetEncoding(932).GetBytes(error));
        Directory.CreateDirectory(Path.Combine(root, "GameData", "Movie"));
        var movie = Path.Combine(root, "GameData", "Movie", "ロゴ.ogv");
        await File.WriteAllTextAsync(movie, "video bytes must not enter model context");
        await File.WriteAllTextAsync(Path.Combine(root, "start.cmd"), "original.exe");
        var filesBefore = Directory.GetFiles(root, "*", SearchOption.AllDirectories);

        var inspection = await GameLaunchInspector.InspectAsync(root);
        var profile = Assert.Single(inspection.LocaleProfiles);
        Assert.True(profile.ExecutableExists);
        Assert.Equal("ja-JP", profile.Location);
        Assert.False(profile.RunAsAdmin);
        Assert.Contains(error, Assert.Single(inspection.ErrorLogs).Content);
        Assert.Contains("original.exe", Assert.Single(inspection.LaunchScripts).Content);
        var found = JsonSerializer.SerializeToElement(GameLaunchInspector.FindFile(root, "ロゴ.ogv"));
        Assert.False(found.GetProperty("exactPathExists").GetBoolean());
        Assert.Equal("GameData/Movie/ロゴ.ogv", Assert.Single(found.GetProperty("matches").EnumerateArray()).GetString());
        Assert.DoesNotContain("video bytes", JsonSerializer.Serialize(inspection));
        Assert.Equal(filesBefore, Directory.GetFiles(root, "*", SearchOption.AllDirectories));
    }

    [Fact]
    public async Task MalformedProfileDoesNotHideOtherEvidenceAndLogSamplesAreBounded()
    {
        await File.WriteAllTextAsync(Path.Combine(root, "game.exe.le.config"), "<not-xml");
        await File.WriteAllTextAsync(Path.Combine(root, "error.log"), new string('x', 10_000) + "\nlatest-error");
        await File.WriteAllTextAsync(Path.Combine(root, "start.ps1"), "Write-Output '中文'", Encoding.Unicode);
        var inspection = await GameLaunchInspector.InspectAsync(root);
        Assert.NotNull(Assert.Single(inspection.LocaleProfiles).Error);
        var log = Assert.Single(inspection.ErrorLogs);
        Assert.True(log.Truncated);
        Assert.Contains("latest-error", log.Content);
        Assert.True(log.Content.Length <= 1200);
        Assert.Contains("中文", Assert.Single(inspection.LaunchScripts).Content);
        Assert.Throws<InvalidDataException>(() => GameLaunchInspector.FindFile(root, "../outside.ogv"));
    }

    public void Dispose() => Directory.Delete(root, true);
}
