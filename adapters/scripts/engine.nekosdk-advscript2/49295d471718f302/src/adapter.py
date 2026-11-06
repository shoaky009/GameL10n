import sys,json,struct,base64,os,zlib
from pathlib import Path
from python.archives.nekopack import open_pak,read_member,build_pak,xor_head

MAG=b'NEKOSDK_ADVSCRIPT2\0'
def nodes(data):
 if not data.startswith(MAG): raise ValueError('脚本签名不匹配')
 count=struct.unpack_from('<I',data,19)[0]; p=23
 for i in range(count):
  if p+212>len(data): raise ValueError('节点头截断')
  op=struct.unpack_from('<I',data,p+12)[0]; p+=212; fields=[]
  for j in range(33):
   start=p; z=struct.unpack_from('<I',data,p)[0];p+=4
   if z<1 or p+z>len(data) or data[p+z-1]!=0: raise ValueError('节点字符串无效')
   fields.append((start,p,p+z-1));p+=z
  yield i,op,fields
 if p!=len(data): raise ValueError('脚本存在未解析尾部')
def encodable(ch):
 try: ch.encode('cp932'); return True
 except UnicodeEncodeError: return False
def tunnel_decode(raw, mapping):
 out=[]; i=0
 while i<len(raw):
  b=raw[i]
  if (0x81<=b<=0x9f or 0xe0<=b<=0xfc) and i+1<len(raw):
   pair=raw[i:i+2]; idx=slot_index(pair)
   if idx is not None and idx<len(mapping) and mapping[idx]!='\0': out.append(mapping[idx])
   else: out.append(pair.decode('cp932'))
   i+=2
  else: out.append(bytes([b]).decode('cp932')); i+=1
 return ''.join(out)
def slot_index(pair):
 hi,lo=pair
 lows=[x for x in range(1,64) if x not in (9,10,13,32,44)]
 if lo not in lows or not (0x81<=hi<=0x9f or 0xe0<=hi<=0xfc): return None
 return (hi-0x81 if hi<=0x9f else 31+hi-0xe0)*len(lows)+lows.index(lo)
def locate(d, mapping=''):

 for i,op,fs in nodes(d):
  if op==5:
   for j in (1,2):
    a,b,c=fs[j]; raw=d[b:c]
    if raw: yield i,j,a,c,tunnel_decode(raw,mapping)
def index_head(data,total):
 if len(data)<14 or data[:10]!=b'NEKOPACK4A': return []
 length=struct.unpack_from('<I',data,10)[0]; p=14; end=p+length
 if end>len(data): return []
 result=[]
 while p<end:
  n=struct.unpack_from('<I',data,p)[0];p+=4
  if not n: break
  if n>1024 or p+n+8>end: return []
  name=data[p:p+n];p+=n
  if name[-1]!=0:return []
  checksum=sum(x-256 if x>127 else x for x in name) & 0xffffffff; off,size=struct.unpack_from('<II',data,p);p+=8;off^=checksum;size^=checksum
  if off+size>total:return []
  result.append((name[:-1].decode('cp932'),off,size))
 return result

