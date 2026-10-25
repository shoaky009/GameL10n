using System;
using System.IO;
using System.Text;
using System.Linq;
using System.Collections.Generic;
using System.Text.RegularExpressions;

// QLIE line scripts: keep line endings and every non-text byte unchanged.
public sealed class QlieScript
{
    public sealed record Segment(string Id,string Kind,string Text,int Start,int Length);
    readonly byte[] original;
    readonly string text;
    readonly Encoding encoding;
    public readonly List<Segment> Segments=new();
    static readonly Regex Tags=new(@"\[[^\]\r\n]*\]",RegexOptions.CultureInvariant);
    public QlieScript(byte[] bytes, Encoding codec=null) {
        Encoding.RegisterProvider(CodePagesEncodingProvider.Instance);
        encoding=codec??Encoding.GetEncoding(932,EncoderFallback.ExceptionFallback,DecoderFallback.ExceptionFallback);
        original=(byte[])bytes.Clone(); text=encoding.GetString(bytes);
        int pos=0,line=0;
        while(pos<text.Length){
            line++; int end=text.IndexOf('\n',pos);if(end<0)end=text.Length;
            int stop=end;if(stop>pos&&text[stop-1]=='\r')stop--;
            string s=text.Substring(pos,stop-pos);
            void Add(string kind,int start,int len,int field=0){if(len>0)Segments.Add(new Segment($"L{line:D6}:{kind}:{field}",kind,text.Substring(start,len),start,len));}
            if(s.StartsWith("^select,",StringComparison.Ordinal)){
                int a=8,f=0;while(a<=s.Length){int b=s.IndexOf(',',a);if(b<0)b=s.Length;Add("choice",pos+a,b-a,++f);if(b==s.Length)break;a=b+1;}
            }else if(s.Length>=2&&s[0]=='【'&&s[^1]=='】')Add("name",pos+1,s.Length-2);
            else if(s.Length>0&&!char.IsWhiteSpace(s[0])&&s[0]!='％'&&s[0]!='@'&&s[0]!='^'&&s[0]!='\\')Add("text",pos,s.Length);
            pos=end<text.Length?end+1:end;
        }
    }
    public byte[] Build(IReadOnlyDictionary<string,string> edits,Encoding outputEncoding=null){
        var map=Segments.ToDictionary(x=>x.Id);
        foreach(var pair in edits){
            if(!map.TryGetValue(pair.Key,out var s))throw new InvalidDataException("未知的脚本文本标识："+pair.Key);
            string value=pair.Value??throw new InvalidDataException("译文不能为空值。");
            if(value==s.Text)continue;
            if(value.IndexOfAny(new[]{'\r','\n','\0'})>=0)throw new InvalidDataException("译文不能包含换行或空字符。");
            if(s.Kind=="choice"&&value.Contains(','))throw new InvalidDataException("选项译文不能包含半角逗号。");
            if(s.Kind=="name"&&(value.Contains('【')||value.Contains('】')))throw new InvalidDataException("姓名译文不能包含姓名分隔符。");
            if(!Tags.Matches(s.Text).Select(m=>m.Value).SequenceEqual(Tags.Matches(value).Select(m=>m.Value)))throw new InvalidDataException("请保留译文中的格式控制标记。");
            if(s.Kind=="text"&&(value.Length==0||char.IsWhiteSpace(value[0])||value[0]=='@'||value[0]=='^'||value[0]=='\\'||value[0]=='％'||value[0]=='【'))throw new InvalidDataException("正文译文不能变为控制行。");
        }
        if(!edits.Any(p=>p.Value!=map[p.Key].Text))return (byte[])original.Clone();
        using var result=new MemoryStream();int at=0;
        foreach(var s in Segments){int start=encoding.GetByteCount(text.Substring(0,s.Start)),end=encoding.GetByteCount(text.Substring(0,s.Start+s.Length));result.Write(original,at,start-at);if(edits.TryGetValue(s.Id,out string v)&&v!=s.Text)result.Write((outputEncoding??encoding).GetBytes(v));else result.Write(original,start,end-start);at=end;}
        result.Write(original,at,original.Length-at);return result.ToArray();
    }
}