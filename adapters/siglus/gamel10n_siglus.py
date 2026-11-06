"""Siglus JSON adapter v1. Shared by built-in recognition and generated rules."""
from collections import Counter
from pathlib import Path
import hashlib
import json
import re
import struct
import sys
import traceback

from siglus_ssu import common, const, dat, pck, textmap, GEI, patch
from siglus_ssu.native_ops import lzss_pack
from siglus_binary import package_header, read_sources, replace_strings, replace_members
from siglus_font import Glyphs, PREFIXES, fkds_template, fill_pages, validate_pages

VERSION = '1.0.0'
WARNINGS = ['支持剧情对白、角色名及已确认的显示调用；图片内文字和未识别的自定义界面不在提取范围内。',
            '补丁通过资源校验后仍需试译检查游戏显示、换行和存档。']


def safe(root, relative):
    path = (root / relative).resolve()
    if not path.is_relative_to(root.resolve()) or path == root.resolve():
        raise ValueError('资源路径超出工作目录。')
    return path


def resources(root, request):
    files = request.get('resourceFiles')
    if files is None:
        files = [p.relative_to(root).as_posix() for p in root.iterdir() if p.is_file()]
        if (root/'g00').is_dir():
            files += [p.relative_to(root).as_posix() for p in (root/'g00').glob('*.g00')
                      if p.name.lower().startswith(tuple(x.lower() for x in PREFIXES))]
    for name in files: safe(root, name)
    by_name = {name.lower():name for name in files}
    if 'scene.pck' not in by_name or 'gameexe.dat' not in by_name:
        return None
    exes = [name for name in files if '/' not in name.replace('\\','/') and name.lower().endswith('.exe')
            and not name.lower().endswith('.zh.exe')]
    if 'siglusengine.exe' in by_name:
        exe = by_name['siglusengine.exe']
    elif len(exes) == 1:
        exe = exes[0]
    else:
        raise ValueError('Siglus 启动程序不明确，请通过智能适配确认所用 EXE。')
    selected = [by_name['scene.pck'], by_name['gameexe.dat'], exe]
    profile = by_name.get((exe+'.le.config').lower())
    if profile: selected.append(profile)
    selected += sorted(name for name in files if name.lower().startswith('g00/')
                       and Path(name).name.lower().startswith(tuple(x.lower() for x in PREFIXES))
                       and name.lower().endswith('.g00'))
    if len(selected) > 1000: raise ValueError('字库资源数量异常，需进一步分析。')
    return {'pck':selected[0], 'config':selected[1], 'exe':exe, 'files':selected}


def rules_file():
    path = Path(sys.argv[0]).resolve().parent / 'siglus-rules.json'
    if not path.exists(): return {}
    rules = json.loads(path.read_text(encoding='utf-8-sig'))
    if not isinstance(rules, dict): raise ValueError('Siglus 规则必须是 JSON 对象。')
    return rules


def input_path(root, relative, inputs):
    original = safe(root, relative)
    return Path(inputs.get(relative, original)) if inputs else original


