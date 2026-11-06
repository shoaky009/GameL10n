import sys,json,struct,zlib,io,tempfile,subprocess,shutil,re,traceback
from pathlib import Path
from gamel10n_host import parser
from hx_container import Archive
from psb_text import SceneText,tokens
from font_support import embed_chinese
from python.archives.kirikiri_hxv4 import MAGIC,read_index
from python.archives.kirikiri_hxv4_static import candidates
from python.engines.kirikiri_simple_crypt import unpack

WARN=['已覆盖 PSB/SCN 剧情、显示姓名、语音与搜索文本以及内置 KS 解析器识别的文本；未覆盖编译 TJS 中的界面字串、CSV 列表和独立帮助表。','图片/视频文字不提取；启动、原有转区路径、字体选择及中文排版尚待游戏内验证。']
KEY='kirikiri-hxv4-kagex'
def result(key,data):return {'protocolVersion':1,'success':True,key:data}
def read_at(q,path,offset,count):
 p=Path(q['sourceDirectory'])/path
 for r in q.get('probeRanges',[]):
  if r['path']==path and r['offset']<=offset and offset+count<=r['offset']+r['length']:
   with open(r['dataPath'],'rb') as s:s.seek(offset-r['offset']);return s.read(count)
 with p.open('rb') as s:s.seek(offset);data=s.read(count)
 return data if len(data)==count else None

def probe(q):
 requests=[];archives=[];exes=[];allfiles={x['path']:x for x in q.get('probeFiles',[])}
 def need(path,at,n):
  if n<=0 or n>1048576:raise ValueError('探测范围超出限制')
  d=read_at(q,path,at,n)
  if d is None:requests.append({'path':path,'offset':at,'length':n})
  return d
 for path,info in allfiles.items():
  head=(Path(q['sourceDirectory'])/path).read_bytes()
  if head.startswith(MAGIC):
   at=struct.unpack_from('<Q',head,11)[0]
   if at+17<=len(head) and head[at]==128:at=struct.unpack_from('<Q',head,at+9)[0]
   h=need(path,at,17)
   if h is None:continue
   flag=h[0]
   if flag not in (0,1):continue
   packed=struct.unpack_from('<Q',h,1)[0]
   if packed>1048000:continue
   b=need(path,at+(17 if flag else 9),packed)
   if b is None:continue
   index=zlib.decompress(b) if flag else b
   if b'Hxv4' in index and 'startup.tjs'.encode('utf-16le') in index:archives.append(path)
  elif head[:2]==b'MZ':
   pe=struct.unpack_from('<I',head,60)[0]
   if pe+24>len(head):continue
   n=struct.unpack_from('<H',head,pe+6)[0];opt=struct.unpack_from('<H',head,pe+20)[0];sections=pe+24+opt
   ranges=[]
   for i in range(n):
    sec=head[sections+40*i:sections+40*(i+1)]
    if sec[:8].rstrip(b'\0') in (b'.rsrc',b'.rdata'):
     size,off=struct.unpack_from('<II',sec,16)
     for at in range(off,off+size,1048576):ranges.append((at,min(1048576,off+size-at)))
   image=bytearray(head);complete=True
   for at,n in ranges:
    b=need(path,at,n)
    if b is None:complete=False;continue
    if at+n>len(image):image.extend(bytes(at+n-len(image)))
    image[at:at+n]=b
   if complete and ranges:
    try:
     if candidates(bytes(image)):exes.append(path)
    except (ValueError,IndexError,struct.error):pass
 if requests:return {'protocolVersion':1,'success':True,'probeReads':requests}
 if not archives or not exes:return result('probe',None)
 required=archives+exes
 for p in allfiles:
  if any(p.lower()==(e+'.le.config').lower() for e in exes):required.append(p)
 return result('probe',{'engineId':KEY,'adapterId':'engine.'+KEY,'adapterVersion':'1.0.0','description':'KiriKiri Hxv4 / KAGEX PSB-SCN + KS（部分界面覆盖）','requiredFiles':required,'originalExecutable':exes[0],'warnings':WARN})

def translations(q):
 t=q.get('translations') or {}
 if q.get('translationsFile'):t=json.loads(Path(q['translationsFile']).read_text('utf-8-sig'))
 return t

def builtin(q,operation,source,files,output=None,trans=None):
 if not files:return {'units':[]} if operation=='extract' else {'files':[]}
 request={'protocolVersion':1,'operation':operation,'sourceDirectory':str(source),'resourceFiles':files,'outputDirectory':str(output) if output else None,'translations':trans or {},'toolPaths':q.get('toolPaths',{})}
 response=parser(request,'KirikiriKs')
 return response['extraction' if operation=='extract' else 'build']

