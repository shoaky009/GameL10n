using System.ComponentModel;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

namespace AvgTranslator.Core.Patching;

internal static class PatchFileSafety
{
    internal const string StateDirectoryName = ".avg-translator";

    internal static string DirectoryRoot(string path)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(path);
        string fullPath = Path.TrimEndingDirectorySeparator(Path.GetFullPath(path));
        if (!Directory.Exists(fullPath))
            throw new DirectoryNotFoundException(fullPath);
        CheckPath(fullPath);
        return fullPath;
    }

    internal static string NormalizeRelativePath(string relativePath, bool allowStateDirectory = false)
    {
        if (string.IsNullOrWhiteSpace(relativePath) || Path.IsPathRooted(relativePath))
            throw new InvalidDataException("A patch path must be relative and nonempty.");

        string normalized = relativePath.Replace('\\', '/');
        string[] parts = normalized.Split('/');
        foreach (string part in parts)
        {
            if (part.Length == 0 || part is "." or ".." || part.EndsWith(' ') || part.EndsWith('.') ||
                part.Any(c => char.IsControl(c) || "<>:\"|?*".Contains(c)))
                throw new InvalidDataException($"Unsafe patch path: {relativePath}");
            string stem = part.Split('.')[0];
            if (stem.Equals("CON", StringComparison.OrdinalIgnoreCase) ||
                stem.Equals("PRN", StringComparison.OrdinalIgnoreCase) ||
                stem.Equals("AUX", StringComparison.OrdinalIgnoreCase) ||
                stem.Equals("NUL", StringComparison.OrdinalIgnoreCase) ||
                (stem.Length == 4 &&
                 (stem.StartsWith("COM", StringComparison.OrdinalIgnoreCase) ||
                  stem.StartsWith("LPT", StringComparison.OrdinalIgnoreCase)) &&
                 "123456789¹²³".Contains(stem[3])))
                throw new InvalidDataException($"A patch path contains a reserved filename: {relativePath}");
        }

        if (!allowStateDirectory && parts[0].Equals(StateDirectoryName, StringComparison.OrdinalIgnoreCase))
            throw new InvalidDataException("A patch cannot modify the installer's state directory.");
        return normalized;
    }

    internal static string UnderRoot(string root, string relativePath, bool allowStateDirectory = false)
    {
        string normalized = NormalizeRelativePath(relativePath, allowStateDirectory);
        string fullPath = Path.GetFullPath(Path.Combine(root, normalized.Replace('/', Path.DirectorySeparatorChar)));
        string prefix = Path.TrimEndingDirectorySeparator(root) + Path.DirectorySeparatorChar;
        if (!fullPath.StartsWith(prefix, StringComparison.OrdinalIgnoreCase))
            throw new InvalidDataException("A patch path escapes its root directory.");
        CheckPath(fullPath);
        return fullPath;
    }

    // Inspect every existing ancestor as well: a safe-looking child can sit below a junction.
    internal static void CheckPath(string path)
    {
        string? current = Path.GetFullPath(path);
        bool leaf = true;
        while (current is not null)
        {
            FileAttributes attributes;
            try { attributes = File.GetAttributes(current); }
            catch (FileNotFoundException) { attributes = 0; }
            catch (DirectoryNotFoundException) { attributes = 0; }

            if ((attributes & FileAttributes.ReparsePoint) != 0)
                throw new IOException($"Symbolic links and reparse points are not allowed: {current}");
            if (leaf && File.Exists(current))
                CheckSingleLink(current);
            leaf = false;
            current = Path.GetDirectoryName(current);
        }
    }

    private static void CheckSingleLink(string path)
    {
        if (OperatingSystem.IsWindows())
        {
            using SafeFileHandle handle = File.OpenHandle(path, FileMode.Open, FileAccess.Read,
                FileShare.ReadWrite | FileShare.Delete);
            if (!GetFileInformationByHandle(handle, out ByHandleFileInformation information))
                throw new IOException($"Cannot inspect file links: {path}", new Win32Exception(Marshal.GetLastPInvokeError()));
            if (information.NumberOfLinks != 1)
                throw new IOException($"Hard-linked files are not allowed: {path}");
            return;
        }

        if (OperatingSystem.IsLinux())
        {
            if (Statx(-100, path, 0x100, 0x4, out StatxInformation information) != 0)
                throw new IOException($"Cannot inspect file links: {path}", new Win32Exception(Marshal.GetLastPInvokeError()));
            if ((information.Mask & 0x4) == 0 || information.NumberOfLinks != 1)
                throw new IOException($"Hard-linked files are not allowed: {path}");
            return;
        }

        throw new PlatformNotSupportedException("Safe patch installation supports Windows and Linux.");
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct ByHandleFileInformation
    {
        public uint FileAttributes;
        public System.Runtime.InteropServices.ComTypes.FILETIME CreationTime;
        public System.Runtime.InteropServices.ComTypes.FILETIME LastAccessTime;
        public System.Runtime.InteropServices.ComTypes.FILETIME LastWriteTime;
        public uint VolumeSerialNumber;
        public uint FileSizeHigh;
        public uint FileSizeLow;
        public uint NumberOfLinks;
        public uint FileIndexHigh;
        public uint FileIndexLow;
    }

    [StructLayout(LayoutKind.Explicit, Size = 256)]
    private struct StatxInformation
    {
        [FieldOffset(0)] public uint Mask;
        [FieldOffset(16)] public uint NumberOfLinks;
    }

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetFileInformationByHandle(SafeFileHandle file, out ByHandleFileInformation information);

    [DllImport("libc", EntryPoint = "statx", SetLastError = true)]
    private static extern int Statx(int directoryFd, [MarshalAs(UnmanagedType.LPUTF8Str)] string path,
        int flags, uint mask, out StatxInformation information);
}