def load(root, selected, rules, disassemble=True, strict=True, inputs=None):
    package = input_path(root,selected['pck'],inputs).read_bytes()
    header = package_header(package)
    key = common.read_siglus_engine_exe_el(str(input_path(root,selected['exe'],inputs)))
    if header['scn_data_exe_angou_mod'] and len(key) != 16:
        raise ValueError('未能从当前 EXE 获取 Siglus 资源密钥，需要智能适配。')
    sources = read_sources(package)
    fkds = fkds_template(sources)
    limitations = []
    if any('$fkds_sys_text' in s for s in sources.values()) and not fkds:
        limitations.append('发现 FKDS 自定义字库，但其显示规则不符合内置模板；请继续智能适配。')
    custom = [{'command':'$fkds_sys_text','argument':1,'kind':'dialogue'}] if fkds else []
    custom += rules.get('commandRules', [])
    for rule in custom:
        if not isinstance(rule.get('command'),str) or not isinstance(rule.get('argument'),int) or not 0 <= rule['argument'] <= 32:
            raise ValueError('显示参数规则无效。')
        if rule.get('kind','dialogue') not in ('dialogue','name','choice','narration'):
            raise ValueError('文本类型规则无效。')
    config=''; offsets=[]
    try:
        info, config = GEI.read_gameexe_dat(str(input_path(root,selected['config'],inputs)), key)
        if not info['ini_ok'] or not re.search(r'^\s*#CONFIG\.FONT\.NAME\s*=',config,re.M|re.I):
            raise ValueError('Gameexe.dat 字体配置无法可靠解析。')
        offsets = patch._find_charset_slot_offsets(input_path(root,selected['exe'],inputs).read_bytes())
        if not offsets: raise ValueError('当前 Siglus EXE 的字体筛选规则尚未适配。')
    except (ValueError, RuntimeError) as error: limitations.append(str(error))
    if strict and limitations: raise ValueError('；'.join(limitations))
    result = dict(package=package,header=header,key=key,sources=sources,fkds=fkds,
                  config=config,charsetOffsets=offsets,scenes=[],units=[],calls=Counter(),unresolved=0,limitations=limitations)
    if not disassemble: return result
    names = set()
    for item in pck.iter_pck_scene_dat_items(package,scene_exe_el=key,require_exe=True):
        name = item['scene_name']
        if name in names: raise ValueError('场景名称重复，无法建立稳定文本标识。')
        names.add(name)
        blob = item['blob']
        if not blob: raise ValueError('场景解码失败：'+name)
        bundle = dat.dat_disassembly_bundle(blob,scene_name=name,scene_no=item['scene_no'],
                                           pack_context=item['pack_context'],emit_text=False)
        if not bundle or not bundle['complete']:
            raise ValueError('场景字节码未完全解析：'+name)
        strings, _ = textmap._parse_scn_dat(blob)
        kinds = {i:('name' if kind==2 else 'dialogue','standard')
                 for i,kind in textmap._collect_dat_string_kinds(bundle).items() if kind in (1,2)}
        for ev in bundle['trace']:
            if ev['op'] != 'CD_COMMAND': continue
            call = ev.get('_call_name','')
            refs = ev.get('_arg_string_ids',[])
            if any(isinstance(i,int) for i in refs): result['calls'][call] += 1
            for rule in custom:
                if call != rule['command']: continue
                a = rule['argument']
                sid = refs[a] if a < len(refs) else None
                if not isinstance(sid,int):
                    result['unresolved'] += 1
                    continue
                if not 0 <= sid < len(strings): raise ValueError('参数字符串引用越界。')
                values = ev.get('_arg_values',[])
                if a >= len(values) or values[a] != strings[sid]:
                    raise ValueError('显示参数值与字节码引用不一致。')
                kinds[sid] = (rule.get('kind','dialogue'),call)
        for sid,(kind,call) in sorted(kinds.items()):
            if not strings[sid].strip(): continue
            locator = name + '/string/' + str(sid)
            uid = 'siglus:' + hashlib.sha256(name.encode('utf-8')).hexdigest()[:20] + ':' + str(sid)
            result['units'].append(dict(id=uid,resourcePath=selected['pck'],locator=locator,
                sourceText=strings[sid],context='Siglus | '+name+' | '+call,speaker=None,kind=kind))
        item['strings'] = strings; item['kinds'] = kinds
        result['scenes'].append(item)
    if result['unresolved'] and strict:
        raise ValueError('%s 个显示调用使用动态字符串，当前规则无法保证覆盖；请继续智能适配。'%result['unresolved'])
    if result['calls']['$fkds_sys_text'] and not fkds:
        message='字节码包含 FKDS 显示调用，但缺少可核对的字库源码模板；请继续智能适配。'
        if strict: raise ValueError(message)
        result['limitations'].append(message)
    if len(result['scenes']) != header['scn_data_cnt']:
        raise ValueError('场景数量与资源索引不一致。')
    if not result['units'] and strict: raise ValueError('尚未识别出可翻译文本，请用智能适配补充显示调用规则。')
    return result