def load(q,tmp):
 source=Path(q['sourceDirectory']);files=q['resourceFiles'];exes=[p for p in files if p.lower().endswith('.exe')];archives=[]
 for path in files:
  if not path.lower().endswith('.xp3'):continue
  accepted=None
  for exe in exes:
   try:accepted=Archive(source/path,source/exe);accepted.executable=exe;break
   except ValueError:continue
  if accepted is None:raise ValueError('资源包与主程序密钥不匹配: '+path)
  archives.append((path,accepted))
 if not archives:raise ValueError('未找到经过验证的 Hxv4 资源包')
 units=[];scenes=[];ksmap={};ksfiles=[];ksdir=tmp/'ks';ksdir.mkdir()
 for ai,(path,a) in enumerate(archives):
  for e in a.entries:
   alias=e['alias'];d=a.data.get(alias)
   if d is None:continue
   name=a.name(e);member=f'hx:{e["id"]:x}'
   if d.startswith(b'PSB\0') and (name.lower().endswith('.scn') or (len(d)>=40 and struct.unpack_from('<I',d,32)[0]==len(d))):
    st=SceneText(d)
    if st.fields:
     scenes.append((path,a,alias,member,st))
     for loc,t,kind,node in st.fields:
      locator=member+'#'+loc;u={'id':path+'::'+locator,'resourcePath':path,'locator':locator,'sourceText':t,'context':f'{name or member} / {loc}','speaker':None,'kind':kind}
      pt=tokens(t)
      if pt:u['protectedTokens']=pt
      units.append(u)
   elif name.lower().endswith('.ks'):
    payload=unpack(d)[0] if d[:2]==b'\xfe\xfe' else d
    filename=f'a{ai}_m{e["id"]:x}.ks';(ksdir/filename).write_bytes(payload);ksfiles.append(filename);ksmap[filename]=(path,a,alias,member,name)
 extracted=builtin(q,'extract',ksdir,ksfiles);ksunits=extracted.get('units',[])
 for u in ksunits:
  path,a,alias,member,name=ksmap[u['resourcePath']];loc=member+'#ks/'+u['locator']
  nu=dict(u,id=path+'::'+loc,resourcePath=path,locator=loc,context=name+' / '+str(u.get('context','')))
  units.append(nu);u['_externalId']=nu['id']
 return archives,units,scenes,ksdir,ksfiles,ksmap,ksunits

def full(q):
 op=q['operation']
 if op=='validate':
  return result('validation',{'passed':True,'issues':[],'runtimeStatus':'not-tested'})
 with tempfile.TemporaryDirectory(prefix='hx-adapter-') as td:
  tmp=Path(td);archives,units,scenes,ksdir,ksfiles,ksmap,ksunits=load(q,tmp)
  if op=='extract':return result('extraction',{'units':units,'warnings':WARN})
  if op!='build':raise ValueError('未知操作')
  trans=translations(q);known={u['id']:u for u in units}
  for uid,t in trans.items():
   if uid not in known:raise ValueError('译文包含未知单元: '+uid)
   if not isinstance(t,str) or '\0' in t:raise ValueError('译文必须为不含 NUL 的文本')
   for tok in known[uid].get('protectedTokens',[]):
    if known[uid]['sourceText'].count(tok)!=t.count(tok):raise ValueError('译文改变受保护标记: '+uid)
  replacements={p:{} for p,a in archives}
  for path,a,alias,member,st in scenes:
   edits={loc:trans[path+'::'+member+'#'+loc] for loc,orig,kind,node in st.fields if path+'::'+member+'#'+loc in trans}
   changed=st.build(edits)
   if changed is not None:replacements[path][alias]=changed
  fontwarnings=[]
  if any(uid in known and text!=known[uid]['sourceText'] for uid,text in trans.items()):
   fontwarnings=embed_chinese(archives,replacements,units,trans)
  kstrans={u['id']:trans[u['_externalId']] for u in ksunits if u['_externalId'] in trans and trans[u['_externalId']]!=u['sourceText']}
  if kstrans:
   out=tmp/'ksout';out.mkdir();built=builtin(q,'build',ksdir,ksfiles,out,kstrans)
   for f in ksfiles:
    p=out/f
    if p.exists() and p.read_bytes()!=(ksdir/f).read_bytes():
     path,a,alias,member,name=ksmap[f];replacements[path][alias]=p.read_bytes()
  output=Path(q['outputDirectory']);output.mkdir(parents=True,exist_ok=True);outfiles=[]
  for f in q['resourceFiles']:
   dest=output/f;dest.parent.mkdir(parents=True,exist_ok=True)
   a=next((a for p,a in archives if p==f),None)
   if a:a.changed_build(dest,replacements[f])
   else:shutil.copyfile(Path(q['sourceDirectory'])/f,dest)
   outfiles.append(f)
  if fontwarnings:
   licensepath=output/'localization-fonts-OFL.txt'
   shutil.copyfile(Path(__file__).parent/'fonts'/'OFL.txt',licensepath);outfiles.append(licensepath.name)
  return result('build',{'files':outfiles,'warnings':WARN+fontwarnings,'launchExecutable':archives[0][1].executable})

def main():
 q=json.load(sys.stdin)
 try:res=probe(q) if q['operation']=='probe' else full(q)
 except Exception as ex:
  traceback.print_exc(file=sys.stderr);res={'protocolVersion':1,'success':False,'error':'资源处理失败: '+str(ex)}
 if q.get('responseFile'):Path(q['responseFile']).write_text(json.dumps(res,ensure_ascii=False),'utf-8')
 else:print(json.dumps(res,ensure_ascii=False))
if __name__=='__main__':main()
