using System;
using System.IO;
using System.Text;
using System.Collections.Generic;

// Structural PE resource + Delphi TPF0 reader; never searches arbitrary EXE bytes.
public static class QlieKey
{
    public static byte[] FromExe(byte[] exe)
    {
        using var stream = new MemoryStream(exe, false);
        using var pe = new System.Reflection.PortableExecutable.PEReader(stream);
        var dir = pe.PEHeaders.PEHeader!.ResourceTableDirectory;
        if (dir.Size == 0) throw new InvalidDataException("EXE 缺少资源目录。");
        int Offset(int rva) {
            foreach (var s in pe.PEHeaders.SectionHeaders)
                if (rva >= s.VirtualAddress && rva - s.VirtualAddress < s.SizeOfRawData)
                    return checked(s.PointerToRawData + rva - s.VirtualAddress);
            throw new InvalidDataException("PE 资源地址无效。");
        }
        int basePos = Offset(dir.RelativeVirtualAddress);
        uint U(int p) => BitConverter.ToUInt32(exe, p);
        int Find(int relative, string name) {
            int p=checked(basePos+relative), count=BitConverter.ToUInt16(exe,p+12)+BitConverter.ToUInt16(exe,p+14);
            for(int i=0;i<count;i++) {
                int e=p+16+i*8;uint id=U(e);
                string n=(id&0x80000000)!=0 ? ResourceName(checked(basePos+(int)(id&0x7fffffff))) : "#"+id;
                if(n==name)return unchecked((int)U(e+4));
            }
            throw new InvalidDataException("缺少 PE 资源："+name);
        }
        string ResourceName(int p) => Encoding.Unicode.GetString(exe,p+2,checked(BitConverter.ToUInt16(exe,p)*2));
        int type=Find(0,"#10");
        if(type>=0)throw new InvalidDataException("无效的 PE 资源层级。");
        int form=Find(type&0x7fffffff,"TFORM1");
        if(form>=0)throw new InvalidDataException("无效的 TFORM1 资源层级。");
        int lang=checked(basePos+(form&0x7fffffff));
        int nlang=BitConverter.ToUInt16(exe,lang+12)+BitConverter.ToUInt16(exe,lang+14);
        if(nlang<1)throw new InvalidDataException("TFORM1 缺少语言项。");
        uint leaf=U(lang+20);
        if((leaf&0x80000000)!=0)throw new InvalidDataException("无效的 TFORM1 数据项。");
        int dp=checked(basePos+(int)leaf);
        int start=Offset(checked((int)U(dp))), size=checked((int)U(dp+4));
        if(size<4 || start>exe.Length-size)throw new InvalidDataException("TFORM1 长度无效。");
        using var data=new BinaryReader(new MemoryStream(exe,start,size,false));
        if(data.ReadUInt32()!=0x30465054)throw new InvalidDataException("TFORM1 不是 TPF0。");
        byte[]? key=null; int nodes=0;
        byte[] Bytes(int length) {if(length<0 || length>data.BaseStream.Length-data.BaseStream.Position)throw new InvalidDataException("TPF0 数据越界。");var b=data.ReadBytes(length);if(b.Length!=length)throw new EndOfStreamException();return b;}
        string Str(int length) => Encoding.Latin1.GetString(Bytes(length));
        string Short() => Str(data.ReadByte());
        object? Value(int depth=0) {
            if(depth>64)throw new InvalidDataException("TPF0 值嵌套过深。");
            byte t=data.ReadByte();
            switch(t){
                case 0:return null;
                case 1: {var list=new List<object?>();while(true){long p=data.BaseStream.Position;if(data.ReadByte()==0)break;data.BaseStream.Position=p;list.Add(Value(depth+1));}return list;}
                case 2:return data.ReadByte();case 3:return data.ReadUInt16();case 4:return data.ReadInt32();
                case 5:return Bytes(10);case 6:case 7:return Short();case 8:return false;case 9:return true;
                case 10:return Bytes(data.ReadInt32());
                case 11:{var list=new List<string>();string s;while((s=Short()).Length!=0)list.Add(s);return list;}
                case 12:return Bytes(4);case 15:case 16:return Bytes(8);
                case 18:return Encoding.Unicode.GetString(Bytes(checked(data.ReadInt32()*2)));
                case 19:return data.ReadInt64();case 20:return Encoding.UTF8.GetString(Bytes(data.ReadInt32()));
                default:throw new InvalidDataException("不支持的 TPF0 值类型："+t);
            }
        }
        bool Node(int depth) {
            int len=data.ReadByte();if(len==0)return false;
            if(depth>64 || ++nodes>100000)throw new InvalidDataException("TPF0 节点超限。");
            if(len>=0xf0)throw new InvalidDataException("暂不支持带前缀的 TPF0 节点。");
            string typeName=Str(len),name=Short(),prop;
            while((prop=Short()).Length!=0){object? value=Value();if(name=="IconKeyImage" && prop=="Picture.Data" && value is byte[] icon){
                if(icon.Length<262 || Encoding.ASCII.GetString(icon,0,6)!="\u0005TIcon")throw new InvalidDataException("IconKeyImage 不是有效 TIcon。");
                if(key!=null)throw new InvalidDataException("存在重复的 IconKeyImage。");key=icon.AsSpan(6,256).ToArray();
            }}
            while(Node(depth+1)){}return true;
        }
        if(!Node(0)||key==null)throw new InvalidDataException("TFORM1 缺少 IconKeyImage 密钥。");
        return key;
    }
}
