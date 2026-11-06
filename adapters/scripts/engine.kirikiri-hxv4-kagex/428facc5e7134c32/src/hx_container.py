"""Container glue. Algorithms are supplied by the pinned GalTransl library."""
import struct,zlib,shutil
from pathlib import Path
from python.archives.kirikiri_hxv4 import derive,read_index,_chunks,name_hash
from python.archives.kirikiri_hxv4_static import candidates
from python.archives.kirikiri_hxv4_payload import Cipher,read_member
from python.engines.kirikiri_hxv4_recover import script_names

class Archive:
 def __init__(self,path,exe):
  self.path=Path(path);accepted=[]
  for pack in candidates(Path(exe).read_bytes()):
   try:
    keys=derive(pack)
    with self.path.open('rb') as s: ix=read_index(s,keys)
    accepted.append((keys,ix))
   except (ValueError,UnicodeError,zlib.error):pass
  if len(accepted)!=1:raise ValueError('无法唯一验证 EXE 与 Hxv4 包的密钥')
  self.keys,self.index=accepted[0];self.cipher=Cipher(self.keys);self.entries=self.index['files'];self.data={};names=set()
  with self.path.open('rb') as s:
   for e in self.entries:
    if 'key' not in e:continue
    d=read_member(s,e,self.cipher);self.data[e['alias']]=d
    if d[:8]==b'TJS2100\0' or d[:2]==b'\xfe\xfe':names.update(script_names(d))
  self.names={}
  names.update(('deffontmap.tjs','embfontlist.tjs','Kosugi-Regular.ttf','KosugiMaru-Regular.ttf','SourceHanSansJP-Bold.otf','SourceHanSansJP-Regular.otf','SourceHanSansJP-Heavy.otf','SourceHanSerifJP-Bold.otf','SourceHanSerifJP-Heavy.otf'))
  for name in names:
   leaf=name.replace('\\','/').rsplit('/',1)[-1]
   if leaf:self.names.setdefault(name_hash(leaf),set()).add(leaf)
 def name(self,e):
  n=self.names.get(e.get('name_hash'),set())
  return next(iter(n)) if len(n)==1 else ''
 def changed_build(self,destination,replacements):
  destination=Path(destination);destination.parent.mkdir(parents=True,exist_ok=True)
  shutil.copyfile(self.path,destination)
  if not replacements:return
  with self.path.open('rb') as s:
   s.seek(11);at=struct.unpack('<Q',s.read(8))[0];s.seek(at);flag=s.read(1)[0]
   if flag==128:s.seek(at+9);at=struct.unpack('<Q',s.read(8))[0];s.seek(at);flag=s.read(1)[0]
   if flag==1:
    plen,rlen=struct.unpack('<QQ',s.read(16));index=zlib.decompress(s.read(plen))
    if len(index)!=rlen:raise ValueError('XP3 索引长度错误')
   elif flag==0:index=s.read(struct.unpack('<Q',s.read(8))[0])
   else:raise ValueError('未支持的 XP3 索引标志')
  byalias={e['alias']:e for e in self.entries};seen=set();records=[]
  def chunk(t,b):return t+struct.pack('<Q',len(b))+b
  with destination.open('r+b') as o:
   o.seek(0,2)
   for tag,body in _chunks(index):
    if tag!=b'File':records.append(chunk(tag,body));continue
    fields=list(_chunks(body));info=dict(fields)[b'info'];units=struct.unpack_from('<H',info,20)[0];alias=info[22:22+units*2].decode('utf-16le')
    if alias not in replacements:records.append(chunk(tag,body));continue
    e=byalias[alias];data=replacements[alias];seen.add(alias)
    payload=self.cipher.transform(data,e) if e['flags'] else data
    packed=zlib.compress(payload);pos=o.tell();o.write(packed)
    newinfo=bytearray(info);struct.pack_into('<QQ',newinfo,4,len(data),len(packed))
    values={b'info':bytes(newinfo),b'segm':struct.pack('<IQQQ',1,pos,len(data),len(packed)),b'adlr':struct.pack('<I',zlib.adler32(data)&0xffffffff)}
    records.append(chunk(tag,b''.join(chunk(t,values.get(t,b)) for t,b in fields)))
   if seen!=set(replacements):raise ValueError('回填成员未能匹配原始身份')
   newindex=b''.join(records);pos=o.tell();packed=zlib.compress(newindex);o.write(b'\x01'+struct.pack('<QQ',len(packed),len(newindex))+packed);o.seek(11);o.write(struct.pack('<Q',pos))
  # Independent structural and plaintext checksum verification of every replacement.
  with destination.open('rb') as s:
   ix=read_index(s,self.keys)
   for e in ix['files']:
    if e['alias'] in replacements and read_member(s,e,self.cipher)!=replacements[e['alias']]:raise ValueError('回填后成员校验不一致')
