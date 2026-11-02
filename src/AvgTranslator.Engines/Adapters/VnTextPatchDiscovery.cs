using AvgTranslator.Core.Isolation;
using AvgTranslator.Core.Development;

namespace AvgTranslator.Core.Adapters;

/// <summary>Shares one filename scan across a sequence of format probes; snapshots are always scanned afresh.</summary>
internal sealed class VnTextPatchDiscovery(GameDiscoverySession? inventory = null)
{
    private readonly object sync = new();
    private readonly HashSet<string> probedFormats = new(StringComparer.Ordinal);
    private string? probeDirectory;
    private IReadOnlyList<string> probeResources = [];
    private static readonly HashSet<string> Extensions = VnTextPatchFormatCatalog.All
        .SelectMany(format => format.Extensions).ToHashSet(StringComparer.OrdinalIgnoreCase);
    private static readonly HashSet<string> Companions = VnTextPatchFormatCatalog.All
        .SelectMany(format => format.CompanionFileNames).ToHashSet(StringComparer.OrdinalIgnoreCase);
    private static readonly HashSet<string> ExcludedDirectories = new(StringComparer.OrdinalIgnoreCase)
    {
        ".avg-translator", ".git", "backup", "backups", "output", "outputs", "patches"
    };

    internal IReadOnlyList<string> ForProbe(string directory, VnTextPatchFormat format, CancellationToken cancellationToken)
    {
        var root = Path.GetFullPath(directory);
        lock (sync)
        {
            // Seeing the same format twice starts a new pass, including callers outside the desktop UI.
            if (!string.Equals(root, probeDirectory, StringComparison.OrdinalIgnoreCase) || probedFormats.Contains(format.Key))
            {
                probeResources = inventory is { Complete: true } && root.Equals(inventory.DirectoryPath, StringComparison.OrdinalIgnoreCase)
                    ? inventory.Select(Extensions.Select(extension => "**/*" + extension).Concat(Companions.Select(name => "**/" + name)).ToArray(), cancellationToken)
                        .Where(path => !path.Split('/').SkipLast(1).Any(ExcludedDirectories.Contains)
                            && !path.Split('/').Any(part => part.EndsWith(".bak", StringComparison.OrdinalIgnoreCase)
                                || part.Contains(".pending-", StringComparison.OrdinalIgnoreCase)
                                || part.Contains(".damaged-", StringComparison.OrdinalIgnoreCase))).ToArray()
                    : Scan(root, cancellationToken);
                probeDirectory = root;
                probedFormats.Clear();
            }
            probedFormats.Add(format.Key);
            return Select(probeResources, format);
        }
    }

    internal void Reset()
    {
        lock (sync)
        {
            probeDirectory = null;
            probeResources = [];
            probedFormats.Clear();
        }
    }

    internal static IReadOnlyList<string> Discover(string directory, VnTextPatchFormat format, CancellationToken cancellationToken) =>
        Select(Scan(Path.GetFullPath(directory), cancellationToken), format);

    private static IReadOnlyList<string> Select(IReadOnlyList<string> files, VnTextPatchFormat format) => files
        .Where(file => format.Extensions.Contains(Path.GetExtension(file), StringComparer.OrdinalIgnoreCase)
            || format.CompanionFileNames.Contains(Path.GetFileName(file), StringComparer.OrdinalIgnoreCase)).ToArray();

    private static IReadOnlyList<string> Scan(string root, CancellationToken cancellationToken)
    {
        WorkspaceFiles.RejectLinkAncestors(root);
        if (!Directory.Exists(root)) return [];
        var files = new List<string>();
        var pending = new Stack<string>();
        pending.Push(root);
        while (pending.TryPop(out var directory))
        {
            cancellationToken.ThrowIfCancellationRequested();
            foreach (var entry in Directory.EnumerateFileSystemEntries(directory))
            {
                cancellationToken.ThrowIfCancellationRequested();
                var attributes = File.GetAttributes(entry);
                if ((attributes & FileAttributes.ReparsePoint) != 0) continue;
                var name = Path.GetFileName(entry);
                if (name.EndsWith(".bak", StringComparison.OrdinalIgnoreCase)
                    || name.Contains(".pending-", StringComparison.OrdinalIgnoreCase)
                    || name.Contains(".damaged-", StringComparison.OrdinalIgnoreCase)) continue;
                if ((attributes & FileAttributes.Directory) != 0)
                {
                    if (!ExcludedDirectories.Contains(name)) pending.Push(entry);
                }
                else if (Extensions.Contains(Path.GetExtension(name)) || Companions.Contains(name))
                    files.Add(Path.GetRelativePath(root, entry).Replace('\\', '/'));
            }
        }
        files.Sort(StringComparer.OrdinalIgnoreCase);
        return files;
    }
}
