using System.Buffers.Binary;
using System.Text;

namespace GameL10n.Core.Adapters;

internal sealed class AosArchive
{
    public const int IndexOffset = 0x111;
    private const int EntrySize = 0x28;
    private readonly byte[] _data;
    public IReadOnlyList<Entry> Entries { get; }

    private AosArchive(byte[] data, IReadOnlyList<Entry> entries) => (_data, Entries) = (data, entries);

    public static AosArchive Read(byte[] data)
    {
        if (data.Length < IndexOffset || ReadUInt(data, 0) != 0)
            throw new InvalidDataException("Only LiLiM AOS version 2 is supported.");
        var dataOffset = ReadUInt(data, 4);
        var indexSize = ReadUInt(data, 8);
        if (indexSize == 0 || indexSize % EntrySize != 0 || indexSize / EntrySize > 100_000 ||
            dataOffset < (long)IndexOffset + indexSize || dataOffset > data.Length)
            throw new InvalidDataException("Invalid AOS v2 directory bounds.");
        var entries = new List<Entry>();
        var names = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        for (var index = IndexOffset; index < IndexOffset + indexSize; index += EntrySize)
        {
            var nameBytes = data.AsSpan(index, 32);
            var terminator = nameBytes.IndexOf((byte)0);
            var name = Sfa16Script.ShiftJis.GetString(terminator < 0 ? nameBytes : nameBytes[..terminator]);
            if (string.IsNullOrWhiteSpace(name) || name.IndexOfAny(['/', '\\', ':']) >= 0 || !names.Add(name))
                throw new InvalidDataException("Invalid or duplicate AOS entry name.");
            var offset = (long)dataOffset + ReadUInt(data, index + 32);
            var size = ReadUInt(data, index + 36);
            if (offset < dataOffset || offset + size > data.Length)
                throw new InvalidDataException($"AOS entry is outside the archive: {name}");
            entries.Add(new Entry(name, index, (int)offset, (int)size));
        }
        long previousEnd = dataOffset;
        foreach (var entry in entries.OrderBy(x => x.Offset))
        {
            if (entry.Offset < previousEnd) throw new InvalidDataException("Overlapping AOS entries are unsupported.");
            previousEnd = (long)entry.Offset + entry.Size;
        }
        return new AosArchive(data, entries);
    }

    public byte[] GetBytes(Entry entry) => _data.AsSpan(entry.Offset, entry.Size).ToArray();

    public byte[] Rebuild(IReadOnlyDictionary<string, byte[]> replacements)
    {
        if (replacements.Count == 0) return _data.ToArray();
        if (replacements.Keys.Any(key => !Entries.Any(e => e.Name == key)))
            throw new InvalidDataException("A replacement refers to an unknown AOS entry.");
        var baseOffset = (int)ReadUInt(_data, 4);
        using var output = new MemoryStream();
        output.Write(_data.AsSpan(0, baseOffset));
        var cursor = baseOffset;
        Span<byte> location = stackalloc byte[8];
        foreach (var entry in Entries.OrderBy(x => x.Offset))
        {
            output.Write(_data.AsSpan(cursor, entry.Offset - cursor));
            var relativeOffset = checked((uint)(output.Position - baseOffset));
            var bytes = replacements.TryGetValue(entry.Name, out var replacement) ? replacement : GetBytes(entry);
            output.Write(bytes);
            var end = output.Position;
            output.Position = entry.Index + 32;
            BinaryPrimitives.WriteUInt32LittleEndian(location, relativeOffset);
            BinaryPrimitives.WriteUInt32LittleEndian(location[4..], checked((uint)bytes.Length));
            output.Write(location);
            output.Position = end;
            cursor = entry.Offset + entry.Size;
        }
        output.Write(_data.AsSpan(cursor));
        return output.ToArray();
    }

    private static uint ReadUInt(byte[] data, int offset) => BinaryPrimitives.ReadUInt32LittleEndian(data.AsSpan(offset, 4));
    internal sealed record Entry(string Name, int Index, int Offset, int Size);
}
