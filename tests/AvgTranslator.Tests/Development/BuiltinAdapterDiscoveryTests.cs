using System.Text;
using AvgTranslator.Core.Development;

namespace AvgTranslator.Tests.Development;

public sealed class BuiltinAdapterDiscoveryTests : IDisposable
{
    private readonly string root = Path.Combine(Path.GetTempPath(), "avg-builtin-discovery-" + Guid.NewGuid().ToString("N"));

    [Fact]
    public async Task NekoSignaturePrioritizesScriptsAndPointsToReferenceToolsWithoutClaimingAWorkingAdapter()
    {
        Write("scr.pak", "NEKOPACK4A"u8.ToArray());
        Write("cfg.pak", "NEKOPACK4A"u8.ToArray());
        for (int index = 0; index < 130; index++)
            Write($"bg{index:D3}.pak", "NEKOPACK4A"u8.ToArray());
        var report = await BuiltinAdapterDiscovery.InspectAsync(root);
        var candidate = Assert.Single(report.Candidates);
        Assert.Equal("NekoSDK", candidate.Format);
        Assert.Equal("reference.nekosdk", candidate.AdapterId);
        Assert.True(report.Partial);
        Assert.Equal(4, candidate.SampleFiles.Count);
        Assert.Equal(new[] { "scr.pak", "cfg.pak" }, candidate.SampleFiles.Take(2));
        Assert.Contains(candidate.Evidence, value => value.Contains("inspect_engine_reference"));
        Assert.NotEmpty(candidate.MissingCapabilities);
    }

    [Theory]
    [InlineData("BSScript Rev.6\0", true)]
    [InlineData("BSScript Rev.60\0", false)]
    [InlineData("unrecognized", false)]
    public async Task BsscriptSignatureIdentifiesTheVersionWithoutClaimingParserSupport(string header, bool matched)
    {
        Write("renamed.dat", Encoding.ASCII.GetBytes(header));
        var report = await BuiltinAdapterDiscovery.InspectAsync(root);
        if (!matched) { Assert.Empty(report.Candidates); return; }
        var candidate = Assert.Single(report.Candidates);
        Assert.Equal("BSScriptRev6", candidate.Format);
        Assert.Equal("reference.bsscript-rev6", candidate.AdapterId);
        Assert.Contains(candidate.Evidence, value => value.Contains("inspect_pe_imports"));
        Assert.NotEmpty(candidate.MissingCapabilities);
    }

    [Theory]
    [InlineData("1.0")]
    [InlineData("2.0")]
    [InlineData("3.0")]
    [InlineData("3.1")]
    public async Task ContainerSignaturesProduceSpecificCandidatesWithUnresolvedCapabilities(string version)
    {
        Write("data.xp3", [0x58, 0x50, 0x33, 0x0D, 0x0A, 0x20, 0x0A, 0x1A, 0x8B, 0x67, 0x01]);
        Write("game/archive.rpa", Encoding.ASCII.GetBytes("RPA-3.0 0000000000000010 12345678\n"));
        var qlie = new byte[200];
        Encoding.ASCII.GetBytes("FilePackVer" + version).CopyTo(qlie, qlie.Length - 28);
        Write("data0.pack", qlie);

        var report = await BuiltinAdapterDiscovery.InspectAsync(root);

        Assert.Equal(BuiltinAdapterDiscovery.Revision, report.Revision);
        Assert.Equal(3, report.FilesScanned);
        Assert.False(report.Partial);
        Assert.Equal(new[] { "KirikiriKs", "Qlie", "Renpy" }, report.Candidates.Select(candidate => candidate.Format).Order());
        Assert.All(report.Candidates, candidate =>
        {
            Assert.Equal("container-signature", candidate.EvidenceKind);
            Assert.Contains(candidate.MissingCapabilities, missing => missing.Contains("解包／回包"));
            Assert.Contains(candidate.MissingCapabilities, missing => missing.Contains("待验证"));
            Assert.StartsWith("engine.vntextpatch-", candidate.AdapterId);
        });
        Assert.Contains(report.Warnings, warning => warning.Contains("尚未运行解析器"));
        Assert.Equal(qlie, File.ReadAllBytes(Path.Combine(root, "data0.pack")));
    }

    [Fact]
    public async Task GenericSuffixesStayUnclassifiedUntilARecognizedSignatureExists()
    {
        foreach (var name in new[] { "notes.txt", "message.dat", "data.bin", "resources.pack", "archive.arc" })
            Write(name, Encoding.UTF8.GetBytes("普通文字和未知资源，不能据此判定游戏引擎。"));
        Assert.Empty((await BuiltinAdapterDiscovery.InspectAsync(root)).Candidates);

        Write("message.dat", Encoding.ASCII.GetBytes("[SCR-MESSAGE]ver4.0"));
        Write("data.bin", Encoding.ASCII.GetBytes("SYS4\0\0\0\0"));
        var report = await BuiltinAdapterDiscovery.InspectAsync(root);

        Assert.Equal(new[] { "Age", "Kaguya" }, report.Candidates.Select(candidate => candidate.Format).Order());
        Assert.All(report.Candidates, candidate => Assert.Equal("script-signature", candidate.EvidenceKind));
        // These deliberately incomplete headers cannot establish that a full parser will accept the files.
        Assert.All(report.Candidates, candidate => Assert.Contains(candidate.MissingCapabilities, missing => missing.Contains("实际解析")));
    }

