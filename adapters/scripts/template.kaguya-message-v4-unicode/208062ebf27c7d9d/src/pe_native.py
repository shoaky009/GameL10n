"""Unicode MessageBox trampoline for proven literals in one verified PE32 build."""
import json
import struct
from pe import Pe, Asm, align, cs_json
from pe_text import Field

CODE, TABLE, TOTAL, OLD_BOX = 0x8000, 0x10000, 0x60000, 0x12337c
MAGIC = b'KAGUYA-NATIVE-UI1\0'
PROVEN = [(0x125198, '玩家名字未设置'), (0x125174, '玩家姓氏未设置'),
          (0x1251fc, '全屏切换失败'), (0x125224, '全屏切换提示标题'),
          (0x1252ac, '图形设备丢失提示'), (0x1252d8, '图形错误标题'),
          (0x1252e8, '主窗口创建失败'), (0x125134, '错误标题'),
          (0x125348, '启动配置加载失败'), (0x125320, '系统数据加载失败'), (0x12537c, '启动错误标题'),
          (0x125388, '声音重置失败'), (0x1253b0, '字体重绘失败'), (0x1253d8, '消息窗口重绘失败'),
          (0x124c24, '清除自动存档确认'), (0x124c60, '清除自动存档标题'),
          (0x124c78, '清除快速存档确认'), (0x124cb8, '清除快速存档标题'),
          (0x1276e8, '快速存档读取失败'), (0x1276bc, '快速存档不存在'), (0x1290b0, '消息资源加载失败')]


def key(rva):
    return f'pe:literal:{rva:08X}'


