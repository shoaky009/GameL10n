"""YU-RIS 480 YPF/YBN and literal display dependencies, with the legacy SJIS tunnel.

YPF layout follows GARbro (morkt, MIT). Scenario rules follow the pinned
VNTextPatch adapter (arcusmaximus, MIT), including EF reservation and name layout.
"""
import json
from pathlib import Path
import re
import struct
import zlib

from binary_common import apply_replacements, files, i32, main, resolve, save, sections, success, translations, u16

WARNINGS = ['已覆盖 YU-RIS 480 YPF/YBN 剧情、姓名、选择、标题及确认的 LET→CGACT/DIALOG 显示文字；图片/视频文字与开发调试不在范围内。',
    '部分覆盖：跨函数变量文字（含音乐/回想名称）未完全追踪；原生 EXE 右键菜单与普通系统提示保留原文。',
    '中文使用 VNTextProxy 隧道；保留原 EXE 和字体请求，不调整字号。原字体缺少部分简体字，依赖 Windows GDI 字体回退；启动、实际字体选择与排版尚未实机验证。']
SWAP = bytes.fromhex('03 48 06 35 0c 10 11 19 1c 1e 09 0b 0d 13 15 1b 20 23 26 29 2c 2f 2e 32')


def lead(value):
    return 0x81 <= value < 0xa0 or 0xe0 <= value < 0xfd


class Tunnel:
    """The YU-RIS variant reserves all 58 EF-leading tunnel slots for control codes."""
    def __init__(self, raw=b''):
        self.mapping, self.reverse = {}, {}
        if len(raw)%2:
            raise ValueError('Invalid SJIS mapping table')
        for offset in range(0, len(raw), 2):
            char = chr(u16(raw, offset))
            if char != '\0':
                self.add(char)

    def add(self, char):
        if char in self.mapping:
            return self.mapping[char]
        if ord(char)>0xffff or 0xd800 <= ord(char) <= 0xdfff:
            raise ValueError('Surrogate chars not supported')
        index = len(self.mapping)
        if index >= 58*58:
            raise ValueError('SJIS tunnel limit exceeded')
        if index >= 46*58:
            index += 58
        hi, low = divmod(index, 58)
        high = 0x81+hi if hi<31 else 0xe0+hi-31
        low += 1
        for blocked in (9,10,13,32,44):
            if low >= blocked:
                low += 1
        pair = bytes([high, low])
        self.mapping[char], self.reverse[pair] = pair, char
        return pair

    def encode(self, text):
        result = bytearray()
        for char in text:
            if '\0' == char:
                raise ValueError('显示文本不允许 NUL')
            pair = self.mapping.get(char)
            if pair is None:
                try:
                    pair = char.encode('cp932')
                    if pair[0] >= 0xf0:
                        pair = None
                except UnicodeError:
                    pair = None
                if pair is None:
                    pair = self.add(char)
            result.extend(pair)
        return bytes(result)

    def decode(self, raw):
        result, position = [], 0
        while position < len(raw):
            width = 2 if lead(raw[position]) else 1
            pair = raw[position:position+width]
            if len(pair) != width:
                raise ValueError('Truncated SJIS text')
            if width == 2 and pair[1] < 0x40:
                if pair not in self.reverse:
                    raise ValueError('SJIS 隧道映射缺失或存在原文碰撞')
                result.append(self.reverse[pair])
            else:
                result.append(pair.decode('cp932'))
            position += width
        return ''.join(result)

    def serialize(self):
        chars = list(self.mapping)
        if len(chars) > 46*58:
            chars[46*58:46*58] = ['\0']*58
        return ''.join(chars).encode('utf-16-le')


def murmur(data):
    value, position = len(data), 0
    while position+4 <= len(data):
        key = struct.unpack_from('<I', data, position)[0]*0x5bd1e995 & 0xffffffff
        key ^= key>>24
        key = key*0x5bd1e995 & 0xffffffff
        value = ((value*0x5bd1e995)^key) & 0xffffffff
        position += 4
    tail = data[position:]
    if tail:
        value ^= int.from_bytes(tail, 'little')
        value = value*0x5bd1e995 & 0xffffffff
    value ^= value>>13
    value = value*0x5bd1e995 & 0xffffffff
    return value^(value>>15)


