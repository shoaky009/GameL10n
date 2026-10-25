using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.IO;
using System.Linq;
using System.Text;

namespace AvgTranslator.AdapterComponents;

/// <summary>CP932 glyph aliases for complete text units. The caller must prove runtime/font support for every allowed slot.</summary>
public sealed class Cp932GlyphEncoding : Encoding
{
    public const string ComponentVersion = "1.0.0";
    private static readonly Encoding Cp932 = CreateSourceEncoding();
    private readonly Dictionary<int, int> unicodeToSlot;
    public IReadOnlyDictionary<int, int> SlotToUnicode { get; }

    private static Encoding CreateSourceEncoding()
    {
        Encoding.RegisterProvider(CodePagesEncodingProvider.Instance);
        return Encoding.GetEncoding(932, EncoderFallback.ExceptionFallback, DecoderFallback.ExceptionFallback);
    }

    public Cp932GlyphEncoding(IReadOnlyDictionary<int, int> slotToUnicode)
    {
        var copy = new SortedDictionary<int, int>();
        unicodeToSlot = new Dictionary<int, int>();
        foreach (var pair in slotToUnicode)
        {
            RequireSlot(pair.Key);
            if (!Rune.IsValid(pair.Value) || pair.Value == 0 || pair.Key == pair.Value
                || slotToUnicode.ContainsKey(pair.Value) || !unicodeToSlot.TryAdd(pair.Value, pair.Key))
                throw new InvalidDataException("字符映射重复、相互冲突或包含无效 Unicode 值。");
            copy.Add(pair.Key, pair.Value);
        }
        SlotToUnicode = new ReadOnlyDictionary<int, int>(copy);
    }

    /// <summary>
    /// Plan once for the whole output, including unchanged Japanese, names and UI literals. preservedTexts are decoded
    /// logical text, not already encoded proxy characters. Existing mappings remain fixed; collisions must be resolved by the caller.
    /// </summary>
    public static Cp932GlyphEncoding Plan(IEnumerable<string> outputTexts, IEnumerable<int> allowedSlots,
        IEnumerable<string> preservedTexts, IReadOnlyDictionary<int, int>? existingMap = null)
    {
        var existing = new Cp932GlyphEncoding(existingMap ?? new Dictionary<int, int>());
        var slots = new SortedSet<int>(allowedSlots);
        foreach (var slot in slots) RequireSlot(slot);
        if (existing.SlotToUnicode.Keys.Any(slot => !slots.Contains(slot)))
            throw new InvalidDataException("已有映射使用了当前运行时尚未确认的码位。");

        var reserved = new HashSet<int>();
        var needed = new SortedSet<int>();
        var seen = new HashSet<int>();
        foreach (var text in outputTexts)
        foreach (var rune in ReadRunes(text))
        {
            if (!seen.Add(rune.Value)) continue;
            if (slots.Contains(rune.Value)) reserved.Add(rune.Value);
            if (!existing.unicodeToSlot.ContainsKey(rune.Value) && !CanRoundtrip(rune)) needed.Add(rune.Value);
        }
        foreach (var text in preservedTexts)
        foreach (var rune in ReadRunes(text))
            if (slots.Contains(rune.Value)) reserved.Add(rune.Value);
        if (existing.SlotToUnicode.Keys.Any(reserved.Contains))
            throw new InvalidDataException("已有字符映射与保留的原文字形冲突，不能覆盖或重新分配已有映射。");

        var map = existing.SlotToUnicode.ToDictionary(pair => pair.Key, pair => pair.Value);
        using var free = slots.Where(slot => !reserved.Contains(slot) && !map.ContainsKey(slot)).GetEnumerator();
        foreach (var unicode in needed)
        {
            if (!free.MoveNext()) throw new InvalidDataException("已确认且未占用的字形码位不足，无法完整生成中文映射。");
            map.Add(free.Current, unicode);
        }
        return new Cp932GlyphEncoding(map);
    }

    private static IEnumerable<Rune> ReadRunes(string text)
    {
        for (var index = 0; index < text.Length;)
        {
            if (!Rune.TryGetRuneAt(text, index, out var rune))
                throw new EncoderFallbackException("文本包含不完整的 UTF-16 代理对。");
            yield return rune;
            index += rune.Utf16SequenceLength;
        }
    }

    private static bool CanRoundtrip(Rune rune)
    {
        try
        {
            var text = rune.ToString();
            return Cp932.GetString(Cp932.GetBytes(text)) == text;
        }
        catch (EncoderFallbackException) { return false; }
    }

    private static void RequireSlot(int slot)
    {
        if (!Rune.IsValid(slot) || slot > 0xffff || !CanRoundtrip(new Rune(slot))
            || Cp932.GetByteCount(char.ConvertFromUtf32(slot)) != 2)
            throw new InvalidDataException("字形码位必须能通过 CP932 双字节无损往返：U+" + slot.ToString("X4"));
    }

    public string ToNative(string text)
    {
        var output = new StringBuilder(text.Length);
        foreach (var rune in ReadRunes(text))
        {
            if (unicodeToSlot.TryGetValue(rune.Value, out var slot)) output.Append(char.ConvertFromUtf32(slot));
            else
            {
                if (SlotToUnicode.ContainsKey(rune.Value))
                    throw new EncoderFallbackException("原文字形与映射码位冲突，请重新检查全补丁保留文本。");
                if (!CanRoundtrip(rune)) throw new EncoderFallbackException("字符未纳入全补丁映射：U+" + rune.Value.ToString("X4"));
                output.Append(rune.ToString());
            }
        }
        return output.ToString();
    }

    public string FromNative(string text)
    {
        var output = new StringBuilder(text.Length);
        foreach (var rune in ReadRunes(text))
            output.Append(SlotToUnicode.TryGetValue(rune.Value, out var unicode) ? char.ConvertFromUtf32(unicode) : rune.ToString());
        return output.ToString();
    }

    public override int GetByteCount(char[] chars, int index, int count) => Cp932.GetByteCount(ToNative(new string(chars, index, count)));
    public override int GetBytes(char[] chars, int charIndex, int charCount, byte[] bytes, int byteIndex)
    {
        var encoded = Cp932.GetBytes(ToNative(new string(chars, charIndex, charCount)));
        encoded.CopyTo(bytes, byteIndex);
        return encoded.Length;
    }
    public override int GetCharCount(byte[] bytes, int index, int count) => FromNative(Cp932.GetString(bytes, index, count)).Length;
    public override int GetChars(byte[] bytes, int byteIndex, int byteCount, char[] chars, int charIndex)
    {
        var decoded = FromNative(Cp932.GetString(bytes, byteIndex, byteCount));
        decoded.CopyTo(0, chars, charIndex, decoded.Length);
        return decoded.Length;
    }
    public override int GetMaxByteCount(int charCount) => charCount < 0 ? throw new ArgumentOutOfRangeException(nameof(charCount)) : checked(charCount * 2 + 2);
    public override int GetMaxCharCount(int byteCount) => byteCount < 0 ? throw new ArgumentOutOfRangeException(nameof(byteCount)) : checked(byteCount * 2 + 2);
}