def warnings(data):
    return WARNINGS + (['此模板使用图片字库：已有字形保留原样，缺少的中文字符以宋体补充。'] if data['fkds'] else [])


def probe(root, req, selected, rules):
    if selected is None: return None
    header = safe(root,selected['pck']).read_bytes()[:92]
    if len(header) != 92 or struct.unpack_from('<I',header)[0] != 92: return None
    # Generated adapters receive bounded headers here. Full reads and template
    # checks happen only after the host snapshots the declared resources.
    if not req.get('probeFiles'): load(root,selected,rules,disassemble=False)
    return dict(engineId='siglus',adapterId='engine.siglus',adapterVersion=VERSION,
                description='Siglus / Scene.pck',requiredFiles=selected['files'],warnings=[],
                originalExecutable=selected['exe'])


def edited_config(original, text, key):
    changed, a = re.subn(r'(^\s*#CONFIG\.FONT\.NAME\s*=\s*)"[^"\r\n]*"',r'\g<1>"SimSun"',text,flags=re.M|re.I)
    changed, b = re.subn(r'(^\s*#CONFIG\.FONT\.TYPE\s*=\s*)\d+',r'\g<1>3',changed,flags=re.M|re.I)
    if a != 1 or b != 1: raise ValueError('Gameexe.dat 字体设置不唯一。')
    payload = bytearray(lzss_pack(changed.encode('utf-16le')))
    GEI.xor_cycle_inplace(payload,const.GAMEEXE_DAT_ANGOU_CODE)
    if struct.unpack_from('<i',original,4)[0]: GEI.xor_cycle_inplace(payload,key)
    return original[:8]+payload


def build(root,out,selected,data,translations,compact):
    out.mkdir(parents=True,exist_ok=True)
    if any(out.iterdir()): raise ValueError('补丁输出目录必须为空。')
    units = {u['id']:u for u in data['units']}
    if not set(translations).issubset(units): raise ValueError('译文中有不存在的文本 ID，请重新识别。')
    changed = {uid:text for uid,text in translations.items() if text != units[uid]['sourceText']}
    for text in changed.values():
        if not isinstance(text,str) or '\0' in text or any(ord(c)>65535 or 0xd800<=ord(c)<=0xdfff for c in text):
            raise ValueError('译文包含当前 Siglus 方案不支持的字符。')
    files = []
    def write(name,content):
        path = safe(out,name); path.parent.mkdir(parents=True,exist_ok=True); path.write_bytes(content)
        if name not in files: files.append(name)
    if not compact or not changed:
        for name in selected['files']: write(name,safe(root,name).read_bytes())
    if not changed: return dict(files=files,warnings=[],launchExecutable=None)
    edits = {}
    by_locator = {units[uid]['locator']:text for uid,text in changed.items()}
    for item in data['scenes']:
        replacement = {sid:by_locator[item['scene_name']+'/string/'+str(sid)] for sid in item['kinds']
                       if item['scene_name']+'/string/'+str(sid) in by_locator}
        if replacement:
            blob = replace_strings(item['blob'],replacement)
            # Check the input decoder, then verify the selected encoding by
            # independently decoding its result before adding it to the pack.
            h=data['header']; i=item['scene_no']
            pos,size=struct.unpack_from('<II',data['package'],h['scn_data_index_list_ofs']+i*8)
            raw=data['package'][h['scn_data_list_ofs']+pos:h['scn_data_list_ofs']+pos+size]
            enc={'lzss':True,'easy':True,'exe':bool(h['scn_data_exe_angou_mod'])}
            if pck._decode_scene_blob(raw,h,data['key'],require_exe=True) != item['blob']:
                raise ValueError('场景压缩或编码配置不匹配。')
            encoded=textmap._encode_scn_dat(blob,enc,data['key'])
            if pck._decode_scene_blob(encoded,h,data['key'],require_exe=True)!=blob:
                raise ValueError('回填场景重新解码失败。')
            edits[i]=encoded
    write(selected['pck'],replace_members(data['package'],edits))
    write(selected['config'],edited_config(safe(root,selected['config']).read_bytes(),data['config'],data['key']))
    executable=bytearray(safe(root,selected['exe']).read_bytes())
    for offset in data['charsetOffsets']:
        if executable[offset] not in (0,128,134): raise ValueError('EXE 字体筛选字节不符合预期。')
        executable[offset]=134
    launch=str(Path(selected['exe']).with_suffix('.zh.exe'))
    write(launch,executable)
    profile=selected['exe']+'.le.config'
    if profile in selected['files']: write(launch+'.le.config',safe(root,profile).read_bytes())
    font=Glyphs()
    try:
        # GDI is also used for ordinary text/name display outside bubble text.
        font.require(''.join(text for uid,text in changed.items() if not units[uid]['context'].endswith('$fkds_sys_text')))
        if data['fkds']:
            chars=set(''.join(text for uid,text in changed.items() if units[uid]['context'].endswith('$fkds_sys_text')))
            for name,content in fill_pages(root,chars,font): write(name,content)
    finally: font.close()
    return dict(files=files,warnings=warnings(data),launchExecutable=launch)


