using System;
using System.IO;
using System.Text;
using System.Linq;
using System.Collections.Generic;

// SFNT transformation: preserve outlines/metrics, add Unicode aliases, rename derivative.
public static class QlieFont
{
 static ushort U16(byte[] b,int p){return (ushort)((b[p]<<8)|b[p+1]);}
 static uint U32(byte[] b,int p){return ((uint)U16(b,p)<<16)|U16(b,p+2);}
 static void W16(byte[] b,int p,int v){b[p]=(byte)(v>>8);b[p+1]=(byte)v;}
 static void W32(byte[] b,int p,uint v){W16(b,p,(int)(v>>16));W16(b,p+2,(int)v);}
 static uint Sum(byte[] b){uint s=0;for(int p=0;p<b.Length;p+=4){uint v=0;for(int j=0;j<4;j++)v=(v<<8)|(p+j<b.Length?b[p+j]:0u);s=unchecked(s+v);}return s;}
 public static SortedDictionary<int,int> ReadMap(byte[] b)
 {
  var m=new SortedDictionary<int,int>();int n=U16(b,2);
  for(int i=0;i<n;i++) {int rec=4+i*8;int platform=U16(b,rec);if(platform!=0&&platform!=3)continue;int p=(int)U32(b,rec+4);int fmt=U16(b,p);
   if(fmt==12){int cnt=(int)U32(b,p+12);for(int j=0;j<cnt;j++){int q=p+16+j*12,a=(int)U32(b,q),z=(int)U32(b,q+4),g=(int)U32(b,q+8);for(int c=a;c<=z;c++)if(g+c-a!=0)m[c]=g+c-a;}}
   if(fmt==4){int cnt=U16(b,p+6)/2,ep=p+14,sp=ep+cnt*2+2,dp=sp+cnt*2,rp=dp+cnt*2;for(int j=0;j<cnt;j++){int a=U16(b,sp+j*2),z=U16(b,ep+j*2),d=(short)U16(b,dp+j*2),r=U16(b,rp+j*2);for(int c=a;c<=z&&c<65535;c++){int g=r==0?(c+d)&65535:U16(b,rp+j*2+r+(c-a)*2);if(r!=0&&g!=0)g=(g+d)&65535;if(g!=0)m[c]=g;}}}
  }return m;
 }
 class Seg{public int A,Z,D;public List<int> G;}
 static byte[] Cmap(SortedDictionary<int,int> m)
 {
  var keys=m.Keys.Where(c=>c<65535).ToArray();var segs=new List<Seg>();
  for(int i=0;i<keys.Length;){int a=keys[i],j=i+1,d=(m[a]-a)&65535;while(j<keys.Length&&keys[j]==keys[j-1]+1&&((m[keys[j]]-keys[j])&65535)==d)j++;
   if(j-i>=3){segs.Add(new Seg{A=a,Z=keys[j-1],D=d});i=j;continue;}
   var gs=new List<int>();gs.Add(m[a]);j=i+1;
   while(j<keys.Length&&keys[j]==keys[j-1]+1){int k=j+1,dd=(m[keys[j]]-keys[j])&65535;while(k<keys.Length&&keys[k]==keys[k-1]+1&&((m[keys[k]]-keys[k])&65535)==dd)k++;if(k-j>=3)break;gs.Add(m[keys[j]]);j++;}
   segs.Add(new Seg{A=a,Z=keys[j-1],G=gs});i=j;
  }
  segs.Add(new Seg{A=65535,Z=65535,D=1});int n=segs.Count,arr=16+n*8,len=arr+segs.Where(s=>s.G!=null).Sum(s=>s.G.Count*2);if(len>65535)throw new InvalidDataException("字体 BMP cmap 超过格式4容量。");
  byte[] f4=new byte[len];W16(f4,0,4);W16(f4,2,len);W16(f4,6,n*2);int pow=1,log=0;while(pow*2<=n){pow*=2;log++;}W16(f4,8,pow*2);W16(f4,10,log);W16(f4,12,n*2-pow*2);int cursor=arr;
  for(int i=0;i<n;i++){var s=segs[i];W16(f4,14+i*2,s.Z);W16(f4,16+n*2+i*2,s.A);W16(f4,16+n*4+i*2,s.D);int rp=16+n*6+i*2;if(s.G!=null){W16(f4,rp,cursor-rp);foreach(int g in s.G){W16(f4,cursor,g);cursor+=2;}}}
  var groups=new List<int[]>();foreach(var kv in m){if(groups.Count>0){var g=groups[groups.Count-1];if(kv.Key==g[1]+1&&kv.Value==g[2]+kv.Key-g[0]){g[1]++;continue;}}groups.Add(new[]{kv.Key,kv.Key,kv.Value});}
  byte[] f12=new byte[16+groups.Count*12];W16(f12,0,12);W32(f12,4,(uint)f12.Length);W32(f12,12,(uint)groups.Count);for(int i=0;i<groups.Count;i++)for(int j=0;j<3;j++)W32(f12,16+i*12+j*4,(uint)groups[i][j]);
  byte[] result=new byte[36+f4.Length+f12.Length];W16(result,2,4);int[] plats={0,0,3,3},enc={3,4,1,10};for(int i=0;i<4;i++){W16(result,4+i*8,plats[i]);W16(result,6+i*8,enc[i]);W32(result,8+i*8,(uint)(36+(i%2==0?0:f4.Length)));}Array.Copy(f4,0,result,36,f4.Length);Array.Copy(f12,0,result,36+f4.Length,f12.Length);return result;
 }
 static byte[] Name(byte[] old,string family)
 {
  var records=new List<Tuple<int,int,int,int,byte[]>>();int n=U16(old,2),start=U16(old,4);
  for(int i=0;i<n;i++){int p=6+i*12,platform=U16(old,p),encoding=U16(old,p+2),lang=U16(old,p+4),id=U16(old,p+6);int len=U16(old,p+8),off=U16(old,p+10);byte[] value=new byte[len];Array.Copy(old,start+off,value,0,len);string s=null;
   // Locale Emulator replaces elfFullName with name ID 4; native GDI names the variable instance "<family> Regular".
   if(id==1||id==16||id==21)s=family;if(id==4)s=family+" Regular";if(id==2||id==17||id==22)s="Regular";if(id==3)s=family+";QLIE;3";if(id==6)s=family.Replace(" ","")+"-Regular";
   if(s!=null){if(platform==0||platform==3)value=Encoding.BigEndianUnicode.GetBytes(s);else if(platform==1)value=Encoding.ASCII.GetBytes(s);else continue;}
   records.Add(Tuple.Create(platform,encoding,lang,id,value));
  }
  int pos=6+records.Count*12;byte[] b=new byte[pos+records.Sum(r=>r.Item5.Length)];W16(b,2,records.Count);W16(b,4,pos);int at=pos;for(int i=0;i<records.Count;i++){var r=records[i];int p=6+i*12;W16(b,p,r.Item1);W16(b,p+2,r.Item2);W16(b,p+4,r.Item3);W16(b,p+6,r.Item4);W16(b,p+8,r.Item5.Length);W16(b,p+10,at-pos);Array.Copy(r.Item5,0,b,at,r.Item5.Length);at+=r.Item5.Length;}return b;
 }
 public static void Create(string input,string output,string family,IDictionary<int,int> aliases)
 {
  byte[] b=File.ReadAllBytes(input);var tables=new SortedDictionary<string,byte[]>(StringComparer.Ordinal);int n=U16(b,4);for(int i=0;i<n;i++){int p=12+i*16;string tag=Encoding.ASCII.GetString(b,p,4);if(tag=="DSIG")continue;int off=(int)U32(b,p+8),len=(int)U32(b,p+12);byte[] data=new byte[len];Array.Copy(b,off,data,0,len);tables[tag]=data;}
  var map=ReadMap(tables["cmap"]);foreach(var alias in aliases){int g;if(!map.TryGetValue(alias.Value,out g))throw new InvalidDataException("字体缺少字符 U+"+alias.Value.ToString("X4"));map[alias.Key]=g;}
  tables["cmap"]=Cmap(map);tables["name"]=Name(tables["name"],family);var os=tables["OS/2"];if(os.Length>=86)W32(os,78,U32(os,78)|(1u<<17));W32(tables["head"],8,0);
  NormalizeMetrics(tables);
  n=tables.Count;int offset=12+n*16;byte[] result=new byte[offset+tables.Values.Sum(v=>(v.Length+3)&~3)];Array.Copy(b,result,12);W16(result,4,n);int pow=1,log=0;while(pow*2<=n){pow*=2;log++;}W16(result,6,pow*16);W16(result,8,log);W16(result,10,n*16-pow*16);int idx=0,head=0;foreach(var t in tables){int p=12+idx++*16;Encoding.ASCII.GetBytes(t.Key).CopyTo(result,p);W32(result,p+4,Sum(t.Value));W32(result,p+8,(uint)offset);W32(result,p+12,(uint)t.Value.Length);t.Value.CopyTo(result,offset);if(t.Key=="head")head=offset;offset+=(t.Value.Length+3)&~3;}W32(result,head+8,unchecked(0xB1B0AFBAu-Sum(result)));Directory.CreateDirectory(Path.GetDirectoryName(Path.GetFullPath(output)));File.WriteAllBytes(output,result);
 }

 static void NormalizeMetrics(SortedDictionary<string,byte[]> tables)
 {
  // This template replaces MS Gothic/Mincho. QLIE requests positive cell height and half-cell lfWidth.
  // Those fonts have a 256-unit em, ascent/descent 220/36 and average width 128. Noto's larger
  // Windows line box and full-em average width otherwise shrink and horizontally compress dialogue.
  int em=U16(tables["head"],18),ascent=(em*220+128)/256,descent=em-ascent;
  var os=tables["OS/2"];W16(os,2,(em+1)/2);W16(os,68,ascent);W16(os,70,-descent);W16(os,72,0);W16(os,74,ascent);W16(os,76,descent);
  var hhea=tables["hhea"];W16(hhea,4,ascent);W16(hhea,6,-descent);W16(hhea,8,0);
  // Retain variable outlines/advances; variations must not override the replacement line metrics.
  tables.Remove("MVAR");
 }
}
