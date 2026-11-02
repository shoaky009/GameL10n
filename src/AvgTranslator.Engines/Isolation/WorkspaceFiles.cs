namespace AvgTranslator.Core.Isolation;

/// <summary>Host-side checks for staged and returned files; extension responses are untrusted.</summary>
public static class WorkspaceFiles
{
    public static string Resolve(string root, string relativePath)
    {
        if (string.IsNullOrWhiteSpace(relativePath) || Path.IsPathRooted(relativePath)
            || relativePath.Contains(':') || relativePath.IndexOfAny(['\0', '\r', '\n']) >= 0)
        {
            throw new InvalidDataException("A workspace file must have a relative path without streams or control characters.");
        }
        foreach (var segment in relativePath.Replace('\\', '/').Split('/'))
        {
            if (segment.Length == 0 || segment is "." or ".." || segment.EndsWith(' ') || segment.EndsWith('.')
                || segment.IndexOfAny(['<', '>', '"', '|', '?', '*']) >= 0 || segment.Any(char.IsControl))
                throw new InvalidDataException("A workspace path contains an unsafe or ambiguous component.");
            var name = segment.Split('.')[0].ToUpperInvariant();
            if (name is "CON" or "PRN" or "AUX" or "NUL" or "CONIN$" or "CONOUT$" or "CLOCK$"
                || name.Length == 4 && (name.StartsWith("COM", StringComparison.Ordinal) || name.StartsWith("LPT", StringComparison.Ordinal)) && name[3] is >= '1' and <= '9')
                throw new InvalidDataException("Windows device paths are not workspace files.");
        }

        var fullRoot = Path.GetFullPath(root).TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar);
        var result = Path.GetFullPath(Path.Combine(fullRoot, relativePath));
        var comparison = OperatingSystem.IsWindows() ? StringComparison.OrdinalIgnoreCase : StringComparison.Ordinal;
        if (!result.StartsWith(fullRoot + Path.DirectorySeparatorChar, comparison))
        {
            throw new InvalidDataException("The extension returned a path outside its workspace.");
        }

        RejectLinkAncestors(fullRoot);
        var next = Path.GetDirectoryName(result);
        while (next is not null && next.Length >= fullRoot.Length)
        {
            RejectReparsePoints(next);
            if (string.Equals(next, fullRoot, comparison)) break;
            next = Path.GetDirectoryName(next);
        }
        RejectReparsePoints(result);
        return result;
    }

    public static void RejectReparsePoints(string path)
    {
        if ((Directory.Exists(path) || File.Exists(path)) && (File.GetAttributes(path) & FileAttributes.ReparsePoint) != 0)
        {
            throw new InvalidDataException("Links and reparse points are not accepted in an extension workspace.");
        }
    }

    public static void RejectLinkAncestors(string path)
    {
        var current = Path.GetFullPath(path);
        while (current is not null)
        {
            RejectReparsePoints(current);
            current = Path.GetDirectoryName(current);
        }
    }

    public static byte[] ReadBounded(string root, string relativePath, long maximumBytes)
    {
        var file = Resolve(root, relativePath);
        using var stream = new FileStream(file, FileMode.Open, FileAccess.Read, FileShare.Read);
        if (stream.Length > maximumBytes) throw new InvalidDataException("The extension output exceeds its size limit.");
        var contents = new byte[checked((int)stream.Length)];
        stream.ReadExactly(contents);
        return contents;
    }
}
