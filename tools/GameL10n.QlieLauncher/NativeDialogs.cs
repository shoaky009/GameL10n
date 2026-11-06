using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;

internal static class NativeDialogs
{
    [DllImport("user32.dll", CharSet = CharSet.Unicode, ExactSpelling = true)]
    private static extern int MessageBoxW(IntPtr owner, string text, string caption, uint flags);

    [DllImport("comdlg32.dll", CharSet = CharSet.Unicode, ExactSpelling = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetOpenFileNameW([In, Out] ref OpenFileName file);

    [DllImport("comdlg32.dll", ExactSpelling = true)]
    private static extern uint CommDlgExtendedError();

    // OPENFILENAMEW includes the reserved fields required on current 64-bit Windows.
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct OpenFileName
    {
        public int Size;
        public IntPtr Owner;
        public IntPtr Instance;
        public string Filter;
        public IntPtr CustomFilter;
        public int MaximumCustomFilter;
        public int FilterIndex;
        public StringBuilder File;
        public int MaximumFile;
        public IntPtr FileTitle;
        public int MaximumFileTitle;
        public string InitialDirectory;
        public string Title;
        public uint Flags;
        public ushort FileOffset;
        public ushort FileExtension;
        public string DefaultExtension;
        public IntPtr CustomData;
        public IntPtr Hook;
        public string TemplateName;
        public IntPtr Reserved;
        public uint ReservedValue;
        public uint ExtendedFlags;
    }

    internal static void ShowError(string message)
    {
        MessageBoxW(IntPtr.Zero, message, "汉化启动失败", 0x10);
    }

    internal static string PickLocaleEmulator()
    {
        var selected = new StringBuilder(32768);
        var picker = new OpenFileName
        {
            Size = Marshal.SizeOf<OpenFileName>(),
            Filter = "LEProc.exe\0LEProc.exe\0\0",
            FilterIndex = 1,
            File = selected,
            MaximumFile = selected.Capacity,
            Title = "选择 Locale Emulator 的 LEProc.exe",
            // Do not change the process directory. Require an existing path and file.
            Flags = 0x00080000 | 0x00001000 | 0x00000800 | 0x00000008,
        };
        if (GetOpenFileNameW(ref picker)) return Path.GetFullPath(selected.ToString());
        uint error = CommDlgExtendedError();
        if (error != 0) throw new IOException("无法打开 Locale Emulator 文件选择窗口：" + error.ToString("X"));
        throw new OperationCanceledException("已取消选择 Locale Emulator。");
    }
}
