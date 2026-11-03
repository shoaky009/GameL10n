using GameL10n.Core.Isolation;

namespace GameL10n.Tests.Isolation;

public sealed class WorkspaceFilesTests
{
    [Theory]
    [InlineData("../escape")]
    [InlineData("sub/../../escape")]
    [InlineData("C:/outside.txt")]
    [InlineData("file.txt:stream")]
    [InlineData("NUL.txt")]
    [InlineData("COM1")]
    [InlineData("sub/CON")]
    [InlineData("file.txt.")]
    [InlineData("file.txt ")]
    [InlineData("sub//file.txt")]
    public void UntrustedPathsCannotEscapeOrAliasWindowsDevices(string path) =>
        Assert.Throws<InvalidDataException>(() => WorkspaceFiles.Resolve(Path.GetTempPath(), path));

    [Fact]
    public void OrdinaryNestedResourcePathsStayUnderTheirWorkspace()
    {
        var root = Path.Combine(Path.GetTempPath(), "gamel10n-path-test");
        Assert.Equal(Path.Combine(root, "data", "script.dat"), WorkspaceFiles.Resolve(root, "data/script.dat"));
    }

    [Fact]
    public void HostReadsRespectTheDeclaredSizeLimit()
    {
        var root = Path.Combine(Path.GetTempPath(), "gamel10n-read-test-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(root);
        try
        {
            File.WriteAllBytes(Path.Combine(root, "large.bin"), new byte[1024]);
            Assert.Throws<InvalidDataException>(() => WorkspaceFiles.ReadBounded(root, "large.bin", 1023));
        }
        finally { Directory.Delete(root, true); }
    }
}