class PeNative:
    def __init__(self, data):
        self.pe = pe = Pe(data)
        self.section = sh = pe.section('.kgui')
        if sh is not None:
            raw = pe.u32(sh + 20)
            if pe.data[raw:raw + len(MAGIC)] != MAGIC:
                raise ValueError('原生提示补丁标记损坏。')
            size = pe.u32(raw + 32)
            if not 2 <= size <= CODE - 64:
                raise ValueError('原生提示元数据长度错误。')
            self.state = json.loads(pe.data[raw + 64:raw + 64 + size])
        else:
            self.state = dict(OriginalImports=pe.u32(pe.opt + 104), OriginalImportSize=pe.u32(pe.opt + 108), Texts={}, Calls=[])
            for rva, _ in PROVEN:
                start = pe.offset(rva)
                end = pe.data.find(0, start)
                if not start <= end <= start + 2048:
                    raise ValueError('原生提示字符串越界。')
                self.state['Texts'][str(rva)] = bytes(pe.data[start:end]).decode('cp932')
            for header in pe.headers():
                if not pe.u32(header + 36) & 0x20000000:
                    continue
                start = pe.u32(header + 20)
                for at in range(start, start + pe.u32(header + 16) - 6):
                    if pe.data[at:at + 2] == b'\xff\x15' and pe.u32(at + 2) == pe.image_base + OLD_BOX:
                        self.state['Calls'].append(pe.rva(at))
        self.fields = [Field(key(rva), self.state['Texts'][str(rva)], context + '；MessageBox 显示参数，不修改脚本逻辑') for rva, context in PROVEN]

    def build(self, edits):
        known = {field.key: field.text for field in self.fields}
        if set(edits) - known.keys():
            raise ValueError('未知原生提示字段。')
        pe, sh, state = self.pe, self.section, self.state
        if not any(text != known[item] for item, text in edits.items()):
            return bytes(pe.data)
        for address, _ in PROVEN:
            text = edits.get(key(address), state['Texts'][str(address)])
            if '\0' in text or '\ufffd' in text:
                raise ValueError('提示文本包含空字符或替代符。')
            state['Texts'][str(address)] = text
        rva = pe.u32(sh + 12) if sh is not None else pe.next_rva()
        payload = bytearray(TOTAL)
        payload[:len(MAGIC)] = MAGIC
        metadata = cs_json(state)
        if len(metadata) > CODE - 64:
            raise ValueError('原生提示文本超过已验证容量。')
        struct.pack_into('<I', payload, 32, len(metadata))
        payload[64:64 + len(metadata)] = metadata
        cursor = TABLE + len(PROVEN) * 8

        def store(data, alignment=1):
            nonlocal cursor
            at = align(cursor, alignment)
            cursor = at + len(data)
            if cursor > TOTAL:
                raise ValueError('原生提示补丁容量不足。')
            payload[at:cursor] = data
            return rva + at

        for index, (address, _) in enumerate(PROVEN):
            text_rva = store((state['Texts'][str(address)] + '\0').encode('utf-16-le'), 2)
            struct.pack_into('<II', payload, TABLE + index * 8, address, text_rva)
        old, count = pe.offset(state['OriginalImports']), 0
        while pe.u32(old + count * 20 + 12):
            count += 1
            if count > 128:
                raise ValueError('导入表越界。')
        functions = ['MessageBoxW', 'MultiByteToWideChar', 'GetProcessHeap', 'HeapAlloc', 'HeapFree']
        descriptors = bytearray((count + len(functions) + 1) * 20)
        descriptors[:count * 20] = pe.data[old:old + count * 20]
        entries = {}
        for index, function in enumerate(functions):
            name = store(('USER32.dll' if not index else 'KERNEL32.dll').encode('ascii') + b'\0')
            hint = store(b'\0\0' + function.encode('ascii') + b'\0', 2)
            array = struct.pack('<II', hint, 0)
            lookup, iat = store(array, 4), store(array, 4)
            struct.pack_into('<IIIII', descriptors, (count + index) * 20, lookup, 0, 0, name, iat)
            entries[function] = iat
        directory = store(descriptors, 4)
        a = Asm()

        def call(name):
            a.b(0xff, 0x93)
            a.u(entries[name])

        a.b(0x55,0x8b,0xec,0x53,0x56,0x57,0x83,0xec,4,0xc7,0x45,0xf0,0,0,0,0)
        a.b(0xe8,0,0,0,0)
        popped = rva + CODE + len(a.data)
        a.b(0x5b,0x81,0xeb); a.u(popped)
        a.b(0x8b,0x55,0x0c,0x2b,0xd3,0x8d,0xb3); a.u(rva + TABLE); a.b(0xb9); a.u(len(PROVEN))
        a.label('text-loop'); a.b(0x3b,0x16); a.jump('found',0x84); a.b(0x83,0xc6,8,0x49); a.jump('text-loop',0x85)
        a.label('fallback'); a.b(0xff,0x75,0x14,0xff,0x75,0x10,0xff,0x75,0x0c,0xff,0x75,8,0xff,0x93); a.u(OLD_BOX); a.jump('done')
        a.label('found'); a.b(0x8b,0x7e,4,0x03,0xfb,0x8b,0x55,0x10,0x85,0xd2); a.jump('null-caption',0x84)
        a.b(0x2b,0xd3,0x8d,0xb3); a.u(rva + TABLE); a.b(0xb9); a.u(len(PROVEN))
        a.label('caption-loop'); a.b(0x3b,0x16); a.jump('caption-found',0x84); a.b(0x83,0xc6,8,0x49); a.jump('caption-loop',0x85)
        call('GetProcessHeap'); a.b(0x68); a.u(65536); a.b(0x6a,0,0x50); call('HeapAlloc')
        a.b(0x89,0x45,0xf0,0x85,0xc0); a.jump('fallback',0x84); a.b(0x8b,0xf0,0x68); a.u(32768)
        a.b(0x56,0x6a,0xff,0xff,0x75,0x10,0x6a,0,0x6a,0); call('MultiByteToWideChar'); a.b(0x85,0xc0); a.jump('fallback',0x84)
        a.b(0x8b,0xc6); a.jump('show')
        a.label('null-caption'); a.b(0x33,0xc0); a.jump('show')
        a.label('caption-found'); a.b(0x8b,0x46,4,0x03,0xc3)
        a.label('show'); a.b(0xff,0x75,0x14,0x50,0x57,0xff,0x75,8); call('MessageBoxW')
        a.label('done'); a.b(0x83,0x7d,0xf0,0); a.jump('return',0x84); a.b(0x8b,0xf8,0xff,0x75,0xf0,0x6a,0)
        call('GetProcessHeap'); a.b(0x50); call('HeapFree'); a.b(0x8b,0xc7)
        a.label('return'); a.b(0x8d,0x65,0xf4,0x5f,0x5e,0x5b,0x5d,0xc2,0x10,0)
        code = a.finish()
        if len(code) >= TABLE - CODE:
            raise ValueError('原生提示代码超过容量。')
        payload[CODE:CODE + len(code)] = code
        if sh is None:
            if pe.add_section(payload, '.kgui', 0xe0000060) != rva:
                raise ValueError('PE节地址不匹配。')
        else:
            if pe.u32(sh + 16) < TOTAL:
                raise ValueError('已有原生提示节容量不足。')
            raw = pe.u32(sh + 20)
            pe.data[raw:raw + len(payload)] = payload
        for site in state['Calls']:
            at = pe.offset(site)
            old_call = pe.data[at:at + 2] == b'\xff\x15' and pe.u32(at + 2) == pe.image_base + OLD_BOX
            new_call = pe.data[at] == 0xe8 and (site + 5 + pe.u32(at + 1)) & 0xffffffff == rva + CODE and pe.data[at + 5] == 0x90
            if not old_call and not new_call:
                raise ValueError('提示调用位置不是已验证版本。')
            pe.remove_relocations(site, 6)
            pe.data[at] = 0xe8
            pe.w32(at + 1, rva + CODE - site - 5)
            pe.data[at + 5] = 0x90
        pe.w32(pe.opt + 104, directory)
        pe.w32(pe.opt + 108, len(descriptors))
        return bytes(pe.data)
