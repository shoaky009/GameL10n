using System.Collections.Frozen;
using System.Text;
using System.Text.Json.Serialization;
using VNTextPatch.Shared.Util;

namespace GameL10n.VnTextPatch;

[JsonUnmappedMemberHandling(JsonUnmappedMemberHandling.Disallow)]
internal sealed record YurisEncodingOptions(string Source = "cp932", string Output = "cp932", string? Mapping = null);

internal sealed record YurisTextCodecs(Encoding Source, Encoding Output)
{
    public static YurisTextCodecs? Create(string format, YurisEncodingOptions? options)
    {
        if (options is null) return null;
        if (format != "Yuris") throw new InvalidDataException("yurisEncoding 仅适用于 YU-RIS。");
        if (options.Source is not ("cp932" or "sjis-tunnel") || options.Output is not ("cp932" or "sjis-tunnel"))
            throw new InvalidDataException("yurisEncoding.source/output 必须是 cp932 或 sjis-tunnel。");
        bool tunnel = options.Source == "sjis-tunnel" || options.Output == "sjis-tunnel";
        if (!tunnel && !string.IsNullOrEmpty(options.Mapping))
            throw new InvalidDataException("CP932 输入及输出不能配置 SJIS tunnel mapping。");
        if (tunnel && options.Mapping is null)
            throw new InvalidDataException("SJIS tunnel 必须显式提供有序 mapping；工具不会新增或重排映射。");
        Encoding strict = StringUtil.SjisEncoding;
        Encoding codec = tunnel ? new YurisTextEncoding(options.Mapping!) : strict;
        return new(options.Source == "sjis-tunnel" ? codec : strict, options.Output == "sjis-tunnel" ? codec : strict);
    }
}

// VNTextProxy v0.0.41 wire format. The caller owns sjis_ext.bin and runtime support.
// This request-local codec consumes a fixed mapping and never allocates new slots.
internal sealed class YurisTextEncoding : Encoding
{
    private const int Capacity = 59 * 58;
    private static readonly byte[] Avoid = [9, 10, 13, 32, 44];
    private static readonly Lazy<FrozenDictionary<char, ushort>> NativeEncodings = new(CreateNativeEncodings);
    private readonly string _mapping;
    private readonly Dictionary<char, ushort> _encoded = [];
    private readonly Encoding _cp932 = StringUtil.SjisEncoding;

    public YurisTextEncoding(string mapping)
    {
        if (mapping.Length > Capacity)
            throw new InvalidDataException("SJIS tunnel mapping 超过 3422 个字符。");
        var seen = new HashSet<char>();
        for (int index = 0; index < mapping.Length; index++)
        {
            char character = mapping[index];
            ValidateCharacter(character);
            if (!seen.Add(character)) throw new InvalidDataException("SJIS tunnel mapping 包含重复字符。");
            _encoded.Add(character, NativeEncodings.Value.TryGetValue(character, out ushort native) ? native : Pair(index));
        }
        _mapping = mapping;
    }

    private static void ValidateCharacter(char character)
    {
        if (character == '\0' || char.IsSurrogate(character))
            throw new InvalidDataException("SJIS tunnel 仅支持非 NUL、无代理项的 BMP 字符。");
    }

    private static FrozenDictionary<char, ushort> CreateNativeEncodings()
    {
        // Python CP932 chooses the first native byte sequence for duplicate Unicode
        // mappings (e.g. U+2170 -> EEEF). .NET's encoder may choose FA40 instead.
        // Invert the strict decoder in byte order, keeping only lead bytes below F0.
        Encoding cp932 = StringUtil.SjisEncoding;
        var encodings = new Dictionary<char, ushort>();
        Span<byte> bytes = stackalloc byte[2];
        Span<char> characters = stackalloc char[2];
        for (int value = 1; value < 0xF000; value++)
        {
            int width = value > 0xFF ? 2 : 1;
            if (width == 1)
            {
                if (value >= 0xF0 || StringUtil.IsShiftJisLeadByte((byte)value)) continue;
                bytes[0] = (byte)value;
            }
            else
            {
                byte high = (byte)(value >> 8), low = (byte)value;
                if (!StringUtil.IsShiftJisLeadByte(high) || low is < 0x40 or 0x7F or > 0xFC) continue;
                ushort canonical = DecoderBytes((ushort)value);
                bytes[0] = (byte)(canonical >> 8);
                bytes[1] = (byte)canonical;
            }
            try
            {
                if (cp932.GetChars(bytes[..width], characters) == 1)
                    encodings.TryAdd(characters[0], (ushort)value);
            }
            catch (DecoderFallbackException) { }
        }
        return encodings.ToFrozenDictionary();
    }

    private static ushort DecoderBytes(ushort value)
    {
        // .NET rejects CP932's duplicate NEC/IBM byte sequences. Python and the
        // VNTextProxy wire format accept them. Resolve aliases before strict decoding.
        int high = value >> 8, low = value & 0xFF;
        if ((high == 0xED || high == 0xEE && low <= 0xEC) && low is >= 0x40 and <= 0xFC and not 0x7F)
        {
            int index = (high - 0xED) * 188 + low - 0x40 - (low > 0x7F ? 1 : 0) + 28;
            int trail = index % 188 + 0x40;
            if (trail >= 0x7F) trail++;
            return (ushort)((0xFA + index / 188) << 8 | trail);
        }
        if (value is >= 0xEEEF and <= 0xEEF8) return (ushort)(0xFA40 + value - 0xEEEF);
        if (value is >= 0xFA4A and <= 0xFA53) return (ushort)(0x8754 + value - 0xFA4A);
        return value switch
        {
            0x8790 => 0x81E0, 0x8791 => 0x81DF, 0x8792 => 0x81E7,
            0x8795 => 0x81E3, 0x8796 => 0x81DB, 0x8797 => 0x81DA,
            0x879A or 0xFA5B => 0x81E6, 0x879B => 0x81BF, 0x879C => 0x81BE,
            0xEEF9 or 0xFA54 => 0x81CA, 0xEEFA => 0xFA55, 0xEEFB => 0xFA56, 0xEEFC => 0xFA57,
            0xFA58 => 0x878A, 0xFA59 => 0x8782, 0xFA5A => 0x8784,
            _ => value
        };
    }