def validate(root,out,selected,data,rules):
    # Overlay only files actually generated. Read unchanged resources from the
    # source snapshot, without creating a second directory tree.
    changed_pck=safe(out,selected['pck'])
    if not changed_pck.is_file(): raise ValueError('补丁缺少剧情资源。')
    package=changed_pck.read_bytes(); package_header(package)
    original_scenes={item['scene_no']:item for item in data['scenes']}
    if read_sources(package)!=data['sources']: raise ValueError('回填修改了内嵌原始源码。')
    chars=set(); seen=set(); replacements={}; any_changes=False
    for item in pck.iter_pck_scene_dat_items(package,scene_exe_el=data['key'],require_exe=True):
        original=original_scenes[item['scene_no']]; seen.add(item['scene_no'])
        if item['scene_name']!=original['scene_name']: raise ValueError('场景名称发生变化。')
        strings,_=textmap._parse_scn_dat(item['blob'])
        if len(strings)!=len(original['strings']): raise ValueError('字符串数量发生变化。')
        edits={i:text for i,text in enumerate(strings) if text!=original['strings'][i]}
        any_changes |= bool(edits)
        if not set(edits).issubset(original['kinds']): raise ValueError('补丁修改了未提取的字符串。')
        if replace_strings(original['blob'],edits)!=item['blob']: raise ValueError('补丁修改了字节码或其他非文本内容。')
        for i,text in edits.items():
            if original['kinds'][i][1]=='$fkds_sys_text': chars.update(text)
        if edits:
            h=package_header(package); i=item['scene_no']
            pos,size=struct.unpack_from('<II',package,h['scn_data_index_list_ofs']+i*8)
            replacements[i]=package[h['scn_data_list_ofs']+pos:h['scn_data_list_ofs']+pos+size]
    if seen!=set(original_scenes): raise ValueError('补丁遗漏场景。')
    if replace_members(data['package'],replacements)!=package:
        raise ValueError('补丁修改了场景以外的封包内容。')
    if any_changes:
        original_config=safe(root,selected['config']).read_bytes()
        if safe(out,selected['config']).read_bytes()!=edited_config(original_config,data['config'],data['key']):
            raise ValueError('补丁字体配置与预期不一致。')
        exe=bytearray(safe(root,selected['exe']).read_bytes())
        for offset in data['charsetOffsets']: exe[offset]=134
        launch=str(Path(selected['exe']).with_suffix('.zh.exe'))
        if safe(out,launch).read_bytes()!=exe: raise ValueError('中文 EXE 含有预期以外的修改。')
        profile=selected['exe']+'.le.config'
        if profile in selected['files'] and safe(out,launch+'.le.config').read_bytes()!=safe(root,profile).read_bytes():
            raise ValueError('中文 EXE 未保留原转区配置。')
    if data['fkds'] and chars: validate_pages(root,out,chars)
    return dict(passed=True,issues=[],runtimeStatus='not-tested')


