using System;
using System.IO;
using System.Text;
using System.Text.Json;
using System.Linq;
using System.Collections.Generic;
using System.Security.Cryptography;
public static class QlieCli {
 static readonly JsonSerializerOptions Json=new(){PropertyNamingPolicy=JsonNamingPolicy.CamelCase,WriteIndented=false};
 const string MapPath="qlie-l10n/map.json";
 const string LaunchPath="启动汉化.exe";
 static string Norm(string p)=>p.Replace('\\','/');
 static string Full(string root,string path){string r=Path.GetFullPath(root)+Path.DirectorySeparatorChar,p=Path.GetFullPath(Path.Combine(root,Norm(path)));if(!p.StartsWith(r,StringComparison.OrdinalIgnoreCase))throw new InvalidDataException("资源路径越界。");return p;}
 static string Str(JsonElement r,string p,string def=null)=>r.TryGetProperty(p,out var v)&&v.ValueKind==JsonValueKind.String?v.GetString():def;
 static string Id(string pack,string entry,string seg)=>Norm(pack)+"|"+Norm(entry)+"|"+seg;
 sealed record Item(string Pack,QlieArchive Arc,QlieArchive.Entry Entry,QlieScript Script);
 public static int Main(){JsonElement req=default;try{using var doc=JsonDocument.Parse(Console.In.ReadToEnd());req=doc.RootElement.Clone();var result=Execute(req);Respond(req,result);return 0;}catch(Exception e){Respond(req,new{protocolVersion=1,success=false,error="QLIE 适配失败："+e.Message});return 1;}}
 static void Respond(JsonElement r,object value){string s=JsonSerializer.Serialize(value,Json);string p=r.ValueKind==JsonValueKind.Object?Str(r,"responseFile"):null;if(p!=null)File.WriteAllText(p,s,new UTF8Encoding(false));else Console.WriteLine(s);}
 static object Execute(JsonElement r){string root=Str(r,"sourceDirectory"),op=Str(r,"operation");var files=r.TryGetProperty("resourceFiles",out var rf)?rf.EnumerateArray().Select(v=>Norm(v.GetString())).ToArray():Directory.GetFiles(root,"*",SearchOption.AllDirectories).Select(p=>Norm(Path.GetRelativePath(root,p))).ToArray();
 // QLIE's primary scenario pack is data0.pack; large graphics/voice packs are not needed.
 var packs=files.Where(p=>Path.GetFileName(p).Equals("data0.pack",StringComparison.OrdinalIgnoreCase)).ToArray();
 var keys=files.Where(p=>p.EndsWith(".fkey",StringComparison.OrdinalIgnoreCase)).ToArray();var exes=files.Where(p=>p.EndsWith(".exe",StringComparison.OrdinalIgnoreCase)&&!p.Equals(LaunchPath,StringComparison.OrdinalIgnoreCase)).ToArray();var configs=files.Where(p=>Path.GetFileName(p).Equals("config.txt",StringComparison.OrdinalIgnoreCase)).ToArray();
 if(op=="probe"){bool Header(string p,byte[] magic){using var f=File.OpenRead(Full(root,p));byte[] b=new byte[magic.Length];return f.Read(b,0,b.Length)==b.Length&&b.SequenceEqual(magic);}exes=exes.Where(p=>Header(p,new byte[]{77,90})).ToArray();bool match=packs.Length==1&&keys.Length==1&&exes.Length>0&&configs.Length==1&&Header(keys[0],Encoding.ASCII.GetBytes("KeyFile ver1.0"));var required=packs.Concat(keys).Concat(exes).Concat(configs).Concat(files.Where(p=>p.EndsWith(".exe.le.config",StringComparison.OrdinalIgnoreCase))).Distinct().ToArray();return new{protocolVersion=1,success=true,probe=match?(object)new{engineId="qlie-filepack3",adapterId="qlie-filepack3-systemfont",adapterVersion="1.0.0",description="QLIE FilePackVer3.0 主脚本封包／system_fix 字体布局（完整资源阶段严格校验）",requiredFiles=required,warnings=new[]{"仅识别主脚本 data0.pack；不改动图像和语音封包。"},originalExecutable=(string)null}:null};}
 if(packs.Length!=1||keys.Length!=1||configs.Length!=1)throw new InvalidDataException("需要一个主脚本封包、一个 fkey 和一个外置 config.txt。");
 string exe=null;byte[] gameKey=null;foreach(string p in exes){byte[] k;try{k=QlieKey.FromExe(File.ReadAllBytes(Full(root,p)));}catch(InvalidDataException){continue;}if(exe!=null)throw new InvalidDataException("找到多个 QLIE 主程序，无法确定启动目标。");exe=p;gameKey=k;}if(gameKey==null)throw new InvalidDataException("未找到含 QLIE 密钥资源的主程序。");
 var aliases=files.Contains(MapPath)?JsonSerializer.Deserialize<SortedDictionary<int,int>>(File.ReadAllText(Full(root,MapPath))):new();var codec=new QlieEncoding(aliases);var arc=new QlieArchive(File.ReadAllBytes(Full(root,packs[0])),File.ReadAllBytes(Full(root,keys[0])),gameKey);
 var items=new List<Item>();var reserved=new HashSet<int>();foreach(var e in arc.Entries.Where(e=>e.Name.EndsWith(".s",StringComparison.OrdinalIgnoreCase))){byte[] b=arc.Read(e);foreach(char c in QlieEncoding.Cp.GetString(b))if(c>=0xE000&&c<=0xE757&&!aliases.ContainsKey(c))reserved.Add(c);items.Add(new(packs[0],arc,e,new QlieScript(b,codec)));}
 if(items.Count==0)throw new InvalidDataException("主封包中没有可解析的 .s 脚本。");
 var fontEntry=arc.Entries.SingleOrDefault(e=>Norm(e.Name).Equals("system/font/fontdata.dat",StringComparison.OrdinalIgnoreCase));if(fontEntry==null)throw new InvalidDataException("缺少系统字体布局定义。");string layout=QlieEncoding.Cp.GetString(arc.Read(fontEntry));for(int i=0;i<8;i++){string values=i==5?"外字登録":"(?:system_fix|system)";if(!System.Text.RegularExpressions.Regex.IsMatch(layout,$@"(?m)^Font\[{i}\]\.List\[0\]={values}\r?$"))throw new InvalidDataException("字体布局与已支持的系统字体／独立外字方案不符。");}
 if(op=="extract"){var units=new List<object>();foreach(var it in items){string speaker=null;foreach(var s in it.Script.Segments){if(s.Kind=="name")speaker=s.Text;units.Add(new{id=Id(it.Pack,it.Entry.Name,s.Id),resourcePath=it.Pack,locator=Norm(it.Entry.Name)+":"+s.Id,sourceText=s.Text,context=Norm(it.Entry.Name),speaker=(string)null,kind=s.Kind=="text"?"dialogue":s.Kind});}}return new{protocolVersion=1,success=true,extraction=new{units,warnings=Array.Empty<string>()}};}
 if(op=="validate")return new{protocolVersion=1,success=true,validation=new{passed=true,issues=Array.Empty<object>(),runtimeStatus="not-tested"}};
 if(op!="build")throw new InvalidDataException("不支持的操作。");
 Dictionary<string,string> edits=new();string tf=Str(r,"translationsFile");if(tf!=null)edits=JsonSerializer.Deserialize<Dictionary<string,string>>(File.ReadAllText(tf));else if(r.TryGetProperty("translations",out var ts)&&ts.ValueKind==JsonValueKind.Object)edits=JsonSerializer.Deserialize<Dictionary<string,string>>(ts.GetRawText());
 var known=items.SelectMany(it=>it.Script.Segments.Select(s=>Id(it.Pack,it.Entry.Name,s.Id))).ToHashSet();foreach(var id in edits.Keys)if(!known.Contains(id))throw new InvalidDataException("译文含未知文本标识："+id);
 bool changed=items.Any(it=>it.Script.Segments.Any(s=>edits.TryGetValue(Id(it.Pack,it.Entry.Name,s.Id),out string v)&&v!=s.Text));
 string output=Str(r,"outputDirectory");Directory.CreateDirectory(output);var outputs=new HashSet<string>();void Put(string p,byte[] b){string f=Full(output,p);Directory.CreateDirectory(Path.GetDirectoryName(f));File.WriteAllBytes(f,b);outputs.Add(p);}foreach(string p in files)Put(p,File.ReadAllBytes(Full(root,p)));
 if(changed){foreach(var it in items)foreach(var s in it.Script.Segments)if(edits.TryGetValue(Id(it.Pack,it.Entry.Name,s.Id),out var v)&&v!=s.Text){if(v==null)throw new InvalidDataException("译文不能为空值。");codec.AddText(v,reserved);}
 var replacements=new Dictionary<string,byte[]>();foreach(var it in items){var local=it.Script.Segments.Where(s=>edits.ContainsKey(Id(it.Pack,it.Entry.Name,s.Id))).ToDictionary(s=>s.Id,s=>edits[Id(it.Pack,it.Entry.Name,s.Id)]);replacements[it.Entry.Name]=it.Script.Build(local,codec);}
 // QLIE filters system_fix by LOGFONT pitch; Noto is proportional. Keep slot 5's separate bitmap font.
 replacements[fontEntry.Name]=QlieEncoding.Cp.GetBytes(System.Text.RegularExpressions.Regex.Replace(layout,@"(?m)^(Font\[[0-46-7]\]\.List\[0\]=)system_fix(?=\r?$)","${1}system"));Put(packs[0],arc.Build(replacements));
 // Include the font metadata revision so an older running patch cannot supply a different face under the same name.
 string fingerprint=Convert.ToHexString(SHA256.HashData(Encoding.UTF8.GetBytes("qlie-font-v3\n"+JsonSerializer.Serialize(codec.Aliases))))[..8];string sans="QLie Han Sans "+fingerprint,serif="QLie Han Serif "+fingerprint;
 foreach(var pair in new[]{("Sans",sans),("Serif",serif)}){string p="qlie-l10n/"+pair.Item1+".ttf";QlieFont.Create(Path.Combine(AppContext.BaseDirectory,"tools","Noto"+pair.Item1+"SC-VF.ttf"),Full(output,p),pair.Item2,codec.Aliases);outputs.Add(p);}
 // The engine stores ENUMLOGFONTEXA.elfFullName, including the variable font instance's Regular suffix.
 string cfg=QlieEncoding.Cp.GetString(File.ReadAllBytes(Full(root,configs[0])));cfg=System.Text.RegularExpressions.Regex.Replace(cfg,@"(?m)^(Font\[(\d+)\]\.Name=)([^\r\n]*)",m=>{int n=int.Parse(m.Groups[2].Value);string f=m.Groups[3].Value;if(n==5){if(f!="外字登録")throw new InvalidDataException("字体槽5不是预期的独立外字字体。");return m.Value;}if(f.StartsWith("QLie Han Sans ")||f=="ＭＳ ゴシック")return m.Groups[1].Value+sans+" Regular";if(f.StartsWith("QLie Han Serif ")||f=="ＭＳ 明朝")return m.Groups[1].Value+serif+" Regular";throw new InvalidDataException("遇到尚未支持的字体风格："+f);});Put(configs[0],QlieEncoding.Cp.GetBytes(cfg));Put(MapPath,Encoding.UTF8.GetBytes(JsonSerializer.Serialize(codec.Aliases)));
 Put(LaunchPath,File.ReadAllBytes(Path.Combine(AppContext.BaseDirectory,"tools","QlieLauncher.exe")));
 var launchConfig=new System.Xml.Linq.XElement("launch",new System.Xml.Linq.XAttribute("executable",exe),new System.Xml.Linq.XElement("font",new System.Xml.Linq.XAttribute("file","qlie-l10n/Sans.ttf"),new System.Xml.Linq.XAttribute("face",sans+" Regular")),new System.Xml.Linq.XElement("font",new System.Xml.Linq.XAttribute("file","qlie-l10n/Serif.ttf"),new System.Xml.Linq.XAttribute("face",serif+" Regular")));
 Put("qlie-l10n/launch.xml",Encoding.UTF8.GetBytes(launchConfig.ToString()));
 foreach(string p in new[]{"NotoSans-LICENSE.txt","NotoSerif-LICENSE.txt","FONT-DERIVATION.txt"})Put("qlie-l10n/"+p,File.ReadAllBytes(Path.Combine(AppContext.BaseDirectory,"tools",p)));
 Put("qlie-l10n/使用说明.txt",Encoding.UTF8.GetBytes("安装补丁后双击游戏目录中的 启动汉化.exe。启动器临时加载汉化字体，并通过本机 Locale Emulator 运行原游戏 EXE，沿用原 EXE 的日语配置；已使用系统日语环境且没有单独转区配置时可直接运行。未找到 Locale Emulator 时会请您选择 LEProc.exe。请保留原游戏的 .exe.le.config；无需对启动器本身设置转区。原 EXE、视频和其他未翻译资源保留。字体仅在游戏运行期间加载，不永久安装。实际游戏内显示仍需试译确认。"));}
 return new{protocolVersion=1,success=true,build=new{files=outputs.OrderBy(p=>p).ToArray(),warnings=changed?new[]{"安装后请双击 启动汉化.exe；启动器会加载汉化字体并沿用原游戏的日语运行配置。游戏内显示尚待确认。"}:Array.Empty<string>(),launchExecutable=changed?LaunchPath:null}};
 }
}
