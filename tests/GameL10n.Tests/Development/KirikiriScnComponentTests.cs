using System.Text.Json;
using GameL10n.Core.Development;

namespace GameL10n.Tests.Development;

public sealed class KirikiriScnComponentTests
{
    [Fact]
    public async Task SemanticComponentPreservesIdsAndUntouchedReferencesAndRecomputesDerivedValues()
    {
        using var fixture = new EngineArchiveFixture("KirikiriScnReference.zip");
        var release = new List<AdapterReleaseFile>();
        await new AdapterComponentAssets().CopyAsync(["kirikiri-scn-text"], fixture.Root, releaseFiles: release);
        Assert.Equal("tools/kirikiri_scn_text.py", Assert.Single(release).Path);
        var check = Path.Combine(fixture.Root, "check.py");
        using (var source = typeof(KirikiriScnComponentTests).Assembly.GetManifestResourceStream("KirikiriScnComponentCheck.py")!)
        await using (var target = File.Create(check)) await source.CopyToAsync(target);
        var component = Path.Combine(fixture.Root, "src", "tools", "kirikiri_scn_text.py");
        var run = await new AdapterProcessRunner().RunAsync(new(fixture.Paths.Python,
            EngineReferenceToolkit.PythonArguments(fixture.Paths, check, [component]), fixture.Root), default);
        Assert.True(run.ExitCode == 0, run.StandardOutput + run.StandardError);
        Assert.Equal("scn-component-ok", run.StandardOutput.Trim());
    }
}