class Ypf:
    def __init__(self, raw, load=True):
        self.raw, self.entries = raw, []
        if len(raw)<32 or raw[:4] != b'YPF\0' or i32(raw,4) != 0x1e0:
            raise ValueError('暂仅验证 YPF 0x1e0')
        count, cursor, key = i32(raw,8), 32, None
        if not 1 <= count <= 100000:
            raise ValueError('YPF entry count')
        for _ in range(count):
            size = raw[cursor+4]^255
            if size in SWAP:
                size = SWAP[SWAP.index(size)^1]
            cursor += 5
            encoded = raw[cursor:cursor+size]
            if len(encoded) != size or size<4:
                raise ValueError('YPF name bounds')
            if key is None:
                key = encoded[-4]^ord('.')
            name = bytes(b^key for b in encoded).decode('cp932').replace('\\','/')
            if name.startswith('/') or ':' in name or '..' in name.split('/'):
                raise ValueError('封包路径不安全')
            cursor += size
            packed, size, stored, offset = struct.unpack_from('<xBiii', raw, cursor)
            if min(size, stored, offset)<0 or load and offset+stored>len(raw):
                raise ValueError('YPF 索引越界')
            value = raw[offset:offset+stored] if load else None
            if load and packed:
                value = zlib.decompress(value)
                if len(value) != size:
                    raise ValueError('解包长度不符')
            self.entries.append(dict(name=name, meta=cursor, packed=bool(packed), size=size, stored=stored,
                                     offset=offset, data=value))
            cursor += 22
        if cursor != i32(raw,12):
            raise ValueError('YPF 索引长度不符')

    def build(self, edits):
        if not edits:
            return self.raw
        end = max(e['offset']+e['stored'] for e in self.entries)
        out, index = bytearray(self.raw[:end]), bytearray(self.raw[:i32(self.raw,12)])
        for entry in self.entries:
            data = edits.get(entry['name'])
            if data is None or data == entry['data']:
                continue
            value = zlib.compress(data,9) if entry['packed'] else data
            struct.pack_into('<iii', index, entry['meta']+2, len(data), len(value), len(out))
            struct.pack_into('<I', index, entry['meta']+18, murmur(value))
            out.extend(value)
        out.extend(self.raw[end:])
        out[:len(index)] = index
        return bytes(out)


def command_names(data):
    names, position = [], 16
    def terminated():
        nonlocal position
        end = data.index(0, position)
        text = data[position:end].decode('cp932')
        position = end+1
        return text
    for _ in range(i32(data,8)):
        names.append(terminated())
        count = data[position]
        position += 1
        for _ in range(count):
            terminated()
            position += 2
    return names


def crypt(data, key):
    result, cursor = bytearray(data), 32
    key = struct.pack('<I',key)
    for size_offset in (12,16,20,24):
        size = i32(data,size_offset)
        if size<0 or cursor+size>len(data):
            raise ValueError('YSTB 区段越界')
        for position in range(size):
            result[cursor+position] ^= key[position%4]
        cursor += size
    return result


def controls(raw, encode=False):
    mappings = {b'\xef\xf0':b'\r\n', b'\xef\xf2':b'\\p', b'\xef\xf3':b'\\c', b'\xef\xf5':b'\\u'}
    if encode:
        mappings = {value:key for key,value in mappings.items()}
    out, position = bytearray(raw), 0
    while position<len(out)-1:
        pair = bytes(out[position:position+2])
        if pair in mappings:
            out[position:position+2] = mappings[pair]
            position += 2
        else:
            position += 2 if lead(out[position]) else 1
    return bytes(out)


def unescape(text):
    escapes = dict(zip('abfnrtv','\a\b\f\n\r\t\v'))
    return re.sub(r'\\(.)', lambda m: escapes.get(m[1],m[1]), text, flags=re.S)


def escape(text):
    escapes = dict(zip('\a\b\f\n\r\t\v','abfnrtv'))
    return ''.join('\\'+escapes[c] if c in escapes else c for c in text)


