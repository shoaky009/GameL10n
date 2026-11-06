using System.Text;

namespace VNTextPatch.Shared.Util;

// Mware learns its source encoding while decoding; failed writes must never switch encodings.
internal sealed class GuessedEncoding : Encoding
{
    private Encoding _encoding = StringUtil.SjisEncoding;
    private bool _read;

    public override string GetString(byte[] bytes, int index, int count)
    {
        try
        {
            string value = _encoding.GetString(bytes, index, count);
            // ASCII metadata does not establish the encoding of subsequent non-ASCII literals.
            _read |= value.Any(c => c > 0x7F);
            return value;
        }
        catch (DecoderFallbackException) when (!_read)
        {
            _encoding = new UTF8Encoding(false, true);
            _read = true;
            return _encoding.GetString(bytes, index, count);
        }
    }

    public override int GetByteCount(string value) => _encoding.GetByteCount(value);
    public override int GetByteCount(char[] chars, int index, int count) => _encoding.GetByteCount(chars, index, count);
    public override byte[] GetBytes(string value) => _encoding.GetBytes(value);
    public override int GetBytes(string value, int charIndex, int charCount, byte[] bytes, int byteIndex) => _encoding.GetBytes(value, charIndex, charCount, bytes, byteIndex);
    public override int GetBytes(char[] chars, int charIndex, int charCount, byte[] bytes, int byteIndex) => _encoding.GetBytes(chars, charIndex, charCount, bytes, byteIndex);
    public override int GetCharCount(byte[] bytes, int index, int count) => GetString(bytes, index, count).Length;
    public override int GetChars(byte[] bytes, int byteIndex, int byteCount, char[] chars, int charIndex)
    {
        string value = GetString(bytes, byteIndex, byteCount);
        value.CopyTo(0, chars, charIndex, value.Length);
        return value.Length;
    }
    public override int GetMaxByteCount(int count) => _encoding.GetMaxByteCount(count);
    public override int GetMaxCharCount(int count) => _encoding.GetMaxCharCount(count);
}
