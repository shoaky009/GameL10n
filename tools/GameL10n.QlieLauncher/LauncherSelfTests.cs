using System;
using System.Buffers.Binary;
using System.IO;
using System.Text;

/// <summary>Exercises AOT parsing paths without loading fonts or starting a game.</summary>
internal static class LauncherSelfTests
{
    internal static int Run()
    {
        string directory = Path.Combine(Path.GetTempPath(), "GameL10n-QlieLauncher-" + Guid.NewGuid().ToString("N"));
        try
        {
            Directory.CreateDirectory(directory);
            if (QlieLauncher.UnderRoot(directory, "qlie-l10n/launch.xml") != Path.Combine(directory, "qlie-l10n", "launch.xml"))
                throw new InvalidDataException("Relative configuration path changed.");
            Reject(() => QlieLauncher.UnderRoot(directory, "../outside.exe"));
            Reject(() => QlieLauncher.UnderRoot(directory, Path.Combine(Path.GetPathRoot(directory), "outside.exe")));
            string xml = Path.Combine(directory, "fixture.xml");
            File.WriteAllText(xml, "<launch executable='Game.exe'><font file='font.ttf' face='Fixture Font'/></launch>");
            if (QlieLauncher.ReadXml(xml).DocumentElement.GetAttribute("executable") != "Game.exe") throw new InvalidDataException("Launch configuration parsing failed.");
            File.WriteAllText(xml, "<!DOCTYPE launch [<!ENTITY external SYSTEM 'file:///not-read'>]><launch>&external;</launch>");
            Reject(() => QlieLauncher.ReadXml(xml));
            string font = Path.Combine(directory, "fixture.ttf");
            File.WriteAllBytes(font, FixtureFont("Fixture Font"));
            QlieLauncher.VerifyFontMetadata(font, "Fixture Font");
            Reject(() => QlieLauncher.VerifyFontMetadata(font, "Other Font"));
            return 0;
        }
        catch (Exception error) { Console.Error.WriteLine(error); return 1; }
        finally { if (Directory.Exists(directory)) Directory.Delete(directory, true); }
    }

    private static void Reject(Action action)
    {
        try { action(); }
        catch (InvalidDataException) { return; }
        catch (System.Xml.XmlException) { return; }
        throw new InvalidDataException("Invalid fixture was accepted.");
    }

    private static byte[] FixtureFont(string face)
    {
        byte[] text = Encoding.BigEndianUnicode.GetBytes(face);
        byte[] font = new byte[28 + 30 + text.Length * 2];
        BinaryPrimitives.WriteUInt32BigEndian(font.AsSpan(0), 0x00010000);
        BinaryPrimitives.WriteUInt16BigEndian(font.AsSpan(4), 1);
        Encoding.ASCII.GetBytes("name").CopyTo(font, 12);
        BinaryPrimitives.WriteUInt32BigEndian(font.AsSpan(20), 28);
        BinaryPrimitives.WriteUInt32BigEndian(font.AsSpan(24), (uint)(font.Length - 28));
        BinaryPrimitives.WriteUInt16BigEndian(font.AsSpan(30), 2);
        BinaryPrimitives.WriteUInt16BigEndian(font.AsSpan(32), 30);
        for (int index = 0; index < 2; index++)
        {
            int at = 34 + index * 12;
            BinaryPrimitives.WriteUInt16BigEndian(font.AsSpan(at), 3);
            BinaryPrimitives.WriteUInt16BigEndian(font.AsSpan(at + 2), 1);
            BinaryPrimitives.WriteUInt16BigEndian(font.AsSpan(at + 4), (ushort)(index == 0 ? 0x409 : 0x411));
            BinaryPrimitives.WriteUInt16BigEndian(font.AsSpan(at + 6), 4);
            BinaryPrimitives.WriteUInt16BigEndian(font.AsSpan(at + 8), (ushort)text.Length);
            BinaryPrimitives.WriteUInt16BigEndian(font.AsSpan(at + 10), (ushort)(index * text.Length));
            text.CopyTo(font, 58 + index * text.Length);
        }
        return font;
    }
}
