using System;
using System.IO;
using System.IO.Compression;
using System.Text;
using System.Collections.Generic;
using System.Linq;

sealed class Ain {
 public byte[] Original, Data; public int Start, End; public List<string> Texts=new(); public List<byte[]> Raw=new();
 public static readonly Encoding Sjis=Encoding.GetEncoding(932,EncoderFallback.ExceptionFallback,DecoderFallback.ExceptionFallback);
 static int I(byte[] b,int p)=>BitConverter.ToInt32(b,p);
 readonly Dictionary<char,char> map; public Ain(byte[] b,Dictionary<char,char>? mapping=null){ map=mapping??new();
 Original=b;
 if(b.Length<18 || !b.AsSpan(0,8).SequenceEqual(new byte[]{65,73,50,0,0,0,0,0}) || I(b,12)!=b.Length-16)throw new InvalidDataException("Unsupported AI2 header");
 using var input=new MemoryStream(b,16,b.Length-16);using var z=new ZLibStream(input,CompressionMode.Decompress);using var o=new MemoryStream();z.CopyTo(o);Data=o.ToArray();
 if(Data.Length!=I(b,8)||!Data.AsSpan(0,4).SequenceEqual("VERS"u8)||I(Data,4)!=14)throw new InvalidDataException("Only AIN version 14 supported");
 int codeEnd=checked(16+I(Data,12));if(codeEnd<16||codeEnd>Data.Length)throw new InvalidDataException("CODE bounds");
 int found=0;
 for(int p=codeEnd;p<Data.Length-12;p++){
 if(!Data.AsSpan(p,4).SequenceEqual("MSG1"u8))continue;
 try{int n=I(Data,p+4),q=p+12;if(n<1||n>(Data.Length-q)/4)continue;var raw=new List<byte[]>();var txt=new List<string>();
 for(int i=0;i<n;i++){int len=I(Data,q);q=checked(q+4);if(len<0||len>Data.Length-q)throw new InvalidDataException();byte[] a=Data.AsSpan(q,len).ToArray();raw.Add((byte[])a.Clone());for(int j=0;j<a.Length;j++)a[j]=unchecked((byte)(a[j]-j-0x60));if(Array.IndexOf(a,(byte)0)>=0)throw new InvalidDataException();string decoded=Sjis.GetString(a); if(map.Count>0){var reverse=map.ToDictionary(k=>k.Value,k=>k.Key); decoded=new string(decoded.Select(c=>reverse.TryGetValue(c,out var u)?u:c).ToArray());}txt.Add(decoded);q+=len;}
 if(q+8>Data.Length||!Data.AsSpan(q,4).SequenceEqual("MAIN"u8))continue;
 found++;Start=p;End=q;Texts=txt;Raw=raw;
 }catch(ArgumentException){}catch(InvalidDataException){}catch(OverflowException){}
 }
 if(found!=1)throw new InvalidDataException("No unique valid MSG1/MAIN table");
 }
 public byte[] Build(Dictionary<int,string> changes){
 if(changes.All(k=>Texts[k.Key]==k.Value))return Original;
 using var m=new MemoryStream();m.Write(Data,0,Start+12);
 for(int i=0;i<Texts.Count;i++){
 // Preserve every unmodified message exactly, including its obfuscation.
 byte[] a;
 if(!changes.TryGetValue(i,out var t)||t==Texts[i]){m.Write(BitConverter.GetBytes(Raw[i].Length));m.Write(Raw[i]);continue;}else{if(t.Contains('\0'))throw new InvalidDataException("NUL is not allowed");a=Sjis.GetBytes(new string(t.Select(c=>map.TryGetValue(c,out var pua)?pua:c).ToArray()));}
 for(int j=0;j<a.Length;j++)a[j]=unchecked((byte)(a[j]+j+0x60));m.Write(BitConverter.GetBytes(a.Length));m.Write(a);
 }
 m.Write(Data,End,Data.Length-End);var plain=m.ToArray();using var packed=new MemoryStream();packed.Write(Original,0,8);packed.Write(BitConverter.GetBytes(plain.Length));packed.Write(new byte[4]);using(var z=new ZLibStream(packed,CompressionLevel.SmallestSize,true))z.Write(plain);var result=packed.ToArray();BitConverter.GetBytes(result.Length-16).CopyTo(result,12);return result;
 }
}
