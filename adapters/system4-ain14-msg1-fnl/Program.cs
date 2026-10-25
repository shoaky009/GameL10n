using System;
using System.IO;
using System.Text;
using System.Text.Json;
using System.Collections.Generic;
using System.Linq;
static class Program {
 static readonly JsonSerializerOptions Opt=new(){PropertyNamingPolicy=JsonNamingPolicy.CamelCase};
 static string Str(JsonElement j,string k,string d="")=>j.TryGetProperty(k,out var v)&&v.ValueKind==JsonValueKind.String?v.GetString()!:d;
 static readonly string[] W={"当前支持剧情消息；部分人名、选项、菜单及图片文字可能仍为日文。","安装后请使用生成的 .zh.exe 启动。新增缺字采用宋体；请先试译确认游戏中的显示和排版。"};
 static void Main(){Encoding.RegisterProvider(CodePagesEncodingProvider.Instance);JsonElement r=default;object result;try{r=JsonDocument.Parse(Console.In.ReadToEnd()).RootElement;result=Run(r);}catch(Exception e){result=new{protocolVersion=1,success=false,error=e.ToString()};}string f=r.ValueKind==JsonValueKind.Object?Str(r,"responseFile"):"";if(f!=""){using var s=File.Create(f);JsonSerializer.Serialize(s,result,Opt);}else Console.WriteLine(JsonSerializer.Serialize(result,Opt));}
 static int Header(string dir,string f){using var s=File.OpenRead(Path.Combine(dir,f));byte[] b=new byte[18];int n=s.Read(b);if(n>=18&&b.AsSpan(0,8).SequenceEqual(new byte[]{65,73,50,0,0,0,0,0})&&BitConverter.ToInt32(b,8)>16&&b[16]==0x78)return 1;if(n>=16&&b.AsSpan(0,4).SequenceEqual("FNA\0"u8)&&BitConverter.ToInt32(b,12)>=4)return 2;return 0;}
 static bool Encodable(char c){try {string s=c.ToString();return Ain.Sjis.GetString(Ain.Sjis.GetBytes(s))==s;}catch(EncoderFallbackException){return false;}}
 static void Save(string root,string path,byte[] b){string full=Path.GetFullPath(Path.Combine(root,path));if(!full.StartsWith(Path.GetFullPath(root)+Path.DirectorySeparatorChar,StringComparison.OrdinalIgnoreCase))throw new Exception("Unsafe resource path");Directory.CreateDirectory(Path.GetDirectoryName(full)!);File.WriteAllBytes(full,b);}
 static object Run(JsonElement r){string dir=Str(r,"sourceDirectory"),op=Str(r,"operation");var files=r.TryGetProperty("resourceFiles",out var fs)&&fs.ValueKind==JsonValueKind.Array?fs.EnumerateArray().Select(x=>x.GetString()!).ToArray():Directory.GetFiles(dir,"*",SearchOption.AllDirectories).Select(x=>Path.GetRelativePath(dir,x)).ToArray();
 var types=files.Where(f=>f.EndsWith(".ain",StringComparison.OrdinalIgnoreCase)||f.EndsWith(".fnl",StringComparison.OrdinalIgnoreCase)).Select(f=>(path:f,type:Header(dir,f))).Where(x=>x.type!=0).ToArray();
 var executables=files.Where(f=>f.EndsWith(".exe",StringComparison.OrdinalIgnoreCase)&&!f.EndsWith(".zh.exe",StringComparison.OrdinalIgnoreCase)).ToArray();
 var runtimes=executables.Where(f=>RuntimePatch.MatchesHeader(Path.Combine(dir,f))).ToArray();
 string? runtime=runtimes.Length==1?runtimes[0]:null;
 if(op=="probe")return new{protocolVersion=1,success=true,probe=types.Any(x=>x.type==1)&&types.Any(x=>x.type==2)&&runtime!=null?(object)new{engineId="system4-ain",adapterId="system4-ain14-msg1-fnl",adapterVersion="1.2.0",description="System4 AIN v14 剧情消息与 FNL 中文字库",requiredFiles=types.Select(x=>x.path).Append(runtime!).ToArray(),warnings=W,originalExecutable=runtime}:null};
 if(runtime==null)throw new Exception("没有匹配的 System4 运行程序修补规则，请继续智能适配分析当前版本。");
 byte[] originalExe=File.ReadAllBytes(Path.Combine(dir,runtime));RuntimePatch.RequireOriginal(originalExe);
 var fonts=types.Where(x=>x.type==2).ToDictionary(x=>x.path,x=>new Fnl(File.ReadAllBytes(Path.Combine(dir,x.path))));if(fonts.Count==0)throw new Exception("FNA FNL font is required");
 var old=fonts.First().Value.Map;foreach(var font in fonts.Values)if(font.Map.Count!=old.Count||old.Any(k=>!font.Map.TryGetValue(k.Key,out var v)||v!=k.Value))throw new Exception("FNL maps disagree");
 var encodingMap=new Dictionary<char,char>(old);var scripts=types.Where(x=>x.type==1).ToDictionary(x=>x.path,x=>new Ain(File.ReadAllBytes(Path.Combine(dir,x.path)),encodingMap));if(scripts.Count==0)throw new Exception("No supported AIN");
 var translations=new Dictionary<string,string>();if(op=="build"){string tf=Str(r,"translationsFile");JsonElement t=tf!=""?JsonDocument.Parse(File.ReadAllText(tf)).RootElement:r.TryGetProperty("translations",out var tr)?tr:default;if(t.ValueKind==JsonValueKind.Object)foreach(var p in t.EnumerateObject())translations.Add(p.Name,p.Value.GetString()!);}
 var units=new List<object>();var changes=new Dictionary<string,Dictionary<int,string>>();var needed=new SortedSet<char>();var reserved=new HashSet<char>(old.Values);
 foreach(var a in scripts.Values){foreach(var text in a.Texts)foreach(char c in text)if(c>=0xe000&&c<=0xe757)reserved.Add(c);
 // STR0 is not exposed for translation; reserve its existing raw CP932 private-use references.
 byte[] d=a.Data;for(int p=a.End;p<d.Length-8;p++){if(!d.AsSpan(p,4).SequenceEqual("STR0"u8))continue;int count=BitConverter.ToInt32(d,p+4),q=p+8;if(count<0||count>d.Length-q)continue;for(int j=0;j<count&&q<d.Length;j++){int end=Array.IndexOf(d,(byte)0,q);if(end<0)break;try{foreach(char c in Ain.Sjis.GetString(d,q,end-q))if(c>=0xe000&&c<=0xe757)reserved.Add(c);}catch(DecoderFallbackException){}q=end+1;}break;}}
 foreach(var pair in scripts){string f=pair.Key;var a=pair.Value;var edits=new Dictionary<int,string>();changes.Add(f,edits);for(int i=0;i<a.Texts.Count;i++){string id=f.Replace('\\','/')+"#msg1:"+i,text=a.Texts[i];if(!string.IsNullOrWhiteSpace(text))units.Add(new{id,resourcePath=f,locator="MSG1["+i+"]",sourceText=text,context="AIN v14 message table",speaker=(string?)null,kind="dialogue"});if(!translations.TryGetValue(id,out var t)||t==text)continue;edits.Add(i,t);foreach(char c in t){if(char.IsSurrogate(c)||c==0||(char.IsControl(c)&&c!='\n'&&c!='\r'&&c!='\t'))throw new Exception("Only BMP text without NUL/invalid controls is supported");if(c>=0xe000&&c<=0xe757)reserved.Add(c);if(!old.ContainsKey(c)&&!Encodable(c))needed.Add(c);}}}
 if(op=="build"){var desired=new Dictionary<char,char>(old);foreach(char c in needed){char slot='\0';for(int k=0xe000;k<=0xe757;k++){char p=(char)k;if(reserved.Contains(p)||fonts.Values.Any(font=>font.Fonts.SelectMany(x=>x).Any(face=>Fnl.Index(p)<face.Glyphs.Count&&face.Glyphs[Fnl.Index(p)].Size>0)))continue;slot=p;break;}if(slot==0)throw new Exception("No free FNL/CP932 private-use slot remains");desired.Add(c,slot);reserved.Add(slot);encodingMap.Add(c,slot);}
 string output=Str(r,"outputDirectory");var outputs=new List<string>();foreach(var a in scripts){Save(output,a.Key,a.Value.Build(changes[a.Key]));outputs.Add(a.Key);}if(needed.Count>0){using var raster=new GlyphRasterizer();foreach(var f in fonts){Save(output,f.Key,f.Value.Build(desired,raster));outputs.Add(f.Key);}}else foreach(var f in fonts){Save(output,f.Key,f.Value.Original);outputs.Add(f.Key);}foreach(var exe in executables){Save(output,exe,File.ReadAllBytes(Path.Combine(dir,exe)));outputs.Add(exe);}
 string? launch=null;if(desired.Count>0){launch=Path.ChangeExtension(runtime,"zh.exe");Save(output,launch,RuntimePatch.Build(originalExe));outputs.Add(launch);}
 return new{protocolVersion=1,success=true,build=new{files=outputs,warnings=W,launchExecutable=launch}};}
 if(op=="extract")return new{protocolVersion=1,success=true,extraction=new{units,warnings=W}};
 if(op=="validate"){
 string output=Str(r,"outputDirectory");var builtFonts=fonts.Keys.Select(f=>new Fnl(File.ReadAllBytes(Path.Combine(output,f)))).ToArray();
 var map=builtFonts[0].Map;if(builtFonts.Any(f=>f.Map.Count!=map.Count||map.Any(e=>!f.Map.TryGetValue(e.Key,out var slot)||slot!=e.Value)))throw new Exception("补丁字库的中文映射不一致。");
 foreach(var font in builtFonts)foreach(var face in font.Fonts.SelectMany(f=>f))foreach(var entry in map)if(!font.Bitmap(face,Fnl.Index(entry.Value)).Any(b=>b!=0))throw new Exception("补丁字库存在空的中文字形。");
 if(map.Count>0&&!File.ReadAllBytes(Path.Combine(output,Path.ChangeExtension(runtime,"zh.exe"))).AsSpan().SequenceEqual(RuntimePatch.Build(originalExe)))throw new Exception("中文启动程序与资源使用的码位不匹配。");
 foreach(var a in scripts.Keys)_=new Ain(File.ReadAllBytes(Path.Combine(output,a)),map);
 return new{protocolVersion=1,success=true,validation=new{passed=true,issues=Array.Empty<object>(),runtimeStatus="not-tested"}};}
 throw new Exception("Unsupported operation");
 }
}
