using System;
using System.Runtime.InteropServices;
using System.Text;

sealed class GlyphRasterizer : IDisposable
{
    IntPtr dc;
    [StructLayout(LayoutKind.Sequential)] struct Point { public int X, Y; }
    [StructLayout(LayoutKind.Sequential)] struct Metrics { public uint BlackX, BlackY; public Point Origin; public short IncX, IncY; }
    [StructLayout(LayoutKind.Sequential)] struct Matrix { public short A, B, C, D, E, F, G, H; }
    [DllImport("gdi32.dll")] static extern IntPtr CreateCompatibleDC(IntPtr dc);
    [DllImport("gdi32.dll")] static extern bool DeleteDC(IntPtr dc);
    [DllImport("gdi32.dll")] static extern bool DeleteObject(IntPtr value);
    [DllImport("gdi32.dll")] static extern IntPtr SelectObject(IntPtr dc, IntPtr value);
    [DllImport("gdi32.dll", CharSet = CharSet.Unicode)] static extern IntPtr CreateFontW(int h, int w, int e, int o, int weight, uint i, uint u, uint s, uint charset, uint op, uint clip, uint quality, uint pitch, string name);
    [DllImport("gdi32.dll", CharSet = CharSet.Unicode)] static extern uint GetGlyphOutlineW(IntPtr dc, uint c, uint format, out Metrics metrics, uint size, byte[]? buffer, ref Matrix matrix);
    [DllImport("gdi32.dll", CharSet = CharSet.Unicode)] static extern uint GetGlyphIndicesW(IntPtr dc, string text, int count, ushort[] indices, uint flags);
    [DllImport("gdi32.dll", CharSet = CharSet.Unicode)] static extern int GetTextFaceW(IntPtr dc, int count, StringBuilder face);

    public GlyphRasterizer()
    {
        dc = CreateCompatibleDC(IntPtr.Zero);
        if (dc == IntPtr.Zero) throw new Exception("无法创建字体绘制上下文。");
    }

    public byte[] Render(char character, int width, int height, byte[] reference)
    {
        int stride = (width + 31) / 32 * 4;
        var output = new byte[stride * height];
        int minX = width, minY = height, maxX = -1, maxY = -1;
        for (int y = 0; y < height; y++)
        for (int x = 0; x < width; x++)
        {
            if ((reference[(height - 1 - y) * stride + x / 8] & (128 >> (x % 8))) == 0) continue;
            minX = Math.Min(minX, x); maxX = Math.Max(maxX, x);
            minY = Math.Min(minY, y); maxY = Math.Max(maxY, y);
        }
        if (maxX < minX) throw new Exception("原字库参考字形为空。");
        // SimSun is provided by Windows' Simplified Chinese fonts, not redistributed in the adapter.
        IntPtr font = CreateFontW(-width, 0, 0, 0, 400, 0, 0, 0, 134, 0, 0, 3, 0, "SimSun");
        if (font == IntPtr.Zero) throw new Exception("无法创建宋体字形。");
        IntPtr previous = SelectObject(dc, font);
        try
        {
            var selected = new StringBuilder(128);
            if (GetTextFaceW(dc, selected.Capacity, selected) == 0 ||
                !(selected.ToString().Equals("SimSun", StringComparison.OrdinalIgnoreCase) || selected.ToString() == "宋体"))
                throw new Exception("需要 Windows 宋体（SimSun）以保持原游戏字体风格，请安装简体中文补充字体。");
            var index = new ushort[1];
            if (GetGlyphIndicesW(dc, character.ToString(), 1, index, 1) == uint.MaxValue || index[0] is 0 or 0xffff)
                throw new Exception("宋体缺少字形：U+" + ((int)character).ToString("X4"));
            var matrix = new Matrix { B = 1, H = 1 };
            uint size = GetGlyphOutlineW(dc, character, 1, out var metrics, 0, null, ref matrix);
            if (size is 0 or uint.MaxValue || metrics.BlackX == 0 || metrics.BlackY == 0)
                throw new Exception("无法绘制字形：" + character);
            var bits = new byte[size];
            if (GetGlyphOutlineW(dc, character, 1, out metrics, size, bits, ref matrix) == uint.MaxValue)
                throw new Exception("无法读取字形位图。");
            int sourceStride = ((int)metrics.BlackX + 31) / 32 * 4;
            int targetW = maxX - minX + 1, targetH = maxY - minY + 1;
            double scale = Math.Min((double)targetW / metrics.BlackX, (double)targetH / metrics.BlackY);
            int w = Math.Max(1, (int)Math.Round(metrics.BlackX * scale)), h = Math.Max(1, (int)Math.Round(metrics.BlackY * scale));
            int ox = minX + (targetW - w) / 2, oy = minY + (targetH - h) / 2;
            bool ink = false;
            for (int y = 0; y < h; y++)
            for (int x = 0; x < w; x++)
            {
                int sx = Math.Min((int)metrics.BlackX - 1, x * (int)metrics.BlackX / w);
                int sy = Math.Min((int)metrics.BlackY - 1, y * (int)metrics.BlackY / h);
                if ((bits[sy * sourceStride + sx / 8] & (128 >> (sx % 8))) == 0) continue;
                output[(height - 1 - (oy + y)) * stride + (ox + x) / 8] |= (byte)(128 >> ((ox + x) % 8));
                ink = true;
            }
            if (!ink) throw new Exception("生成的中文字形为空。");
            return output;
        }
        finally { SelectObject(dc, previous); DeleteObject(font); }
    }

    public void Dispose()
    {
        if (dc != IntPtr.Zero) { DeleteDC(dc); dc = IntPtr.Zero; }
    }
}
