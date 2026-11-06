using System;
using System.IO;
using System.Text;
using System.Collections.Generic;
using System.Linq;

// FilePackVer3.0 only, with external fkey and executable IconKeyImage.
public sealed class QlieArchive
{
    public sealed class Entry {
        public byte[] RawName=Array.Empty<byte>();
        public string Name="";
        public long Offset;
        public int Size, UnpackedSize, Packed, Encryption, IndexPosition;
        public uint Hash;
        internal byte[] Key=Array.Empty<byte>();
    }
    readonly byte[] original, gameKey;
    readonly uint arcKey;
    readonly int indexOffset, tailOffset;
    public readonly List<Entry> Entries=new List<Entry>();
    public QlieArchive(byte[] bytes, byte[] fkey, byte[] executableKey) {
        original=bytes; gameKey=executableKey;
        Encoding.RegisterProvider(CodePagesEncodingProvider.Instance);
        var cp=Encoding.GetEncoding(932,EncoderFallback.ExceptionFallback,DecoderFallback.ExceptionFallback);
        if(bytes.Length<0x41c || Encoding.ASCII.GetString(bytes,bytes.Length-28,14)!="FilePackVer3.0")throw new InvalidDataException("仅支持 QLIE FilePackVer3.0。");
        tailOffset=bytes.Length-0x41c;
        arcKey=QlieCodec.Hash(bytes.AsSpan(tailOffset,256).ToArray())&0xfffffff;
        indexOffset=checked((int)BitConverter.ToInt64(bytes,bytes.Length-8));
        int count=BitConverter.ToInt32(bytes,bytes.Length-12),p=indexOffset;
        if(count<1||count>1000000||p<0||p>=tailOffset)throw new InvalidDataException("封包索引范围无效。");
        byte[] key=fkey;bool needPackKey=true;
        for(int i=0;i<count;i++){
            if(p>tailOffset-2)throw new InvalidDataException("封包文件名越界。");
            int len=BitConverter.ToUInt16(bytes,p);p+=2;
            if(len<1||len>256||p>tailOffset-len-28)throw new InvalidDataException("封包索引项无效。");
            byte[] name=bytes.AsSpan(p,len).ToArray();p+=len;
            int nk=len+(int)(arcKey^0x3e);for(int j=0;j<len;j++)name[j]^=(byte)(((j+1)^nk)+j+1);
            var e=new Entry{RawName=name,Name=cp.GetString(name),Offset=BitConverter.ToInt64(bytes,p),Size=checked((int)BitConverter.ToUInt32(bytes,p+8)),UnpackedSize=checked((int)BitConverter.ToUInt32(bytes,p+12)),Packed=BitConverter.ToInt32(bytes,p+16),Encryption=BitConverter.ToInt32(bytes,p+20),Hash=BitConverter.ToUInt32(bytes,p+24),IndexPosition=p,Key=key};p+=28;
            if(e.Offset<0||e.Size<0||e.Offset>indexOffset-e.Size||e.UnpackedSize<0||e.UnpackedSize>512*1024*1024)throw new InvalidDataException("封包数据范围无效。");
            // V3.0 treats every nonzero encryption flag identically.
            Entries.Add(e);
            if(needPackKey&&e.Name.Contains("pack_keyfile",StringComparison.OrdinalIgnoreCase)){key=Read(e);needPackKey=false;}
        }
    }
    public byte[] Read(Entry e) {
        byte[] b=original.AsSpan(checked((int)e.Offset),e.Size).ToArray();
        if(e.Encryption!=0)QlieCodec.Crypt(b,e.RawName,e.Key,gameKey,arcKey);
        if(e.Packed!=0)b=QlieCodec.Expand(b);
        if(e.Packed!=0&&b.Length!=e.UnpackedSize)throw new InvalidDataException("解压长度与索引不一致："+e.Name);
        return b;
    }
    public void VerifyCipherRoundtrip() {
        foreach(var e in Entries){if(e.Encryption==0)continue;var cipher=original.AsSpan(checked((int)e.Offset),e.Size);var b=cipher.ToArray();QlieCodec.Crypt(b,e.RawName,e.Key,gameKey,arcKey);QlieCodec.Crypt(b,e.RawName,e.Key,gameKey,arcKey,true);if(!cipher.SequenceEqual(b))throw new InvalidDataException("加密回环失败："+e.Name);}
    }
    // Unchanged entries retain their ciphertext. Changed entries are stored uncompressed;
    // no editing of pack_keyfile is permitted because it keys subsequent entries.
    public byte[] Build(IReadOnlyDictionary<string,byte[]> replacements) {
        var changes=new Dictionary<Entry,byte[]>();
        foreach(var pair in replacements){var matches=Entries.Where(e=>e.Name==pair.Key).ToArray();if(matches.Length!=1)throw new InvalidDataException("替换目标不唯一或不存在："+pair.Key);var e=matches[0];if(!Read(e).AsSpan().SequenceEqual(pair.Value)){if(e.Name.Contains("pack_keyfile",StringComparison.OrdinalIgnoreCase))throw new InvalidDataException("禁止修改封包密钥项。");changes.Add(e,pair.Value);}}
        if(changes.Count==0)return (byte[])original.Clone();
        using var result=new MemoryStream(); using var writer=new BinaryWriter(result,Encoding.UTF8,true);
        var positions=new Dictionary<Entry,long>(); var hashes=new Dictionary<Entry,uint>();
        // Preserve unknown prefix/padding as well as all original resource data.
        writer.Write(original,0,indexOffset);
        foreach(var item in changes){positions.Add(item.Key,result.Position);byte[] data=(byte[])item.Value.Clone();if(item.Key.Encryption!=0)QlieCodec.Crypt(data,item.Key.RawName,item.Key.Key,gameKey,arcKey,true);hashes.Add(item.Key,QlieCodec.Hash(data));writer.Write(data);}
        long newIndex=result.Position;
        byte[] index=original.AsSpan(indexOffset,tailOffset-indexOffset).ToArray();
        foreach(var item in changes){var e=item.Key;int p=e.IndexPosition-indexOffset;BitConverter.GetBytes(positions[e]).CopyTo(index,p);BitConverter.GetBytes(item.Value.Length).CopyTo(index,p+8);BitConverter.GetBytes(item.Value.Length).CopyTo(index,p+12);BitConverter.GetBytes(0).CopyTo(index,p+16);BitConverter.GetBytes(hashes[e]).CopyTo(index,p+24);}
        writer.Write(index);writer.Write(original,tailOffset,original.Length-tailOffset);writer.Flush();
        byte[] output=result.ToArray();BitConverter.GetBytes(newIndex).CopyTo(output,output.Length-8);return output;
    }
}