class Scenario:
    def __init__(self, raw, names, tunnel, layout=None):
        self.raw, self.names, self.tunnel = raw, names, tunnel
        self.il, self.dl, self.vl, self.ll = struct.unpack_from('<iiii',raw,12)
        self.vp = 32+self.il+self.dl
        if self.il != i32(raw,8)*4 or self.vp+self.vl+self.ll != len(raw):
            raise ValueError('YSTB section sizes')
        self.key = struct.unpack_from('<I',raw,32+self.il+8)[0] if self.dl else 0
        self.data = crypt(raw,self.key)
        self.layout = {int(k):v for k,v in (layout or {}).items()}
        self.attrs, self.commands = [], []
        for position in range(32+self.il,self.vp,12):
            ident, kind, length, offset = struct.unpack_from('<HHii',self.data,position)
            if min(length,offset)<0 or self.vp+offset+length>len(self.data):
                raise ValueError('YSTB attribute bounds')
            self.attrs.append(dict(index=len(self.attrs), id=ident,type=kind,length=length,rel=offset,descriptor=position))
        index = 0
        for position in range(32,32+self.il,4):
            ident,count = self.data[position:position+2]
            self.commands.append((names[ident],self.attrs[index:index+count]))
            index += count
        if index != len(self.attrs):
            raise ValueError('YSTB instruction/attribute mismatch')

    def tokens(self, attr):
        if attr['type'] not in (1,3,0x101,0x103):
            return []
        value = self.data[self.vp+attr['rel']:self.vp+attr['rel']+attr['length']]
        tokens, position = [], 0
        while position<len(value):
            if position+3>len(value):
                return []
            length = u16(value,position+1)
            if position+3+length>len(value):
                return []
            tokens.append((value[position],bytes(value[position+3:position+3+length])))
            position += 3+length
        return tokens

    def quoted(self, attr):
        tokens = self.tokens(attr)
        if len(tokens)==1 and tokens[0][0]==0x4d:
            data = tokens[0][1]
            if len(data)>=2 and data[:1]==data[-1:]==b'"':
                return self.tunnel.decode(data[1:-1])
        return ''

    def references(self, attr):
        return {u16(value,1) for op,value in self.tokens(attr) if op in (0x48,0x56) and len(value)==3 and value[0]==0x24}

    def target(self, attr):
        tokens = self.tokens(attr)
        if tokens and tokens[0][0] in (0x48,0x56) and len(tokens[0][1])==3 and tokens[0][1][0]==0x24:
            return u16(tokens[0][1],1)
        return None

    def value(self, attr):
        value = bytes(self.data[self.vp+attr['rel']:self.vp+attr['rel']+attr['length']])
        if attr['type']==0:
            return self.tunnel.decode(controls(value))
        if attr['type'] != 3:
            return None
        if len(value)>=3 and value[0]==0x4d and 3+u16(value,1)==len(value):
            text = self.tunnel.decode(value[3:])
            return re.sub(r'(?<!\r)\n','\r\n',unescape(text[1:-1]))
        if len(value)<33 or (len(value)-15)%36 != 18:
            return None
        result, position = bytearray(), 0
        variable = u16(value,4)
        for index in range(1+(len(value)-15)//18):
            if value[position]!=0x56 or u16(value,position+4)!=variable or value[position+6]!=0x57 or value[position+11]!=0x29:
                return None
            code = u16(value,position+9)
            if code>255 or index%2==0 and code!=0xef:
                return None
            result.append(code)
            position += 15
            if index:
                if value[position]!=0x2b:
                    return None
                position += 3
        return self.tunnel.decode(controls(result))

    def groups(self):
        groups, current = [], []
        for name, attrs in self.commands:
            strings = []
            if name in ('WORD','RETURNCODE'):
                strings = attrs[:1]
            elif name=='_' and attrs and self.value(attrs[0]) is not None:
                strings = attrs[:1]
            elif name=='GOSUB' and attrs and (self.value(attrs[0]) or '').upper() in ('ES.CHAR.NAME','ES.SEL.SET'):
                for attr in attrs[1:]:
                    if not self.value(attr):
                        break
                    strings.append(attr)
            elif name in ('DIALOG','MENU','WINDOW'):
                strings = [a for a in attrs if a['type']==3 and self.value(a) is not None and any(ord(c)>=0x3000 for c in self.value(a))]
            strings = [a for a in strings if a['length']>0]
            if name=='WORD' or name=='_' and '\\p' not in (self.value(attrs[0]) or ''):
                current.extend(strings)
            else:
                if current:
                    groups.append(current)
                    current = []
                if name not in ('RETURNCODE','_'):
                    groups.extend([[a] for a in strings])
        if current:
            groups.append(current)
        return groups

    def strings(self):
        strings = []
        for group, attrs in enumerate(self.groups()):
            text = ''.join(self.value(a) or '' for a in attrs)
            if group in self.layout:
                count = self.layout[group]
                if count<0:
                    strings.append((text,'dialogue'))
                else:
                    if len(text)<count+2 or text[count]!='「' or text[-1]!='」':
                        raise ValueError('姓名字段布局与脚本不一致')
                    strings.extend([(text[:count],'name'),(text[count+1:-1],'dialogue')])
                continue
            match = re.fullmatch(r'(.+?)(?:[「『](.+)[』」]|(（.+）))',text,re.S)
            if match:
                strings.extend([(match[1],'name'),(match[2] or match[3],'dialogue')])
            else:
                strings.append((text,'dialogue'))
        return strings

    def serialize_value(self, attr, text):
        if attr['type']==0:
            return controls(self.tunnel.encode(text),True)
        if attr['type']==3:
            if '`' in text:
                raise ValueError('Message cannot contain backticks')
            text = '`'+escape(text.replace('\r',''))+'`'
            encoded = self.tunnel.encode(text)
            if len(encoded)>65535:
                raise ValueError('YSTB expression string is too long')
            return b'M'+struct.pack('<H',len(encoded))+encoded
        raise ValueError('Unsupported string attribute')

    def build(self, values):
        original, changed, cursor = self.strings(), [], 0
        descriptors = {}
        for group, attrs in enumerate(self.groups()):
            text, kind = values[cursor]
            old, _ = original[cursor]
            count = -1
            cursor += 1
            if kind=='name':
                count = len(text)
                text += '「'+values[cursor][0]+'」'
                old += '「'+original[cursor][0]+'」'
                cursor += 1
            if text==old:
                continue
            self.layout[group] = count
            for index, attr in enumerate(attrs):
                replacement = self.serialize_value(attr,text) if index==0 else b''
                changed.append((self.vp+attr['rel'],attr['length'],replacement))
                descriptors[attr['descriptor']+4] = len(replacement)
        out, mapping = apply_replacements(self.data,changed)
        for position,length in descriptors.items():
            struct.pack_into('<i',out,position,length)
        for attr in self.attrs:
            struct.pack_into('<i',out,attr['descriptor']+8,mapping(self.vp+attr['rel'])-self.vp)
        struct.pack_into('<i',out,20,mapping(self.vp+self.vl)-self.vp)
        return bytes(crypt(out,self.key))

    def extra_units(self):
        debug_call = any(name=='GOSUB' and any(self.quoted(a)=='es.DBG.STR.SET' for a in attrs) for name,attrs in self.commands)
        debug_layer = any(name=='CG' and any(a['id']==0 and self.quoted(a).startswith(':/W1/DBG') for a in attrs) for name,attrs in self.commands)
        if debug_call and debug_layer:
            return True, []
        wanted, dialogue, unsafe, edges = set(),set(),set(),[]
        for name,attrs in self.commands:
            for attr in attrs:
                refs = self.references(attr)
                if name=='CGACT' and attr['id']==11:
                    wanted.update(refs)
                elif name=='DIALOG' and attr['id'] in (0,1):
                    dialogue.update(refs)
                elif name in ('CG','FONT','GO','LABEL') or name=='CGACT' and attr['id']!=11 or name=='GOSUB' and attr['id']==0:
                    unsafe.update(refs)
            if name=='LET' and len(attrs)==2:
                target = self.target(attrs[0])
                if target is not None:
                    edges.extend((target,source) for source in self.references(attrs[1]))
        for collection in (wanted,dialogue,unsafe):
            while True:
                previous = len(collection)
                collection.update(source for target,source in edges if target in collection)
                if len(collection)==previous:
                    break
        cg_vars = set(wanted)
        wanted = (wanted|dialogue)-unsafe
        result = []
        for ci,(name,attrs) in enumerate(self.commands):
            if name!='LET' or len(attrs)!=2:
                continue
            target = self.target(attrs[0])
            if target is None or target not in wanted:
                continue
            for token,(op,value) in enumerate(self.tokens(attrs[1])):
                if op!=0x4d or len(value)<2 or value[0] not in (34,39) or value[-1]!=value[0]:
                    continue
                text = self.tunnel.decode(value[1:-1])
                if text:
                    context = f"LET -> {'CGACT.text' if target in cg_vars else 'DIALOG.title/text'}; command {ci}; string variable ${target:04x}"
                    result.append((attrs[1]['index'],token,text,context))
        return False,result

    def build_extra(self, edits):
        if not edits:
            return self.raw
        head, values = bytearray(self.data[:self.vp]), bytearray(self.data[self.vp:self.vp+self.vl])
        for attr in self.attrs:
            if not any(key[0]==attr['index'] for key in edits):
                continue
            expression = bytearray()
            for index,(op,value) in enumerate(self.tokens(attr)):
                if (attr['index'],index) in edits:
                    value = b'"'+self.tunnel.encode(edits[(attr['index'],index)])+b'"'
                if len(value)>65535:
                    raise ValueError('YSTB 字符串超过表达式长度上限')
                expression.extend(bytes([op])+struct.pack('<H',len(value))+value)
            struct.pack_into('<ii',head,attr['descriptor']+4,len(expression),len(values))
            values.extend(expression)
        struct.pack_into('<i',head,20,len(values))
        return bytes(crypt(head+values+self.data[self.vp+self.vl:],self.key))


def tokens(text):
    return list(dict.fromkeys(re.findall(r'\\[pcu]|\\[A-Za-z]+\[[^\]\r\n]*\]',text)))[:64]


def handle(request):
    source,operation = request['sourceDirectory'],request['operation']
    if operation=='probe':
        return probe(request)
    if operation=='validate':
        return success('validation',dict(passed=True,issues=[],runtimeStatus='not-tested'))
    selected = files(request)
    executable = next((p for p in selected if p.lower().endswith('.exe')),None)
    root = Path(executable).parent.as_posix() if executable else '.'
    def aux(name):
        return name if root=='.' else root+'/'+name
    map_path,layout_path = aux('sjis_ext.bin'),aux('yuris_text_layout.json')
    map_file,layout_file = resolve(source,map_path),resolve(source,layout_path)
    tunnel = Tunnel(map_file.read_bytes() if map_file.is_file() else b'')
    layouts = json.loads(layout_file.read_text(encoding='utf-8-sig')) if layout_file.is_file() else {}
    maps = translations(request) if operation=='build' else {}
    units,outputs,any_changed = [],[],False
    output = request.get('outputDirectory')
    for path in selected:
        raw = resolve(source,path).read_bytes()
        if not path.lower().endswith('.ypf'):
            if operation=='build':
                save(output,path,raw)
                outputs.append(path)
            continue
        archive,edits = Ypf(raw),{}
        names = command_names(next(e['data'] for e in archive.entries if e['data'][:4]==b'YSCM'))
        for entry in archive.entries:
            data,name = entry['data'],entry['name']
            if not name.lower().endswith('.ybn') or data[:4] not in (b'YSTB',b'YSCF'):
                continue
            script,extra = None,[]
            layout_key = path+'::'+name
            if data[:4]==b'YSTB':
                if not i32(data,16):
                    continue
                script = Scenario(data,names,tunnel,layouts.get(layout_key))
                developer,extra = script.extra_units()
                if developer:
                    continue
                values = script.strings()
            else:
                length = u16(data,0x4c) if len(data)>=0x4e else 0
                values = [(tunnel.decode(data[0x4e:]),'dialogue')] if len(data)==0x4e+length else []
            patched,changed = [],False
            for index,(text,kind) in enumerate(values):
                key = f'{path}::{name}::string:{index}'
                units.append(dict(id=key,resourcePath=path,locator=f'{name}#string:{index}',sourceText=text,
                    context=name,speaker=None,kind='name' if kind=='name' else 'ui' if name.lower().startswith('yscfg') else 'dialogue',
                    protectedTokens=tokens(text)))
                value = maps.get(key,text)
                changed |= value!=text
                patched.append((value,kind))
            any_changed |= changed
            if operation=='build' and changed:
                if script:
                    edits[name] = script.build(patched)
                    layouts[layout_key] = script.layout
                else:
                    caption = tunnel.encode(patched[0][0])
                    if len(caption)>32767:
                        raise ValueError('YSCF caption exceeds signed length limit')
                    edits[name] = data[:0x4c]+struct.pack('<h',len(caption))+caption
            extra_edits = {}
            for attribute,token,text,context in extra:
                suffix = f'let:{attribute}:literal:{token}'
                key = f'{path}::{name}::{suffix}'
                units.append(dict(id=key,resourcePath=path,locator=name+'#'+suffix,sourceText=text,
                    context=context,speaker=None,kind='ui',protectedTokens=tokens(text)))
                if key in maps and maps[key]!=text:
                    extra_edits[(attribute,token)] = maps[key]
                    any_changed = True
            if operation=='build' and extra_edits:
                basis = edits.get(name,data)
                edits[name] = Scenario(basis,names,tunnel).build_extra(extra_edits)
        if operation=='build':
            save(output,path,archive.build(edits))
            outputs.append(path)
    if operation=='extract':
        return success('extraction',dict(units=units,warnings=WARNINGS))
    if operation=='build':
        if any_changed:
            if not executable:
                raise ValueError('中文构建需要已识别的游戏主程序。')
            proxy = Path(__file__).parent/'tools'/'vntextproxy'/'winmm.dll'
            proxy_bytes = proxy.read_bytes()
            existing = next((p for p in selected if p.lower()==aux('winmm.dll').lower()),None)
            if existing and resolve(source,existing).read_bytes()!=proxy_bytes:
                raise ValueError('游戏已有 winmm.dll，不能覆盖已有代理；请保留原文件并单独核对。')
            for path,value in [(layout_path,json.dumps(layouts,separators=(',',':')).encode()),(map_path,tunnel.serialize()),(aux('winmm.dll'),proxy_bytes)]:
                save(output,path,value)
                if path not in outputs:
                    outputs.append(path)
            actual = handle(dict(operation='extract',sourceDirectory=output,resourceFiles=outputs))['extraction']['units']
            if {u['id'] for u in actual} != {u['id'] for u in units}:
                raise ValueError('回填单元结构变化')
        return success('build',dict(files=outputs,warnings=WARNINGS,launchExecutable=executable))
    raise ValueError('Unsupported operation: '+operation)


def probe(request):
    source,candidates = request['sourceDirectory'],request.get('probeFiles',[])
    required,reads,executable = [],[],None
    def range_bytes(path,offset,length):
        for item in request.get('probeRanges',[]):
            if item['path']==path and item['offset']==offset:
                return Path(item['dataPath']).read_bytes()[:length]
        return None
    for item in candidates:
        path = item['path']
        header = resolve(source,path).read_bytes()
        if path.lower().endswith('.ypf'):
            if len(header)<32 or header[:4]!=b'YPF\0' or i32(header,4)!=0x1e0:
                continue
            end = i32(header,12)
            if not 32<=end<=4<<20:
                continue
            directory = header[:end] if end<=len(header) else range_bytes(path,0,end)
            if directory is None:
                if end<=1<<20:
                    reads.append(dict(path=path,offset=0,length=end))
                continue
            archive = Ypf(directory,False)
            if any(e['name'].lower().endswith('.ybn') for e in archive.entries):
                required.append(path)
        elif path.lower().endswith('.exe') and len(header)>1024 and header[:2]==b'MZ' and b'YU-RIS' in header:
            _,items = sections(header)
            section = next((s for s in items if s['name']=='.rdata'),None)
            if not section or section['size']>1<<20:
                continue
            offset,size = section['file'],section['size']
            value = header[offset:offset+size] if offset+size<=len(header) else range_bytes(path,offset,size)
            if value is None:
                reads.append(dict(path=path,offset=offset,length=size))
            elif all(name in value for name in (b'TextOutA\0',b'CreateFontIndirectA\0',b'WINMM.dll\0')):
                required.append(path)
                executable = executable or path
    if reads:
        return success('probeReads',reads)
    if not required:
        return success('probe',None)
    for item in candidates:
        path = item['path']
        if path.lower().endswith('.exe.le.config') and path[:-10] in required:
            required.append(path)
        if executable and (path.lower().endswith('yuris_text_layout.json') or path.lower() in
                [((Path(executable).parent/aux).as_posix()).lower() for aux in ('sjis_ext.bin','winmm.dll')]):
            required.append(path)
    return success('probe',dict(engineId='engine.yuris-ypf',adapterId='engine.yuris-ypf',adapterVersion='0.2.0',
        description='YU-RIS 480 YPF/YBN（复用 VNTextPatch）',requiredFiles=required,warnings=WARNINGS,originalExecutable=executable))


if __name__=='__main__':
    main(handle)