    [Fact]
    public async Task SamplesAreLimitedAndBackupAndWorkspaceResourcesAreExcluded()
    {
        for (int i = 0; i < 5; i++) Write($"game/scene{i}.rpy", Encoding.UTF8.GetBytes("label start:\n    \"hello\"\n"));
        foreach (var path in new[] { ".avg-translator/old.ks", "backup/old.ks", "scene.ks.bak", "scene.bak.ks", "nested/backups/old.ks" })
            Write(path, Encoding.UTF8.GetBytes("*start\n日本語\n"));

        var report = await BuiltinAdapterDiscovery.InspectAsync(root);

        Assert.Equal(5, report.FilesScanned);
        var candidate = Assert.Single(report.Candidates);
        Assert.Equal("Renpy", candidate.Format);
        Assert.Equal("extension-hint", candidate.EvidenceKind);
        Assert.Equal(new[] { "game/scene0.rpy", "game/scene1.rpy", "game/scene2.rpy" }, candidate.SampleFiles);
        Assert.Equal(3, candidate.Evidence.Count);
    }

    [Fact]
    public async Task StrongEvidenceTakesPriorityAndCandidateCountIsBounded()
    {
        Write("one.dat", Encoding.ASCII.GetBytes("CatScene"));
        Write("two.dat", Encoding.ASCII.GetBytes("[SCR-MESSAGE]ver4.0"));
        Write("three.dat", Encoding.ASCII.GetBytes("BurikoCompiledScriptVer1.00\0"));
        Write("four.dat", Encoding.ASCII.GetBytes("TJS2100\0"));
        Write("five.dat", Encoding.ASCII.GetBytes("MajiroObjV1.000\0"));
        Write("six.dat", Encoding.ASCII.GetBytes("SHSysSC\0"));
        Write("seven.dat", Encoding.ASCII.GetBytes("YSTB\0\0\0\0"));
        Write("weak.rpy", Encoding.UTF8.GetBytes("not a script"));

        var report = await BuiltinAdapterDiscovery.InspectAsync(root);

        Assert.True(report.Partial);
        Assert.Equal(6, report.Candidates.Count);
        Assert.All(report.Candidates, candidate => Assert.Equal("script-signature", candidate.EvidenceKind));
        Assert.DoesNotContain(report.Candidates, candidate => candidate.Format == "Renpy");
        Assert.Contains(report.Warnings, warning => warning.Contains("超过 6 种"));
    }

    [Fact]
    public async Task ReadFailuresAndSamplingLimitProducePartialEvidenceInsteadOfBlocking()
    {
        for (int i = 0; i < 129; i++) Write($"scene{i:D3}.rpy", []);
        Write("locked.xp3", [1, 2, 3]);
        using var locked = new FileStream(Path.Combine(root, "locked.xp3"), FileMode.Open, FileAccess.ReadWrite, FileShare.None);

        var report = await BuiltinAdapterDiscovery.InspectAsync(root);

        Assert.True(report.Partial);
        Assert.Equal(130, report.FilesScanned);
        Assert.Contains(report.Warnings, warning => warning.Contains("无法读取资源特征 locked.xp3"));
        Assert.Contains(report.Warnings, warning => warning.Contains("128"));
        var kirikiri = Assert.Single(report.Candidates, candidate => candidate.Format == "KirikiriKs");
        Assert.Equal("container-extension-hint", kirikiri.EvidenceKind);
    }

    [Fact]
    public async Task MissingDirectoryReturnsWarningAndCancellationStillCancels()
    {
        var report = await BuiltinAdapterDiscovery.InspectAsync(root);
        Assert.True(report.Partial);
        Assert.Empty(report.Candidates);
        Assert.NotEmpty(report.Warnings);
        using var cancellation = new CancellationTokenSource();
        cancellation.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => BuiltinAdapterDiscovery.InspectAsync(root, cancellation.Token));
    }

    private void Write(string relativePath, byte[] data)
    {
        var path = Path.Combine(root, relativePath);
        Directory.CreateDirectory(Path.GetDirectoryName(path)!);
        File.WriteAllBytes(path, data);
    }

    public void Dispose()
    {
        if (Directory.Exists(root)) Directory.Delete(root, recursive: true);
    }
}
