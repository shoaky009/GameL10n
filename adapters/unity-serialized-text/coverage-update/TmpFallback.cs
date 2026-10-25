using System;using System.IO;using System.Linq;using System.Text;using System.Collections.Generic;
public static class TmpFallback {
 // Uses a shipped dynamic Unicode fallback. No code-page tunnel, font replacement or executable patch.
 public static Dictionary<(string,long),byte[]> Prepare(UnitySet set,IEnumerable<string> changed){
  var needed=new HashSet<int>();foreach(string text in changed){_ = new UTF8Encoding(false,true).GetBytes(text);foreach(var rune in text.EnumerateRunes())if(rune.Value>127&&!Rune.IsControl(rune))needed.Add(rune.Value);}
  var patches=new Dictionary<(string,long),byte[]>();if(needed.Count==0)return patches;
  var settings=new List<(string path,long id,byte[] raw,TmpSettings value)>();var fonts=new List<(string path,long id,byte[] raw,TmpFont value)>();
  foreach(var a in set.Assets)foreach(dynamic inf in a.Value.Infos){if((int)inf.TypeId!=114)continue;byte[] raw=a.Value.Raw(inf);string script=set.Script(a.Key,raw);if(script=="TMPro.TMP_Settings")settings.Add((a.Key,(long)inf.PathId,raw,TmpSettings.Parse(raw)));if(script=="TMPro.TMP_FontAsset"){try{var font=TmpFont.Parse(raw);if(font.Mode==1&&font.Family.Contains("Unifont",StringComparison.OrdinalIgnoreCase))fonts.Add((a.Key,(long)inf.PathId,raw,font));}catch(InvalidDataException){/* other versions are not fallback candidates */}}}
  if(settings.Count==0)throw new InvalidDataException("中文回填需要 TMP_Settings 全局回退配置及其依赖资源");
  foreach(var s in settings){bool found=false;foreach(var f in fonts.OrderBy(f=>f.path==s.path?0:1)){
   int file;if(f.path==s.path)file=0;else{file=0;var ext=((System.Collections.IEnumerable)set.Assets[s.path].Instance.file.Metadata.Externals).Cast<dynamic>().ToArray();for(int i=0;i<ext.Length;i++){try{if(set.Resolve(s.path,i+1)==f.path){file=i+1;break;}}catch(InvalidDataException){}}if(file==0)continue;}
   string source=set.Resolve(f.path,f.value.Source.File);var cmap=new SfntCoverage(set.Raw(source,f.value.Source.Id));if(needed.Any(c=>cmap.Glyph(c)==0))continue;
   var ptr=new UnityPtr(file,f.id);patches[(s.path,s.id)]=s.value.Add(s.raw,ptr);if(!f.value.MultiAtlas){byte[] raw=(byte[])f.raw.Clone();raw[f.value.MultiAtlasOffset]=1;patches[(f.path,f.id)]=raw;}found=true;break;
  }if(!found)throw new InvalidDataException("内嵌动态 Unifont 缺少所需字形或依赖；不支持的扩展汉字/表情不能静默替换。请使用该字体覆盖的文字，或另行适配字体资源。");}
  return patches;
 }
 public static Dictionary<(string,long),byte[]> Plan(UnitySet set,IReadOnlyDictionary<string,string> tr,IEnumerable<string>? extraChanged=null){
  var patches=new Dictionary<(string,long),byte[]>();var known=new HashSet<string>(StringComparer.Ordinal);var changed=new List<string>(extraChanged??Array.Empty<string>());
  void Known(string id,string before){if(!known.Add(id))throw new InvalidDataException("文本 ID 冲突："+id);if(tr.TryGetValue(id,out string? text)&&text!=before){if(text==null)throw new InvalidDataException("译文不得为 null");changed.Add(text);}}
  foreach(var a in set.Assets)foreach(dynamic inf in a.Value.Infos){
   if((int)inf.TypeId==49){DialogueAsset? d=DialogueAsset.Parse((byte[])a.Value.Raw(inf),(long)inf.PathId);if(d==null)continue;foreach(var t in d.Tokens)Known(DialogueAsset.Id(a.Key,d.PathId,t.Key),t.Text);byte[] raw=d.Rebuild(a.Key,tr);if(!raw.AsSpan().SequenceEqual(d.Raw))patches[(a.Key,d.PathId)]=raw;}
   else if((int)inf.TypeId==114){SerializedText? s=SerializedText.Parse(set,a.Key,inf);if(s==null)continue;foreach(var t in s.Tokens)Known(SerializedText.Id(a.Key,s.PathId,t.Field),t.Text);byte[] raw=s.Rebuild(a.Key,tr);if(!raw.AsSpan().SequenceEqual(s.Raw))patches[(a.Key,s.PathId)]=raw;}
  }
  foreach(var id in tr.Keys)if(!known.Contains(id))throw new InvalidDataException("译文引用了不存在的单元："+id);
  foreach(var p in Prepare(set,changed))patches.Add(p.Key,p.Value);return patches;
 }
}
