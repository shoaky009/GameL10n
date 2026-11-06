using System;
using GameRes.Formats.Qlie;
// QLIE v3 cipher and byte-pair decompression, based on GARbro (morkt, MIT).
public static class QlieCodec {
static ulong Add(ulong a,ulong b,int n){ulong r=0, mask=n==32?0xffffffffUL:(1UL<<n)-1;for(int i=0;i<64;i+=n)r|=(((a>>i)+(b>>i))&mask)<<i;return r;}
public static uint Hash(byte[] b){ulong h=0,k=0;for(int p=0;p+8<=b.Length;p+=8){h=Add(h,0x0307030703070307,16);k=Add(k,BitConverter.ToUInt64(b,p)^h,16);}return (uint)(k^(k>>32));}
public static byte[] Crypt(byte[] b,byte[] name,byte[] key,byte[] game,uint arc,bool encrypt=false){uint h=0x85f532,s=0x33f641;for(uint i=0;i<name.Length;i++){h+=(i&255)*name[i];s^=h;}uint len=(uint)b.Length;s+=arc^(7*(len&0xffffff)+len+h+(h^len^0x8f32dc));s=9*(s&0xffffff);if(game!=null)s^=0x453a;var mt=new QlieMersenneTwister(s);mt.XorState(key);if(game!=null)mt.XorState(game);ulong[] tab=new ulong[16];for(int i=0;i<16;i++)tab[i]=mt.Rand64();for(int i=0;i<9;i++)mt.Rand();ulong v=mt.Rand64();uint t=mt.Rand()&15;for(int i=0;i+8<=b.Length;i+=8){v=Add(v^tab[t],tab[t],32);ulong raw=BitConverter.ToUInt64(b,i),d=encrypt?raw:raw^v;Array.Copy(BitConverter.GetBytes(raw^v),0,b,i,8);v=Add(v,d,8)^d;v=Add(((v&0x7fffffff7fffffffUL)<<1),d,16);t=(t+1)&15;}return b;}
public static byte[] Expand(byte[] input){
 if(input.Length<12||BitConverter.ToUInt32(input,0)!=0xff435031)throw new System.IO.InvalidDataException("QLIE 压缩头无效。");
 bool wide=(input[4]&1)!=0; int size=BitConverter.ToInt32(input,8);
 if(size<0||size>512*1024*1024)throw new System.IO.InvalidDataException("QLIE 解压大小超出范围。");
 byte[,] node=new byte[2,256];var stack=new System.Collections.Generic.Stack<int>();byte[] output=new byte[size];int src=12,dst=0;
 int Get(){if(src>=input.Length)throw new System.IO.InvalidDataException("QLIE 压缩数据截断。");return input[src++];}
 while(src<input.Length){
  for(int i=0;i<256;i++)node[0,i]=(byte)i;
  for(int i=0;i<256;){int count=Get();if(count>127){i+=count-127;count=0;}if(i>255)break;count++;if(i+count>256)throw new System.IO.InvalidDataException("QLIE 压缩字典越界。");for(int k=0;k<count;k++,i++){node[0,i]=(byte)Get();if(node[0,i]!=i)node[1,i]=(byte)Get();}}
  int cnt=Get()|(Get()<<8);if(!wide)cnt|=Get()<<16|Get()<<24;
  if(cnt<0||cnt>input.Length-src)throw new System.IO.InvalidDataException("QLIE 压缩块范围无效。");
  long steps=0,limit=512L*(cnt+output.Length-dst+1);
  while(cnt>0||stack.Count>0){if(++steps>limit||stack.Count>512)throw new System.IO.InvalidDataException("QLIE 压缩字典含循环。");int v;if(stack.Count>0)v=stack.Pop();else{cnt--;v=Get();}if(node[0,v]==v){if(dst>=output.Length)throw new System.IO.InvalidDataException("QLIE 解压输出越界。");output[dst++]=(byte)v;}else{stack.Push(node[1,v]);stack.Push(node[0,v]);}}
 }
 if(dst!=output.Length)throw new System.IO.InvalidDataException("QLIE 解压长度不符。");return output;
}

}