using System.Buffers.Binary;

namespace GameL10n.Core.Adapters;

// AOS SCR layout follows the MIT-licensed GARbro/AosTools implementations.
// See THIRD-PARTY-NOTICES.md for references and license notices.
internal static class AosHuffman
{
    private const int MaxDecodedSize = 16 * 1024 * 1024;

    public static byte[] Decode(byte[] data)
    {
        if (data.Length < 6) throw new InvalidDataException("Truncated AOS Huffman script.");
        var length = BinaryPrimitives.ReadUInt32LittleEndian(data);
        if (length > MaxDecodedSize) throw new InvalidDataException("AOS script exceeds the decoded size limit.");
        var bits = new BitReader(data);
        var nodes = 0;
        Node ReadNode(int depth)
        {
            if (++nodes > 511 || depth > 255) throw new InvalidDataException("Invalid AOS Huffman tree.");
            if (!bits.Read()) return new Node(bits.ReadByte());
            return new Node(ReadNode(depth + 1), ReadNode(depth + 1));
        }
        var root = ReadNode(0);
        var decoded = new byte[length];
        for (var i = 0; i < decoded.Length; i++)
        {
            var node = root;
            while (node.Left is not null) node = bits.Read() ? node.Right! : node.Left;
            decoded[i] = node.Value;
        }
        return decoded;
    }

    public static byte[] Encode(byte[] decoded)
    {
        if (decoded.Length > MaxDecodedSize) throw new InvalidDataException("AOS script exceeds the decoded size limit.");
        var counts = new long[256];
        foreach (var value in decoded) counts[value]++;
        var queue = new PriorityQueue<Node, (long Weight, int Order)>();
        var order = 0;
        for (var value = 0; value < counts.Length; value++)
            if (counts[value] != 0) queue.Enqueue(new Node((byte)value), (counts[value], order++));
        if (queue.Count == 0) queue.Enqueue(new Node(0), (1, order++));
        while (queue.Count > 1)
        {
            queue.TryDequeue(out var left, out var leftWeight);
            queue.TryDequeue(out var right, out var rightWeight);
            queue.Enqueue(new Node(left!, right!), (leftWeight.Weight + rightWeight.Weight, order++));
        }
        var root = queue.Dequeue();
        var codes = new bool[256][];
        var writer = new BitWriter(decoded.Length);
        void WriteNode(Node node, List<bool> path)
        {
            writer.Write(node.Left is not null);
            if (node.Left is null)
            {
                writer.WriteByte(node.Value);
                codes[node.Value] = path.ToArray();
                return;
            }
            path.Add(false);
            WriteNode(node.Left, path);
            path[^1] = true;
            WriteNode(node.Right!, path);
            path.RemoveAt(path.Count - 1);
        }
        WriteNode(root, []);
        foreach (var value in decoded)
            foreach (var bit in codes[value]) writer.Write(bit);
        return writer.ToArray();
    }

    private sealed class Node
    {
        public byte Value { get; }
        public Node? Left { get; }
        public Node? Right { get; }
        public Node(byte value) => Value = value;
        public Node(Node left, Node right) => (Left, Right) = (left, right);
    }

    private sealed class BitReader(byte[] data)
    {
        private int _position = 32;
        public bool Read()
        {
            if (_position >= data.Length * 8) throw new InvalidDataException("Truncated AOS Huffman bit stream.");
            return ((data[_position / 8] >> (7 - _position++ % 8)) & 1) != 0;
        }
        public byte ReadByte()
        {
            var value = 0;
            for (var i = 0; i < 8; i++) value = (value << 1) | (Read() ? 1 : 0);
            return (byte)value;
        }
    }

    private sealed class BitWriter
    {
        private readonly MemoryStream _output = new();
        private int _value;
        private int _count;
        public BitWriter(int length)
        {
            Span<byte> header = stackalloc byte[4];
            BinaryPrimitives.WriteUInt32LittleEndian(header, (uint)length);
            _output.Write(header);
        }
        public void Write(bool bit)
        {
            _value = (_value << 1) | (bit ? 1 : 0);
            if (++_count == 8)
            {
                _output.WriteByte((byte)_value);
                _count = _value = 0;
            }
        }
        public void WriteByte(byte value)
        {
            for (var bit = 7; bit >= 0; bit--) Write((value & (1 << bit)) != 0);
        }
        public byte[] ToArray()
        {
            if (_count != 0) _output.WriteByte((byte)(_value << (8 - _count)));
            return _output.ToArray();
        }
    }
}
