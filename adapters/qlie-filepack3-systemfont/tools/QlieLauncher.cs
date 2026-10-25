using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Windows.Forms;
using System.Xml;
using Microsoft.Win32;

// A Windows .NET Framework launcher: the player does not need the translator or its SDK.
internal static class QlieLauncher
{
    [DllImport("gdi32.dll", CharSet = CharSet.Unicode, ExactSpelling = true)]
    private static extern int AddFontResourceExW(string file, uint flags, IntPtr reserved);
    [DllImport("gdi32.dll", CharSet = CharSet.Unicode, ExactSpelling = true)]
    private static extern bool RemoveFontResourceExW(string file, uint flags, IntPtr reserved);
    [DllImport("kernel32.dll")] private static extern uint GetACP();
    [DllImport("gdi32.dll")] private static extern IntPtr CreateCompatibleDC(IntPtr dc);
    [DllImport("gdi32.dll")] private static extern bool DeleteDC(IntPtr dc);
    private delegate int FontCallback(IntPtr font, IntPtr metrics, uint type, IntPtr data);
    [DllImport("gdi32.dll", ExactSpelling = true)]
    private static extern int EnumFontFamiliesExA(IntPtr dc, byte[] query, FontCallback callback, IntPtr data, uint flags);

    [STAThread]
    private static int Main(string[] args)
    {
        var loaded = new List<string>();
        try
        {
            var root = AppDomain.CurrentDomain.BaseDirectory;
            var config = ReadXml(Path.Combine(root, "qlie-l10n", "launch.xml"));
            var game = UnderRoot(root, config.DocumentElement.GetAttribute("executable"));
            if (!File.Exists(game)) throw new IOException("未找到原游戏程序，请先将汉化补丁安装到完整游戏目录。");

            var info = new ProcessStartInfo { WorkingDirectory = Path.GetDirectoryName(game) };
            string ownProfile = game + ".le.config";
            if (File.Exists(ownProfile) || GetACP() != 932)
            {
                string le = args.Length == 2 && args[0] == "--locale-emulator" ? Path.GetFullPath(args[1]) : FindLocaleEmulator();
                if (!File.Exists(le)) throw new IOException("未找到 Locale Emulator 的 LEProc.exe。");
                XmlElement profile;
                if (File.Exists(ownProfile))
                {
                    profile = ReadXml(ownProfile).SelectSingleNode("/LEConfig/Profiles/Profile") as XmlElement;
                    info.Arguments = "-run " + Quote(game);
                }
                else
                {
                    profile = ReadXml(Path.Combine(Path.GetDirectoryName(le), "LEConfig.xml"))
                        .SelectSingleNode("/LEConfig/Profiles/Profile[Location='ja-JP']") as XmlElement;
                    if (profile == null) throw new InvalidDataException("请在 Locale Emulator 中为原游戏程序设置日语运行配置，然后重新启动汉化程序。");
                    Guid guid;
                    if (!Guid.TryParse(profile.GetAttribute("Guid"), out guid)) throw new InvalidDataException("Locale Emulator 配置标识无效。");
                    info.Arguments = "-runas " + guid.ToString() + " " + Quote(game);
                }
                if (profile == null || profile.SelectSingleNode("Location") == null || profile.SelectSingleNode("Location").InnerText != "ja-JP")
                    throw new InvalidDataException("原游戏的 Locale Emulator 配置不是日语。请将原 EXE 的运行区域设置为日语后再试。");
                if (profile.SelectSingleNode("RunWithSuspend") != null && profile.SelectSingleNode("RunWithSuspend").InnerText == "true")
                    throw new InvalidDataException("请关闭原游戏 Locale Emulator 配置中的暂停进程选项后再启动。");
                info.FileName = le;
                bool elevated = profile.SelectSingleNode("RunAsAdmin") != null && profile.SelectSingleNode("RunAsAdmin").InnerText == "true";
                info.UseShellExecute = elevated;
                if (elevated) info.Verb = "runas";
            }
            else
            {
                info.FileName = game;
                info.UseShellExecute = false;
            }

            foreach (XmlElement font in config.SelectNodes("/launch/font"))
            {
                string path = UnderRoot(root, font.GetAttribute("file"));
                VerifyFontMetadata(path, font.GetAttribute("face"));
                if (!File.Exists(path) || AddFontResourceExW(path, 0, IntPtr.Zero) == 0)
                    throw new IOException("无法加载汉化字体：" + font.GetAttribute("file"));
                loaded.Add(path);
            }
            if (loaded.Count == 0) throw new InvalidDataException("汉化启动配置没有声明字体。");
            VerifyFontNames(config);
            // LEProc waits for the game process. Keep the session fonts loaded for that entire lifetime.
            using (var child = Process.Start(info))
            {
                if (child == null) throw new IOException("游戏进程未能启动。");
                child.WaitForExit();
                return child.ExitCode;
            }
        }
        catch (Exception error)
        {
            MessageBox.Show(error.Message, "汉化启动失败", MessageBoxButtons.OK, MessageBoxIcon.Error);
            return 1;
        }
        finally
        {
            foreach (var font in loaded) RemoveFontResourceExW(font, 0, IntPtr.Zero);
        }
    }

