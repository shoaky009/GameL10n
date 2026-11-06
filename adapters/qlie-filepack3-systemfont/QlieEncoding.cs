using System;
using System.Text;
using System.Collections.Generic;
using System.Linq;
using System.IO;
public sealed class QlieEncoding:Encoding {
 public static readonly Encoding Cp;
 public readonly SortedDictionary<int,int> Aliases;
 readonly Dictionary<int,int> reverse;
 static QlieEncoding(){Encoding.RegisterProvider(CodePagesEncodingProvider.Instance);Cp=Encoding.GetEncoding(932,EncoderFallback.ExceptionFallback,DecoderFallback.ExceptionFallback);}
 public QlieEncoding(IDictionary<int,int> aliases){Aliases=new(aliases);reverse=aliases.ToDictionary(p=>p.Value,p=>p.Key);}
 public void AddText(string text,HashSet<int> reserved){foreach(var r in text.EnumerateRunes()){if(reverse.ContainsKey(r.Value))continue;try{Cp.GetBytes(r.ToString());continue;}catch(EncoderFallbackException){} int slot=Enumerable.Range(0xE000,1880).FirstOrDefault(x=>!Aliases.ContainsKey(x)&&!reserved.Contains(x));if(slot==0)throw new InvalidDataException("新增字符超过可用字库容量（最多1880字），请减少不同字符数量。");Aliases.Add(slot,r.Value);reverse.Add(r.Value,slot);}}
 string ToNative(string s){var b=new StringBuilder();foreach(var r in s.EnumerateRunes())b.Append(reverse.TryGetValue(r.Value,out int p)?char.ConvertFromUtf32(p):r.ToString());return b.ToString();}
 string FromNative(string s){var b=new StringBuilder();foreach(char c in s)b.Append(Aliases.TryGetValue(c,out int u)?char.ConvertFromUtf32(u):c.ToString());return b.ToString();}
 public override int GetByteCount(char[] c,int i,int n)=>Cp.GetByteCount(ToNative(new string(c,i,n)));
 public override int GetBytes(char[] c,int i,int n,byte[] b,int j){var a=Cp.GetBytes(ToNative(new string(c,i,n)));a.CopyTo(b,j);return a.Length;}
 public override int GetCharCount(byte[] b,int i,int n)=>FromNative(Cp.GetString(b,i,n)).Length;
 public override int GetChars(byte[] b,int i,int n,char[] c,int j){var s=FromNative(Cp.GetString(b,i,n));s.CopyTo(0,c,j,s.Length);return s.Length;}
 public override int GetMaxByteCount(int n)=>checked(n*2+2);
 public override int GetMaxCharCount(int n)=>checked(n*2+2);
}
