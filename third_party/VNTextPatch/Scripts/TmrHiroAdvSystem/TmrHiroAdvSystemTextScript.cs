using System;
using System.Collections.Generic;
using System.IO;
using VNTextPatch.Shared.Util;

namespace VNTextPatch.Shared.Scripts.TmrHiroAdvSystem
{
    public class TmrHiroAdvSystemTextScript : IScript
    {
        public string? Extension => null;

        private byte[] _data = [];

        public void Load(ScriptLocation location)
        {
            _data = File.ReadAllBytes(location.ToFilePath());
        }

        public IEnumerable<ScriptString> GetStrings()
        {
            int pos = 0;
            while (pos < _data.Length)
            {
                if (_data.Length - pos < 2)
                    throw new InvalidDataException("Truncated text length");
                int length = BitConverter.ToInt16(_data, pos);
                pos += 2;
                if (length <= 0 || length > _data.Length - pos)
                    throw new InvalidDataException("Invalid text length");

                string text = StringUtil.SjisEncoding.GetString(_data, pos, length);
                if (text.Any(c => char.IsControl(c) && c is not ('\r' or '\n' or '\t')))
                    throw new InvalidDataException("Invalid control character in text");
                pos += length;

                yield return new ScriptString(text, ScriptStringType.Message);
            }
        }

        public void WritePatched(IEnumerable<ScriptString> strings, ScriptLocation location)
        {
            using Stream stream = File.Open(location.ToFilePath(), FileMode.Create);
            BinaryWriter writer = new BinaryWriter(stream);
            foreach (ScriptString str in strings)
            {
                byte[] textBytes = StringUtil.SjisTunnelEncoding.GetBytes(str.Text);
                if (textBytes.Length > short.MaxValue)
                    throw new InvalidDataException("Translated text exceeds the format's signed 16-bit length");
                writer.Write((short)textBytes.Length);
                writer.Write(textBytes);
            }
        }
    }
}
