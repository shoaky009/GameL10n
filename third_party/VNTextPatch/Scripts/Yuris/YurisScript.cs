using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Text;
using VNTextPatch.Shared.Util;

namespace VNTextPatch.Shared.Scripts.Yuris
{
    public class YurisScript : IScript
    {
        public string Extension => ".ybn";

        private IScript? _innerScript;
        private readonly Encoding _readEncoding;
        private readonly Encoding _writeEncoding;

        public YurisScript(Encoding? readEncoding = null, Encoding? writeEncoding = null)
        {
            _readEncoding = readEncoding ?? StringUtil.SjisEncoding;
            _writeEncoding = writeEncoding ?? StringUtil.SjisTunnelEncoding;
        }

        public void Load(ScriptLocation location)
        {
            _innerScript = null;

            string? magic = ReadMagic(location);
            _innerScript = magic switch
                           {
                               "YSTB" => new YurisScenarioScript(_readEncoding, _writeEncoding),
                               "YSCF" => new YurisConfigScript(_readEncoding, _writeEncoding),
                               _ => null
                           };
            _innerScript?.Load(location);
        }

        public IEnumerable<ScriptString> GetStrings()
        {
            return _innerScript?.GetStrings() ?? Enumerable.Empty<ScriptString>();
        }

        public IEnumerable<YurisString> GetStringsWithSources()
        {
            return _innerScript switch
            {
                YurisScenarioScript scenario => scenario.GetStringsWithSources(),
                YurisConfigScript config => config.GetStrings().Select(str =>
                    new YurisString(str, new YurisTextSource(0, "caption", []))),
                _ => Enumerable.Empty<YurisString>()
            };
        }

        public void WritePatched(IEnumerable<ScriptString> strings, ScriptLocation location)
        {
            _innerScript?.WritePatched(strings, location);
        }

        private static string? ReadMagic(ScriptLocation location)
        {
            using Stream stream = File.OpenRead(location.ToFilePath());
            byte[] magic = new byte[4];
            if (stream.Read(magic, 0, magic.Length) < magic.Length)
                return null;

            return Encoding.ASCII.GetString(magic);
        }
    }
}