    private string DecodeNative(ReadOnlySpan<byte> bytes)
    {
        if (bytes.Length == 1) return _cp932.GetString(bytes);
        ushort canonical = DecoderBytes((ushort)(bytes[0] << 8 | bytes[1]));
        Span<byte> normalized = stackalloc byte[2];
        normalized[0] = (byte)(canonical >> 8);
        normalized[1] = (byte)canonical;
        return _cp932.GetString(normalized);
    }

    private static ushort Pair(int index)
    {
        int high = index / 58;
        high = high < 31 ? 0x81 + high : 0xE0 + high - 31;
        int low = index % 58 + 1;
        foreach (byte blocked in Avoid) if (low >= blocked) low++;
        return (ushort)(high << 8 | low);
    }

    private ushort EncodeCharacter(char character)
    {
        ValidateCharacter(character);
        if (_encoded.TryGetValue(character, out ushort encoded)) return encoded;
        if (NativeEncodings.Value.TryGetValue(character, out ushort native)) return native;
        throw new InvalidDataException($"SJIS tunnel mapping 缺少字符 U+{(int)character:X4}；请先由调用方建立完整映射。");
    }

    public override int GetByteCount(char[] chars, int index, int count) => GetByteCount(chars.AsSpan(index, count));
    public override int GetByteCount(string value) => GetByteCount(value.AsSpan());
    public override int GetByteCount(ReadOnlySpan<char> chars)
    {
        int count = 0;
        foreach (char character in chars) count = checked(count + (EncodeCharacter(character) > 0xFF ? 2 : 1));
        return count;
    }

    public override byte[] GetBytes(string value)
    {
        byte[] bytes = new byte[GetByteCount(value)];
        GetBytes(value.AsSpan(), bytes);
        return bytes;
    }
    public override int GetBytes(char[] chars, int charIndex, int charCount, byte[] bytes, int byteIndex) =>
        GetBytes(chars.AsSpan(charIndex, charCount), bytes.AsSpan(byteIndex));
    public override int GetBytes(string value, int charIndex, int charCount, byte[] bytes, int byteIndex) =>
        GetBytes(value.AsSpan(charIndex, charCount), bytes.AsSpan(byteIndex));
    public override int GetBytes(ReadOnlySpan<char> chars, Span<byte> bytes)
    {
        if (bytes.Length < GetByteCount(chars)) throw new ArgumentException("Output byte buffer is too small.", nameof(bytes));
        int position = 0;
        foreach (char character in chars)
        {
            ushort encoded = EncodeCharacter(character);
            if (encoded > 0xFF) bytes[position++] = (byte)(encoded >> 8);
            bytes[position++] = (byte)encoded;
        }
        return position;
    }

    public override string GetString(byte[] bytes, int index, int count) => Decode(bytes.AsSpan(index, count));
    private string Decode(ReadOnlySpan<byte> bytes)
    {
        var text = new StringBuilder(bytes.Length);
        for (int position = 0; position < bytes.Length;)
        {
            byte high = bytes[position];
            int width = StringUtil.IsShiftJisLeadByte(high) ? 2 : 1;
            if (position + width > bytes.Length) throw new InvalidDataException("SJIS tunnel 字符字节不完整。");
            ReadOnlySpan<byte> chunk = bytes.Slice(position, width);
            if (chunk.Contains((byte)0)) throw new InvalidDataException("SJIS tunnel 文字中不支持 NUL。");
            if (width == 2 && chunk[1] < 0x40)
            {
                byte low = chunk[1];
                if (Avoid.Contains(low)) throw new InvalidDataException("SJIS tunnel 尾字节与控制或分隔字节冲突。");
                int highIndex = high < 0xA0 ? high - 0x81 : 31 + high - 0xE0;
                int index = highIndex * 58 + low - 1 - Avoid.Count(blocked => blocked < low);
                if (index >= _mapping.Length) throw new InvalidDataException("SJIS tunnel mapping 缺失或不完整。");
                text.Append(_mapping[index]);
            }
            else text.Append(DecodeNative(chunk));
            position += width;
        }
        return text.ToString();
    }
    public override int GetCharCount(byte[] bytes, int index, int count) => Decode(bytes.AsSpan(index, count)).Length;
    public override int GetChars(byte[] bytes, int byteIndex, int byteCount, char[] chars, int charIndex)
    {
        string text = Decode(bytes.AsSpan(byteIndex, byteCount));
        text.AsSpan().CopyTo(chars.AsSpan(charIndex));
        return text.Length;
    }
    public override int GetMaxByteCount(int count) => count >= 0 ? checked(count * 2) : throw new ArgumentOutOfRangeException(nameof(count));
    public override int GetMaxCharCount(int count) => count >= 0 ? count : throw new ArgumentOutOfRangeException(nameof(count));
}