def inspect(root,selected,rules,req):
    inputs=req.get('inspectionInputs')
    source=req.get('source',''); query=req.get('query','')
    if source or query:
        sources=read_sources(input_path(root,selected['pck'],inputs).read_bytes())
        limit=min(12000,max(1,int(req.get('length',6000)))); start=max(0,int(req.get('offset',0)))
        matches=[]; remaining=limit
        for name,content in sources.items():
            if source and name!=source: continue
            pos=content.lower().find(query.lower(),start) if query else start
            if pos<0: continue
            pos=max(start,pos-160) if query else pos
            end=min(len(content),pos+remaining)
            matches.append(dict(source=name,offset=pos,nextOffset=end,more=end<len(content),content=content[pos:end]))
            remaining-=end-pos
            if remaining<=0 or len(matches)>=8: break
        return dict(matches=matches,sources=list(sources) if source and not matches else None)
    data=load(root,selected,rules,strict=False,inputs=inputs)
    return dict(engine='Siglus',scenes=len(data['scenes']),units=len(data['units']),template='FKDS 48/441' if data['fkds'] else 'standard',
                embeddedSources=list(data['sources']),calls=data['calls'].most_common(30),warnings=warnings(data),
                limitations=data['limitations'],unresolvedDisplayCalls=data['unresolved'],
                usage='已支持模板可用常规识别；limitations 非空时需先解决限制。新显示模板先查询内嵌源码，再 copy_adapter engine.siglus 并编辑 tools/siglus-rules.json；不要重新编写解包器。')


def dispatch(req):
    if req.get('protocolVersion')!=1: raise ValueError('不支持的适配协议版本。')
    root=Path(req['sourceDirectory']).resolve(); selected=resources(root,req); rules=rules_file()
    op=req['operation']
    if op=='probe': return {'probe':probe(root,req,selected,rules)}
    if not selected: raise ValueError('未找到 Siglus 剧情包、配置和启动程序。')
    if op=='inspect': return {'inspection':inspect(root,selected,rules,req)}
    data=load(root,selected,rules)
    if op=='extract': return {'extraction':dict(units=data['units'],warnings=warnings(data))}
    out=Path(req['outputDirectory']).resolve()
    if out==root or out.is_relative_to(root) or root.is_relative_to(out):
        raise ValueError('原始资源与补丁目录必须相互独立。')
    if op=='build':
        translations=json.loads(Path(req['translationsFile']).read_text(encoding='utf-8-sig')) if req.get('translationsFile') else req.get('translations',{})
        return {'build':build(root,out,selected,data,translations,req.get('compactOutput',False))}
    if op=='validate': return {'validation':validate(root,out,selected,data,rules)}
    raise ValueError('不支持的操作。')


def main():
    req=json.load(sys.stdin)
    try: response=dict(protocolVersion=1,success=True,**dispatch(req))
    except Exception as error:
        traceback.print_exc(file=sys.stderr)
        response=dict(protocolVersion=1,success=False,error='Siglus 处理失败：'+str(error))
    encoded=json.dumps(response,ensure_ascii=False,separators=(',',':'))
    if req.get('responseFile'): Path(req['responseFile']).write_text(encoded,encoding='utf-8')
    else: print(encoded)
    return 0 if response['success'] else 1


if __name__=='__main__': sys.exit(main())
