"""LiLiM SFA16 / AOS v2. Port of the guarded built-in implementation.

Huffman layout follows MIT GARbro/AosTools; see THIRD-PARTY-NOTICES.md.
Only the exact original executable profile can enable CP936 rendering.
"""
from collections import Counter
import hashlib
import heapq
import json
from pathlib import Path
import re
import struct
import sys

INDEX = 0x111
MAX_DECODED = 16 << 20
ORIGINAL_HASH = '3de457fbd05caff77391cf4e817c8d182699b0b07f0343d1a5fbaee0ad9b742b'
CALLS = [0x40C551,0x40EE05,0x418D99,0x41A881,0x41B739,0x41E319,0x41F279,0x4218D9,0x4233D9,0x428412]
LEADS = [(0x40F3BE,0xA0,0xFF),(0x40F86C,0xA0,0xFF),(0x456239,0x9F,0xFE)]
RELOCS = [(0xACC8E,0x3553),(0xACE34,0x3E07),(0xAD0AA,0x3D9B),(0xAD1BE,0x3883),(0xAD20A,0x373B),
          (0xAD346,0x331B),(0xAD3AE,0x327B),(0xAD4F4,0x38DB),(0xAD5FC,0x33DB),(0xADA10,0x3414)]
CAVE, CAVE_ADDRESS, VIRTUAL_SIZE, ORIGINAL_VSIZE = 0x7CB90, 0x47D790, 0x218, 0x7C78B
STUB = bytes.fromhex('8B442404C6401786C7401C53696D5366C74020756EC6402200E80000000058FFA0BA080000')
ORIGINAL_CALL = bytes.fromhex('FF1568E04700')
BARE = set('bgoff wndoff cgoff pause savedisable btnenable loadenable seoff backtitle loaddisable bgmoff wbl saveenable exit'.split())
TOKENS = re.compile(r'\\[A-Za-z0-9]+|@[A-Za-z0-9]')
WARNINGS = ['支持剧情对白、旁白、角色名和选项；图片、视频及游戏菜单不在覆盖范围内。',
            '建议先少量试译，检查游戏启动、中文显示、换行及存档。']


def u32(data, offset):
    return struct.unpack_from('<I', data, offset)[0]


def archive_read(data):
    if len(data) < INDEX or u32(data, 0) != 0:
        raise ValueError('仅支持 LiLiM AOS v2。')
    base, size = u32(data, 4), u32(data, 8)
    if not size or size % 40 or size // 40 > 100000 or base < INDEX + size or base > len(data):
        raise ValueError('AOS v2 目录边界无效。')
    entries, names = [], set()
    for index in range(INDEX, INDEX + size, 40):
        name = data[index:index+32].split(b'\0', 1)[0].decode('cp932')
        offset, length = base + u32(data, index+32), u32(data, index+36)
        if not name.strip() or any(c in name for c in '/\\:') or name.casefold() in names or offset+length > len(data):
            raise ValueError('AOS 成员路径、重复名称或数据边界无效。')
        names.add(name.casefold())
        entries.append((name, index, offset, length))
    end = base
    for _, _, offset, length in sorted(entries, key=lambda item:item[2]):
        if offset < end:
            raise ValueError('不支持重叠 AOS 成员。')
        end = offset + length
    return entries


def archive_build(data, entries, replacements):
    if not replacements:
        return data
    if set(replacements) - {entry[0] for entry in entries}:
        raise ValueError('替换引用了不存在的 AOS 成员。')
    base = u32(data, 4)
    output, cursor = bytearray(data[:base]), base
    for name, index, offset, length in sorted(entries, key=lambda item:item[2]):
        output.extend(data[cursor:offset])
        relative = len(output) - base
        value = replacements.get(name, data[offset:offset+length])
        output.extend(value)
        struct.pack_into('<II', output, index+32, relative, len(value))
        cursor = offset + length
    output.extend(data[cursor:])
    return bytes(output)