def main(r):
 op=r['operation']; src=Path(r['sourceDirectory']); files=r.get('resourceFiles') or []
 if op=='probe':
  hits=[]; requests=[]
  ranges={(x['path'],x['offset'],x['length']):x for x in r.get('probeRanges',[])}
  for f in r.get('probeFiles',[]):
   path=f['path']; p=src/path; data=p.read_bytes(); entries=index_head(data,f['length'])
   candidates=[e for e in entries if e[0].lower().endswith('.txt')]
   if not candidates: continue
   name,off,size=candidates[0]; length=min(size,1024)
   if length<8: continue
   if off+length<=len(data): part=data[off:off+length]
   elif (path,off,length) in ranges: part=Path(ranges[(path,off,length)]['dataPath']).read_bytes()
   else:
    requests.append({'path':path,'offset':off,'length':length}); continue
   try:
    decoded=xor_head(part,size)+part[4:]
    plain=zlib.decompressobj().decompress(decoded[:-4] if length==size else decoded,64)
    if plain.startswith(MAG): hits.append(path)
   except (ValueError,zlib.error): pass
  if requests: return {'probeReads':requests}
  if not hits:return {'probe':None}
  return {'probe':{'engineId':'nekosdk','adapterId':'nekosdk-advscript2','adapterVersion':'1','description':'NEKOSDK ADVSCRIPT2 / NEKOPACK4A','requiredFiles':hits,'warnings':['中文显示依赖系统字体及其回退字形，覆盖范围尚未验证；原始启动环境和游戏内显示也尚未验证。'],'originalExecutable':None}}
 mapping_path=src/'sjis_ext.bin'
 mapping=mapping_path.read_bytes().decode('utf-16le') if mapping_path.exists() else ''
 units=[]; archives=[]
 for path in files:
  raw=(src/path).read_bytes(); arc=open_pak(raw); items=[]
  for ent in arc.entries:
   data=read_member(raw,ent); items.append((ent.name,data))
   if data.startswith(MAG):
    for i,j,a,c,text in locate(data,mapping):
     key=f'{path}|{ent.name}|{i}|{j}'
     units.append({'id':key,'resourcePath':path,'locator':f'{ent.name}:{i}:{j}','sourceText':text,'context':ent.name,'speaker':None,'kind':'name' if j==1 else 'dialogue'})
  archives.append((path,raw,items))
 if op=='extract':return {'extraction':{'units':units,'warnings':[]}}
 if op=='validate':return {'validation':{'passed':True,'issues':[],'runtimeStatus':'not-tested'}}
 if op=='build':
  tr=r.get('translations') or {}
  if r.get('translationsFile'): tr=json.loads(Path(r['translationsFile']).read_text(encoding='utf-8-sig'))
  out=Path(r['outputDirectory']); outputs=[]
  # Allocate package-wide; printable proxy trails only, excluding reserved comma.
  # The runtime lead-byte helper skips the second byte during text traversal.
  chars=sorted({ch for u in units for ch in tr.get(u['id'],u['sourceText']) if not encodable(ch)})
  slots=[bytes([hi,lo]) for hi in list(range(0x81,0xa0))+list(range(0xe0,0xf0)) for lo in range(0x21,0x40) if lo != 0x2c]
  if any(ord(ch)>0xffff for ch in chars): raise ValueError('代理映射不支持辅助平面字符')
  if len(chars)>len(slots): raise ValueError('可用代理码位不足（最多1410个不同字符）')
  charmap=dict(zip(chars,slots)); table=['\0']*(max([slot_index(x) for x in charmap.values()],default=-1)+1)
  for ch,pair in charmap.items(): table[slot_index(pair)]=ch

  for path,raw,items in archives:
   changed=False; new={}
   for name,data in items:
    edits=[]
    if data.startswith(MAG):
     for i,j,a,c,text in locate(data,mapping):
      key=f'{path}|{name}|{i}|{j}'; t=tr.get(key,text)
      if t!=text:
       b=b''.join(charmap.get(ch) or ch.encode('cp932') for ch in t);
       if b'\0' in b: raise ValueError('文本包含空字符')
       edits.append((a,c,struct.pack('<I',len(b)+1)+b+b'\0'))
    for a,c,b in reversed(edits):data=data[:a]+b+data[c+1:]
    changed|=bool(edits);new[name]=data
   target=out/path;target.parent.mkdir(parents=True,exist_ok=True);target.write_bytes(build_pak(new) if changed else raw);outputs.append(path)
  if charmap:
   import shutil
   (out/'sjis_ext.bin').write_bytes(''.join(table).encode('utf-16le')); outputs.append('sjis_ext.bin')
   shutil.copyfile(Path(__file__).with_name('winmm.dll'),out/'winmm.dll'); outputs.append('winmm.dll')
   shutil.copyfile(Path(__file__).with_name('VNTranslationTools-LICENSE.txt'),out/'VNTranslationTools-LICENSE.txt'); outputs.append('VNTranslationTools-LICENSE.txt')
  return {'build':{'files':outputs,'warnings':['代理字体、系统区域设置和游戏内显示尚未验证；当前最多支持1410个非CP932基本多文种平面字符'] if charmap else [],'launchExecutable':None}}
 raise ValueError('未知操作')
try:
 r=json.load(sys.stdin); result={'protocolVersion':1,'success':True,'error':None}; result.update(main(r))
except Exception as e:
 result={'protocolVersion':1,'success':False,'error':'适配器处理失败：'+str(e)}
if r.get('responseFile'): Path(r['responseFile']).write_text(json.dumps(result,ensure_ascii=False),encoding='utf-8')
else: print(json.dumps(result,ensure_ascii=False))
