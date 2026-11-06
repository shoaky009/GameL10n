using System;
using System.IO;
using System.IO.Compression;
using System.Collections.Generic;
using System.Linq;
using System.Text;

sealed class Fnl {
 public byte[] Original; public Dictionary<char,char> Map=new();
 public sealed class Glyph {public ushort Width;public int Pos,Size;}
 public sealed class Face {public int Height,Uk;public List<Glyph> Glyphs=new();}
 public List<List<Face>> Fonts=new();int indexEnd;
 static int I(byte[] b,int p)=>BitConverter.ToInt32(b,p);
 public static int Index(char pua)=>8994+pua-0xe000;
 public Fnl(byte[] bytes){Original=bytes;if(bytes.Length<20||!bytes.AsSpan(0,4).SequenceEqual("FNA\0"u8)||I(bytes,8)!=bytes.Length)throw new InvalidDataException("FNL header/size mismatch");indexEnd=checked(16+I(bytes,12));if(indexEnd<20||indexEnd>bytes.Length)throw new InvalidDataException("FNL index bounds");using var ms=new MemoryStream(bytes,0,indexEnd);using var r=new BinaryReader(ms);ms.Position=16;int n=r.ReadInt32();if(n<1||n>100)throw new InvalidDataException("FNL font count");for(int f=0;f<n;f++){int count=r.ReadInt32();if(count<1||count>200)throw new InvalidDataException("FNL face count");var faces=new List<Face>();Fonts.Add(faces);for(int j=0;j<count;j++){var face=new Face{Height=r.ReadInt32(),Uk=r.ReadInt32()};int gc=r.ReadInt32();if(face.Height<1||face.Height>4096||gc<3424||gc>11438)throw new InvalidDataException("FNL face layout");faces.Add(face);for(int g=0;g<gc;g++){var x=new Glyph{Width=r.ReadUInt16(),Pos=r.ReadInt32(),Size=r.ReadInt32()};if(x.Width>4096||x.Size<0||x.Pos<indexEnd||x.Pos>bytes.Length-x.Size)throw new InvalidDataException("FNL glyph bounds");face.Glyphs.Add(x);}}}if(ms.Position!=indexEnd)throw new InvalidDataException("FNL index size");
 if(bytes.Length>=12&&bytes.AsSpan(bytes.Length-8).SequenceEqual("A4CNMAP1"u8)){int len=I(bytes,bytes.Length-12);int start=bytes.Length-12-len;if(len<4||start<indexEnd)throw new InvalidDataException("FNL mapping footer");int cnt=I(bytes,start);if(len!=4+cnt*4||cnt<0||cnt>1880)throw new InvalidDataException("FNL mapping count");var used=new HashSet<char>();for(int k=0;k<cnt;k++){char unicode=(char)BitConverter.ToUInt16(bytes,start+4+k*4),pua=(char)BitConverter.ToUInt16(bytes,start+6+k*4);if(char.IsSurrogate(unicode)||pua<0xe000||pua>0xe757||!used.Add(pua)||!Map.TryAdd(unicode,pua)||Fonts.SelectMany(x=>x).Any(x=>x.Glyphs.Count<=Index(pua)))throw new InvalidDataException("FNL mapping invalid");}}
 }
 public byte[] Bitmap(Face face,int index){var g=face.Glyphs[index];int length=checked((g.Width+31)/32*4*face.Height);byte[] a=new byte[length];using var m=new MemoryStream(Original,g.Pos,g.Size);using var z=new ZLibStream(m,CompressionMode.Decompress);int total=0,n;while(total<a.Length&&(n=z.Read(a,total,a.Length-total))>0)total+=n;if(z.ReadByte()!=-1)throw new InvalidDataException("FNL oversized glyph");return a;}
 public byte[] Build(Dictionary<char,char> desired,GlyphRasterizer raster){
 var added=desired.Where(k=>!Map.ContainsKey(k.Key)).ToArray();if(added.Length==0)return Original;
 int max=desired.Values.Max(x=>Index(x))+1;int newSize=4+Fonts.Sum(f=>4+f.Sum(face=>12+Math.Max(max,face.Glyphs.Count)*10));int delta=newSize+16-indexEnd;
 using var data=new MemoryStream();data.Write(Original,indexEnd,Original.Length-indexEnd);var lists=new Dictionary<Face,List<Glyph>>();
 foreach(var face in Fonts.SelectMany(x=>x)) {var list=face.Glyphs.Select(g=>new Glyph{Width=g.Width,Pos=g.Pos+delta,Size=g.Size}).ToList();while(list.Count<max)list.Add(new Glyph{Width=face.Glyphs[0].Width,Pos=face.Glyphs[0].Pos+delta,Size=face.Glyphs[0].Size});int width=face.Glyphs[3423].Width;var reference=Bitmap(face,3423);
 foreach(var entry in added){int index=Index(entry.Value);if(index<face.Glyphs.Count && face.Glyphs[index].Size>0)throw new InvalidDataException("Refusing to overwrite an existing private glyph");byte[] bitmap=raster.Render(entry.Key,width,face.Height,reference);using var packed=new MemoryStream();using(var z=new ZLibStream(packed,CompressionLevel.SmallestSize,true))z.Write(bitmap);byte[] b=packed.ToArray();list[index]=new Glyph{Width=(ushort)width,Pos=checked(16+newSize+(int)data.Length),Size=b.Length};data.Write(b);}lists.Add(face,list);}
 using var output=new MemoryStream();using var w=new BinaryWriter(output,Encoding.UTF8,true);w.Write(Original,0,16);w.Write(Fonts.Count);foreach(var f in Fonts){w.Write(f.Count);foreach(var face in f){w.Write(face.Height);w.Write(face.Uk);w.Write(lists[face].Count);foreach(var g in lists[face]){w.Write(g.Width);w.Write(g.Pos);w.Write(g.Size);}}}if(output.Position!=16+newSize)throw new Exception("FNL rebuild index");data.Position=0;data.CopyTo(output);w.Write(desired.Count);foreach(var e in desired.OrderBy(x=>x.Value)){w.Write((ushort)e.Key);w.Write((ushort)e.Value);}w.Write(4+desired.Count*4);w.Write("A4CNMAP1"u8);byte[] result=output.ToArray();BitConverter.GetBytes(result.Length).CopyTo(result,8);BitConverter.GetBytes(newSize).CopyTo(result,12);
 var check=new Fnl(result);foreach(var face in check.Fonts.SelectMany(x=>x))foreach(var entry in added)if(!check.Bitmap(face,Index(entry.Value)).Any(b=>b!=0))throw new Exception("Rebuilt FNL glyph empty");return result;
 }
}