def huffman_decode(data):
    if len(data) < 6:
        raise ValueError('AOS Huffman 数据被截断。')
    length = u32(data, 0)
    if length > MAX_DECODED:
        raise ValueError('AOS 脚本解压长度超过限制。')
    position, nodes = 32, 0
    def bit():
        nonlocal position
        if position >= len(data)*8:
            raise ValueError('AOS Huffman 位流被截断。')
        value = data[position//8] >> (7-position%8) & 1
        position += 1
        return value
    def node(depth=0):
        nonlocal nodes
        nodes += 1
        if nodes > 511 or depth > 255:
            raise ValueError('AOS Huffman 树无效。')
        if bit():
            return (node(depth+1), node(depth+1))
        value = 0
        for _ in range(8):
            value = value << 1 | bit()
        return value
    root = node()
    output = bytearray()
    for _ in range(length):
        current = root
        while isinstance(current, tuple):
            current = current[bit()]
        output.append(current)
    return bytes(output)


def huffman_encode(data):
    if len(data) > MAX_DECODED:
        raise ValueError('AOS 脚本超过限制。')
    counts = Counter(data)
    queue = [(count, order, value) for order, (value,count) in enumerate(sorted(counts.items()))]
    if not queue:
        queue = [(1, 0, 0)]
    heapq.heapify(queue)
    order = len(queue)
    while len(queue) > 1:
        left, right = heapq.heappop(queue), heapq.heappop(queue)
        heapq.heappush(queue, (left[0]+right[0], order, (left[2],right[2])))
        order += 1
    output, current, count, codes = bytearray(struct.pack('<I',len(data))), 0, 0, {}
    def bit(value):
        nonlocal current, count
        current = current << 1 | int(value)
        count += 1
        if count == 8:
            output.append(current)
            current, count = 0, 0
    def node(value, path=()):
        bit(isinstance(value, tuple))
        if isinstance(value, tuple):
            node(value[0], path+(0,)); node(value[1], path+(1,))
        else:
            for shift in range(7,-1,-1): bit(value >> shift & 1)
            codes[value] = path
    node(queue[0][2])
    for value in data:
        for value in codes[value]: bit(value)
    if count: output.append(current << (8-count))
    return bytes(output)


def lead(value, encoding):
    return 0x81 <= value <= 0xFE if encoding == 'cp936' else 0x81 <= value <= 0x9F or 0xE0 <= value <= 0xFC


def extract_script(resource, data, encoding):
    spans, position, line = [], 0, 0
    def add(start, end, kind, speaker=None):
        value = data[start:end].decode(encoding)
        if value.strip():
            spans.append(dict(offset=start, length=end-start, unit=dict(id='', resourcePath=resource,
                locator=f'line {line}, {kind}', sourceText=value, context='', speaker=speaker, kind=kind)))
    while position < len(data):
        line += 1
        end = position
        while end < len(data) and data[end] not in (10,13): end += 1
        start, finish = position, end
        while start < finish and data[start] in (9,32): start += 1
        while finish > start and data[finish-1] in (9,32): finish -= 1
        position = end
        if position < len(data) and data[position] == 13: position += 1
        if position < len(data) and data[position] == 10: position += 1
        if start == finish or data[start] in b'#/:^%': continue
        if data[start] == ord('['):
            cursor = start + 1
            while cursor < finish:
                if lead(data[cursor], encoding): cursor += 2; continue
                if data[cursor] == ord(']'): break
                cursor += 1
            if cursor >= finish: raise ValueError('角色名括号未闭合。')
            speaker = data[start+1:cursor].decode(encoding)
            add(start+1,cursor,'name'); add(cursor+1,finish,'dialogue',speaker)
            continue
        if lead(data[start],encoding) or data[start] == ord('\\'):
            add(start,finish,'dialogue'); continue
        prefix = data[start:min(finish,start+64)].decode('ascii',errors='replace')
        word = re.match(r'^[A-Za-z_][A-Za-z0-9_]*',prefix)
        if word and word[0] in BARE: continue
        button = re.match(r'^btnset\s*\(',prefix)
        choice = re.match(r'^\$(?:select|selsub)_text[0-9]+\s*=',prefix)
        if not button and not choice:
            if data[start] != ord('$') and not re.match(r'^(?:[A-Za-z_][A-Za-z0-9_]*\s*\(|(?:var|global|float)\b)',prefix):
                add(start,finish,'dialogue')
            continue
        quotes, cursor = [], start
        while cursor < finish:
            if data[cursor] == ord('#'): break
            if data[cursor] != ord('"'): cursor += 1; continue
            quote_end = data.find(b'"',cursor+1,finish)
            if quote_end < 0: raise ValueError('指令字符串未闭合。')
            quotes.append((cursor+1,quote_end)); cursor = quote_end+1
        target = 1 if button else 0
        if len(quotes)>target:
            begin, close = quotes[target]
            if close > begin and data[begin] != ord('$'): add(begin,close,'choice')
    occurrences, narrative_index = Counter(), 0
    narrative = [span for span in spans if span['unit']['kind']!='name']
    for span in spans:
        unit = span['unit']
        anchor = hashlib.sha256((unit['kind']+'\n'+(unit['speaker'] or '')+'\n'+unit['sourceText']).encode('utf8')).hexdigest()
        occurrences[anchor] += 1
        unit['id'] = f"{resource}:{unit['kind']}:{anchor[:24]}:{occurrences[anchor]}"
        if unit['kind']=='name': unit['context']='LiLiM SFA16; character-name'
        else:
            previous=narrative[narrative_index-1]['unit']['sourceText'][-256:] if narrative_index else ''
            following=narrative[narrative_index+1]['unit']['sourceText'][:256] if narrative_index+1<len(narrative) else ''
            unit['context']=f'LiLiM SFA16; {resource}; previous: {previous}; next: {following}'
            narrative_index += 1
    return spans


def normalize(text):
    return text.replace('・','·').replace('♪','～')


def validate_text(unit, text):
    if not text.strip() or any(c in text for c in '\0\r\n'):
        raise ValueError('译文为空或含脚本换行。')
    kind=unit['kind']
    if kind=='choice' and '"' in text or kind in ('name','dialogue') and any(c in text for c in '[]'):
        raise ValueError('译文包含脚本分隔符。')
    if kind=='dialogue':
        if text[0] in '#/:^$%': raise ValueError('译文会被识别为脚本指令。')
        if unit.get('speaker') is None:
            word=re.match(r'^[A-Za-z_][A-Za-z0-9_]*',text)
            if word and word[0] in BARE or re.match(r'^(?:[A-Za-z_][A-Za-z0-9_]*\s*\(|(?:var|global|float)\b)',text):
                raise ValueError('译文会被识别为脚本指令。')
    if TOKENS.findall(unit['sourceText'])!=TOKENS.findall(text):
        raise ValueError('译文改变了脚本控制标记。')


def rebuild_script(data, spans, translations):
    output, cursor=bytearray(),0
    for span in spans:
        output.extend(data[cursor:span['offset']])
        text=normalize(translations.get(span['unit']['id'],span['unit']['sourceText']))
        validate_text(span['unit'],text)
        output.extend(text.encode('cp936'))
        cursor=span['offset']+span['length']
    output.extend(data[cursor:])
    return bytes(output)


def file_offset(address): return address-0x401000+0x400


def patch_executable(original):
    if hashlib.sha256(original).hexdigest()!=ORIGINAL_HASH:
        raise ValueError('此 SFA16 程序没有通过验证的修改配置，未生成修改 EXE。')
    data=bytearray(original)
    if u32(data,VIRTUAL_SIZE)!=ORIGINAL_VSIZE or any(data[CAVE:CAVE+len(STUB)]):
        raise ValueError('程序预期的代码空隙不存在。')
    for address in CALLS:
        offset=file_offset(address)
        if data[offset:offset+6]!=ORIGINAL_CALL: raise ValueError('字体调用特征不匹配。')
        data[offset:offset+6]=b'\xe8'+struct.pack('<i',CAVE_ADDRESS-address-5)+b'\x90'
    for address,old,new in LEADS:
        offset=file_offset(address)
        if data[offset]!=old: raise ValueError('字符前导字节分类特征不匹配。')
        data[offset]=new
    for offset,value in RELOCS:
        if struct.unpack_from('<H',data,offset)[0]!=value: raise ValueError('字体调用重定位特征不匹配。')
        struct.pack_into('<H',data,offset,value&0xFFF)
    data[CAVE:CAVE+len(STUB)]=STUB
    struct.pack_into('<I',data,VIRTUAL_SIZE,CAVE_ADDRESS-0x401000+len(STUB))
    return bytes(data)


def is_patched(data):
    if len(data)<CAVE+len(STUB) or data[CAVE:CAVE+len(STUB)]!=STUB or u32(data,VIRTUAL_SIZE)!=CAVE_ADDRESS-0x401000+len(STUB): return False
    restored=bytearray(data)
    for address in CALLS:
        offset=file_offset(address)
        if data[offset:offset+6]!=b'\xe8'+struct.pack('<i',CAVE_ADDRESS-address-5)+b'\x90': return False
        restored[offset:offset+6]=ORIGINAL_CALL
    for address,old,new in LEADS:
        offset=file_offset(address)
        if data[offset]!=new:return False
        restored[offset]=old
    for offset,value in RELOCS:
        if offset+2>len(data) or struct.unpack_from('<H',data,offset)[0]!=(value&0xFFF):return False
        struct.pack_into('<H',restored,offset,value)
    restored[CAVE:CAVE+len(STUB)]=bytes(len(STUB));struct.pack_into('<I',restored,VIRTUAL_SIZE,ORIGINAL_VSIZE)
    return hashlib.sha256(restored).hexdigest()==ORIGINAL_HASH


def load(root):
    raw=(root/'scr.aos').read_bytes();entries=archive_read(raw)
    encoding='cp936' if (root/'sfa16.zh.exe').exists() and is_patched((root/'sfa16.zh.exe').read_bytes()) else 'cp932'
    scripts=[]
    for entry in entries:
        name,_,offset,length=entry
        if name.lower().endswith('.scr'):
            data=huffman_decode(raw[offset:offset+length]);scripts.append((entry,data,extract_script(name,data,encoding)))
    return raw,entries,scripts


def execute(request):
    root=Path(request['sourceDirectory']).resolve();operation=request['operation']
    if operation=='probe':
        if not (root/'scr.aos').is_file() or not (root/'sfa16.exe').is_file():return dict(probe=None)
        if hashlib.sha256((root/'sfa16.exe').read_bytes()).hexdigest()!=ORIGINAL_HASH:return dict(probe=None)
        if not any(entry[0].lower().endswith('.scr') for entry in archive_read((root/'scr.aos').read_bytes())):return dict(probe=None)
        files=['scr.aos','sfa16.exe']
        if (root/'sfa16.exe.le.config').is_file():files.append('sfa16.exe.le.config')
        return dict(probe=dict(engineId='sfa16',adapterId='engine.sfa16.aos-v2',adapterVersion='2.0.4',
                               description='LiLiM SFA16 / AOS v2 / SCR',requiredFiles=files,originalExecutable='sfa16.exe',warnings=WARNINGS))
    raw,entries,scripts=load(root)
    units=[span['unit'] for _,_,spans in scripts for span in spans]
    if operation=='extract':return dict(extraction=dict(units=units,warnings=WARNINGS))
    output=Path(request['outputDirectory']).resolve()
    if output==root:raise ValueError('补丁必须写入独立目录。')
    translations=request.get('translations') or {}
    if request.get('translationsFile'):translations=json.loads(Path(request['translationsFile']).read_text(encoding='utf-8-sig'))
    known={unit['id']:unit for unit in units}
    for key,value in translations.items():
        if key not in known:raise ValueError('未知文本 ID：'+key)
        validate_text(known[key],value)
    changed=any(value!=known[key]['sourceText'] for key,value in translations.items())
    if operation=='build':
        replacements={}
        executable=patch_executable((root/'sfa16.exe').read_bytes()) if changed else None
        if changed:
            for entry,data,spans in scripts:
                rebuilt=rebuild_script(data,spans,translations)
                if rebuilt!=data:replacements[entry[0]]=huffman_encode(rebuilt)
        rebuilt=archive_build(raw,entries,replacements) if changed else raw
        output.mkdir(parents=True,exist_ok=True);(output/'scr.aos').write_bytes(rebuilt)
        files=['scr.aos']
        if executable is not None:(output/'sfa16.zh.exe').write_bytes(executable);files.append('sfa16.zh.exe')
        result=dict(files=files,warnings=WARNINGS)
        if executable is not None:result['launchExecutable']='sfa16.zh.exe'
        return dict(build=result)
    if operation!='validate':raise ValueError('不支持的适配操作。')
    patched,patched_entries,patched_scripts=load(output)
    if [entry[0] for entry in entries]!=[entry[0] for entry in patched_entries]:raise ValueError('AOS 成员名称或顺序改变。')
    if raw==patched:
        if (output/'sfa16.zh.exe').exists():raise ValueError('未修改资源不应启用中文程序。')
        return dict(validation=dict(passed=True,issues=[],runtimeStatus='not-tested'))
    if not is_patched((output/'sfa16.zh.exe').read_bytes()):raise ValueError('中文程序未通过固定修改配置校验。')
    for before,after in zip(entries,patched_entries):
        if not before[0].lower().endswith('.scr') and raw[before[2]:before[2]+before[3]]!=patched[after[2]:after[2]+after[3]]:
            raise ValueError('补丁改变了非脚本成员。')
    for (_,before,before_spans),(_,after,after_spans) in zip(scripts,patched_scripts):
        if [(s['unit']['locator'],s['unit']['kind']) for s in before_spans]!=[(s['unit']['locator'],s['unit']['kind']) for s in after_spans]:raise ValueError('补丁改变了文本位置。')
        def skeleton(data,spans):
            output,cursor=bytearray(),0
            for span in spans:output.extend(data[cursor:span['offset']]);cursor=span['offset']+span['length']
            output.extend(data[cursor:]);return output
        if skeleton(before,before_spans)!=skeleton(after,after_spans):raise ValueError('补丁改变了脚本控制数据。')
    return dict(validation=dict(passed=True,issues=[],runtimeStatus='not-tested'))


def main():
    request = json.load(sys.stdin)
    try:
        result=dict(protocolVersion=1,success=True,**execute(request))
    except Exception as error:
        result=dict(protocolVersion=1,success=False,error='SFA16 '+str(error))
    if request.get('responseFile'):
        with open(request['responseFile'], 'w', encoding='utf-8') as output:
            json.dump(result, output, ensure_ascii=False)
    else:
        print(json.dumps(result,ensure_ascii=False))
    return 0 if result['success'] else 1


if __name__=='__main__':sys.exit(main())
