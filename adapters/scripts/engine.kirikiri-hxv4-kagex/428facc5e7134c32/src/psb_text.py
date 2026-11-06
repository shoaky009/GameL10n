import re
from python.engines.kirikiri_psb import Psb

CONTROL=re.compile(r'\\[nrt]|%[A-Za-z][^;\r\n]*;|\[[^\]\r\n]*\]')
def tokens(text):
 result=re.findall(r'\\[nrt]|%[A-Za-z][^;\r\n]*;',text)
 if '[' in text:result+=['[',']']
 return list(dict.fromkeys(result))
def obj(p,n):return {p.keys[k]:v for k,v in n.value} if n.tag==33 else {}
def arr(n):return n.value if n is not None and n.tag==32 else []
def text(p,n):return p.text(n) if n is not None and 21<=n.tag<=24 else None
def rendered_length(s):return len(CONTROL.sub('',s).encode('utf-16le'))//2

class SceneText:
 def __init__(self,data):
  self.p=Psb(data);self.fields=[];self.counts=[];root=obj(self.p,self.p.root)
  if 'scenes' not in root:return
  for si,scene in enumerate(arr(root['scenes'])):
   sc=obj(self.p,scene)
   self.add(sc.get('title'),f'scenes/{si}/title','ui')
   for ti,row in enumerate(arr(sc.get('texts'))):
    rr=arr(row)
    if len(rr)<2:raise ValueError('SCN texts 字段布局未知')
    for vi,variant in enumerate(arr(rr[1])):
     vals=arr(variant)
     if len(vals)<3:raise ValueError('SCN language variant 字段布局未知')
     path=f'scenes/{si}/texts/{ti}/1/{vi}'
     self.add(vals[0],path+'/0','name')
     self.add(vals[1],path+'/1','dialogue')
     self.counts.append((path+'/1',path+'/2',vals[2]))
     for i in (3,4):
      if i<len(vals):self.add(vals[i],path+'/'+str(i),'dialogue')
 def add(self,n,path,kind):
  t=text(self.p,n)
  if t and t.strip():self.fields.append((path,t,kind,n))
 def route(self,path):
  node=self.p.root;route=[]
  for part in path.split('/'):
   if node.tag==33:
    index=next(i for i,(k,v) in enumerate(node.value) if self.p.keys[k]==part)
    node=node.value[index][1]
   elif node.tag==32:
    index=int(part);node=node.value[index]
   else:raise ValueError('SCN 字段路径类型错误: '+path)
   route.append(index)
  return tuple(route)
 def build(self,translations):
  edits={};changed={}
  for path,original,kind,node in self.fields:
   if path not in translations or translations[path]==original:continue
   new=translations[path]
   if not isinstance(new,str) or '\0' in new:raise ValueError('PSB 文本必须是无 NUL 的字符串')
   for token in tokens(original):
    if original.count(token)!=new.count(token):raise ValueError('SCN 控制符数量改变: '+path)
   edits[self.route(path)]=new;changed[path]=new
  if not edits:return None
  for body,path,count in self.counts:
   if body not in changed:continue
   if not 4<=count.tag<=12:raise ValueError('SCN 文本长度字段类型未知')
   edits[self.route(path)]=rendered_length(changed[body])
  return self.p.patch(edits)
