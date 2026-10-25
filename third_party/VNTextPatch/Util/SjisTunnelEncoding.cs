using System.Text;

namespace VNTextPatch.Shared.Util;

// Keep the upstream API, but never emit tunneled bytes without a verified game runtime hook.
public sealed class SjisTunnelEncoding : Encoding
{
    private static Encoding Strict
    {
        get
        {
            Encoding.RegisterProvider(CodePagesEncodingProvider.Instance);
            return Encoding.GetEncoding(932, EncoderFallback.ExceptionFallback, DecoderFallback.ExceptionFallback);
        }
    }

    private readonly Encoding _encoding = Strict;
    public override int GetByteCount(char[] chars, int index, int count) => _encoding.GetByteCount(chars, index, count);
    public override int GetByteCount(string value) => _encoding.GetByteCount(value);
    public override byte[] GetBytes(string value) => _encoding.GetBytes(value);
    public override int GetBytes(char[] chars, int charIndex, int charCount, byte[] bytes, int byteIndex) => _encoding.GetBytes(chars, charIndex, charCount, bytes, byteIndex);
    public override int GetBytes(string value, int charIndex, int charCount, byte[] bytes, int byteIndex) => _encoding.GetBytes(value, charIndex, charCount, bytes, byteIndex);
    public override int GetCharCount(byte[] bytes, int index, int count) => _encoding.GetCharCount(bytes, index, count);
    public override int GetChars(byte[] bytes, int byteIndex, int byteCount, char[] chars, int charIndex) => _encoding.GetChars(bytes, byteIndex, byteCount, chars, charIndex);
    public override string GetString(byte[] bytes, int index, int count) => _encoding.GetString(bytes, index, count);
    public override int GetMaxByteCount(int count) => _encoding.GetMaxByteCount(count);
    public override int GetMaxCharCount(int count) => _encoding.GetMaxCharCount(count);
}