    private static void VerifyFontNames(XmlDocument config)
    {
        var missing = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        foreach (XmlElement font in config.SelectNodes("/launch/font"))
        {
            string face = font.GetAttribute("face");
            if (string.IsNullOrWhiteSpace(face) || face.Length > 31)
                throw new InvalidDataException("汉化启动配置缺少有效的完整字体名称，请重新生成补丁。");
            missing.Add(face);
        }
        IntPtr dc = CreateCompatibleDC(IntPtr.Zero);
        if (dc == IntPtr.Zero) throw new IOException("无法检查汉化字体。");
        try
        {
            var query = new byte[60]; // LOGFONTA; QLIE enumerates SHIFTJIS_CHARSET.
            query[23] = 128;
            FontCallback callback = delegate(IntPtr font, IntPtr metrics, uint type, IntPtr data)
            {
                // QLIE compares elfFullName, not lfFaceName. The adapter uses the system font list (no fixed-pitch filter).
                if (Marshal.ReadByte(font, 23) == 128)
                    missing.Remove(Marshal.PtrToStringAnsi(IntPtr.Add(font, 60)));
                return 1;
            };
            EnumFontFamiliesExA(dc, query, callback, IntPtr.Zero, 0);
            GC.KeepAlive(callback);
        }
        finally { DeleteDC(dc); }
        if (missing.Count != 0)
            throw new IOException("游戏无法识别汉化字体，请重新生成补丁：" + string.Join("、", missing));
    }

    // LE uses Japanese name ID 4 (or English as fallback), instead of native GDI's variable-instance name.
    private static void VerifyFontMetadata(string path, string expected)
    {
        if (string.IsNullOrWhiteSpace(expected) || expected.Length > 31)
            throw new InvalidDataException("汉化启动配置缺少有效的完整字体名称，请重新生成补丁。");
        using (var stream = File.OpenRead(path))
        using (var reader = new BinaryReader(stream))
        {
            byte[] header = reader.ReadBytes(12);
            int count = ReadUInt16(header, 4);
            for (int i = 0; i < count; i++)
            {
                byte[] record = reader.ReadBytes(16);
                if (record.Length != 16) break;
                if (System.Text.Encoding.ASCII.GetString(record, 0, 4) != "name") continue;
                uint offset = ReadUInt32(record, 8), length = ReadUInt32(record, 12);
                if (length > 1024 * 1024 || (long)offset + length > stream.Length) break;
                stream.Position = offset;
                byte[] names = reader.ReadBytes((int)length);
                int records = ReadUInt16(names, 2), storage = ReadUInt16(names, 4);
                bool english = false;
                for (int j = 0; j < records; j++)
                {
                    int at = 6 + j * 12;
                    if (ReadUInt16(names, at) != 3 || ReadUInt16(names, at + 2) != 1 || ReadUInt16(names, at + 6) != 4) continue;
                    int language = ReadUInt16(names, at + 4);
                    if (language != 0x409 && language != 0x411) continue;
                    int size = ReadUInt16(names, at + 8), start = storage + ReadUInt16(names, at + 10);
                    if ((size & 1) != 0 || start > names.Length - size ||
                        System.Text.Encoding.BigEndianUnicode.GetString(names, start, size) != expected)
                        throw new InvalidDataException("汉化字体内部名称与游戏配置不一致，转区运行会回退到原字体。请重新生成补丁。");
                    if (language == 0x409) english = true;
                }
                if (english) return;
                break;
            }
        }
        throw new InvalidDataException("汉化字体缺少可用于转区的完整名称，请重新生成补丁。");
    }

    private static int ReadUInt16(byte[] bytes, int at)
    {
        if (at < 0 || at > bytes.Length - 2) throw new InvalidDataException("汉化字体数据不完整。");
        return (bytes[at] << 8) | bytes[at + 1];
    }

    private static uint ReadUInt32(byte[] bytes, int at)
    {
        return ((uint)ReadUInt16(bytes, at) << 16) | (uint)ReadUInt16(bytes, at + 2);
    }

    private static string FindLocaleEmulator()
    {
        const string key = @"SOFTWARE\Classes\CLSID\{C52B9871-E5E9-41FD-B84D-C5ACADBEC7AE}\InprocServer32";
        foreach (var hive in new[] { RegistryHive.CurrentUser, RegistryHive.LocalMachine })
            foreach (var view in new[] { RegistryView.Registry64, RegistryView.Registry32 })
                using (var registry = RegistryKey.OpenBaseKey(hive, view))
                using (var registration = registry.OpenSubKey(key))
                {
                    string location = registration == null ? null : registration.GetValue("CodeBase") as string;
                    Uri uri;
                    if (location == null || !Uri.TryCreate(location, UriKind.Absolute, out uri) || !uri.IsFile) continue;
                    string path = Path.Combine(Path.GetDirectoryName(uri.LocalPath), "LEProc.exe");
                    if (File.Exists(path)) return path;
                }
        using (var picker = new OpenFileDialog { Title = "选择 Locale Emulator 的 LEProc.exe", Filter = "LEProc.exe|LEProc.exe", CheckFileExists = true })
        {
            if (picker.ShowDialog() != DialogResult.OK) throw new OperationCanceledException("已取消选择 Locale Emulator。");
            return picker.FileName;
        }
    }

    private static XmlDocument ReadXml(string path)
    {
        var document = new XmlDocument { XmlResolver = null };
        using (var reader = XmlReader.Create(path, new XmlReaderSettings { DtdProcessing = DtdProcessing.Prohibit, XmlResolver = null })) document.Load(reader);
        return document;
    }

    private static string UnderRoot(string root, string relative)
    {
        string path = Path.GetFullPath(Path.Combine(root, relative));
        if (string.IsNullOrWhiteSpace(relative) || Path.IsPathRooted(relative) || !path.StartsWith(Path.GetFullPath(root).TrimEnd('\\') + "\\", StringComparison.OrdinalIgnoreCase))
            throw new InvalidDataException("汉化启动配置中的文件路径无效。");
        return path;
    }

    private static string Quote(string path) { return "\"" + path + "\""; }
}
