"""Chinese glyphs through KAGEX's existing AddTrueTypeFont path; no launcher."""
import struct,re,json
from pathlib import Path
from python.engines.kirikiri_simple_crypt import unpack

class Cmap:
 def __init__(self,path):
  self.data=Path(path).read_bytes();d=self.data
  tables={}
  for i in range(struct.unpack_from('>H',d,4)[0]):
   tag,check,offset,size=struct.unpack_from('>4sIII',d,12+16*i);tables[tag]=(offset,size)
  base,size=tables[b'cmap'];self.sub=[]
  for i in range(struct.unpack_from('>H',d,base+2)[0]):
   platform,encoding,off=struct.unpack_from('>HHI',d,base+4+8*i);p=base+off;fmt=struct.unpack_from('>H',d,p)[0]
   if platform in (0,3) and fmt in (4,12):self.sub.append((fmt,p))
  if not self.sub:raise ValueError('补充字体缺少 Unicode cmap')
 def glyph(self,cp):
  d=self.data
  for fmt,p in self.sub:
   if fmt==12:
    n=struct.unpack_from('>I',d,p+12)[0];lo=0;hi=n
    while lo<hi:
     mid=(lo+hi)//2;start,end,g=struct.unpack_from('>III',d,p+16+12*mid)
     if cp<start:hi=mid
     elif cp>end:lo=mid+1
     else:return g+cp-start
   elif cp<=65535:
    n=struct.unpack_from('>H',d,p+6)[0]//2;end=p+14;start=end+2*n+2;delta=start+2*n;ranges=delta+2*n
    for i in range(n):
     e=struct.unpack_from('>H',d,end+2*i)[0]
     if cp>e:continue
     s=struct.unpack_from('>H',d,start+2*i)[0]
     if cp<s:break
     dv=struct.unpack_from('>h',d,delta+2*i)[0];ro=struct.unpack_from('>H',d,ranges+2*i)[0]
     if not ro:return (cp+dv)&65535
     g=struct.unpack_from('>H',d,ranges+2*i+ro+2*(cp-s))[0]
     return ((g+dv)&65535) if g else 0
  return 0

def font_assets():
 base=Path(__file__).resolve().parent/'fonts'
 return base,{w:Cmap(base/f'SourceHanSansSC-{w}.otf') for w in ('Regular','Bold')}

def embed_chinese(archives,replacements,units,translations):
 base,fonts=font_assets()
 # Both weights must cover every preserved/output logical character, not only edited rows.
 chars={c for u in units for c in translations.get(u['id'],u['sourceText']) if ord(c)>=32 and not c.isspace()}
 for weight,font in fonts.items():
  missing=sorted(ord(c) for c in chars if not font.glyph(ord(c)))
  if missing:raise ValueError('中文字体缺少输出字形（'+weight+'）: '+', '.join(f'U+{cp:04X}' for cp in missing[:16]))
 count=0
 for path,archive in archives:
  def entry(name):
   found=[e for e in archive.entries if archive.name(e).lower()==name.lower()]
   if len(found)!=1:raise ValueError('无法唯一定位嵌入字体成员: '+name)
   return found[0]
  found=[e for e in archive.entries if archive.name(e).lower()=='embfontlist.tjs']
  if not found:continue
  if len(found)!=1:raise ValueError('嵌入字体表不唯一')
  e=found[0];raw=archive.data[e['alias']]
  raw=unpack(raw)[0] if raw[:2]==b'\xfe\xfe' else raw
  if raw[:2] not in (b'\xff\xfe',b'\xfe\xff'):raise ValueError('嵌入字体表并非已验证的 Unicode 脚本')
  s=raw.decode('utf-16')
  changes=[
   (r'("file"\s*=>\s*)"Kosugi-Regular.ttf"',r'\1"SourceHanSansJP-Regular.otf"'),
   (r'("face"\s*=>\s*)"MotoyaLCedar"',r'\1"Source Han Sans SC"'),
   (r'("face"\s*=>\s*)"源ノ角ゴシック JP Regular"',r'\1"Source Han Sans SC"'),
   (r'("face"\s*=>\s*)"源ノ角ゴシック JP Bold"',r'\1"Source Han Sans SC Bold"'),
  ]
  for pattern,new in changes:
   s,n=re.subn(pattern,new,s)
   if n!=1:raise ValueError('嵌入字体声明与已验证布局不符: '+pattern)
  # Inline %f uses the same registered name map. Keep the protected command unchanged.
  marker='// VLゴシック'
  if s.count(marker)!=1:raise ValueError('无法安全追加内联字体别名')
  extra='(const)%[ ], (const)[ (const)%[ "name"=>"ＭＳ ゴシック", "file"=>"SourceHanSansJP-Regular.otf", "face"=>"Source Han Sans SC", "noentry"=>true ] ],\n\t'
  s=s.replace(marker,extra+marker)
  replacements[path][e['alias']]=b'\xff\xfe'+s.encode('utf-16le')
  for original,weight in [('SourceHanSansJP-Regular.otf','Regular'),('SourceHanSansJP-Bold.otf','Bold')]:
   e=entry(original);replacements[path][e['alias']]=fonts[weight].data
  count+=1
 if not count:raise ValueError('未找到已验证的 KAGEX 嵌入字体机制，不能生成中文补丁')
 return ['已通过现有嵌入机制接入思源黑体简体常规体/粗体；保留字体大小，正文常规体字形与原小杉黑体略有差异。实际游戏选字与排版仍待验证。']
