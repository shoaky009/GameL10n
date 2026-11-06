"""Read inert PE/Delphi resources and verify the known Unicode loader structure."""
import re
import struct


class PE:
    def __init__(self, data):
        self.data = data
        if self.take(0, 2) != b'MZ':
            raise ValueError('不是 PE 程序。')
        pe = self.u32(0x3c)
        if self.take(pe, 4) != b'PE\0\0':
            raise ValueError('无效 PE 头。')
        self.machine = self.u16(pe+4)
        self.optional = pe+24
        self.magic = self.u16(self.optional)
        self.image_base = self.u32(self.optional+28) if self.magic == 0x10b else 0
        directory = {0x10b: 96, 0x20b: 112}.get(self.magic)
        if directory is None:
            raise ValueError('不支持的 PE 可选头。')
        self.directory = self.optional+directory
        count, size = self.u16(pe+6), self.u16(pe+20)
        if not 0 < count <= 96:
            raise ValueError('PE 节数量无效。')
        self.sections = []
        for i in range(count):
            p = self.optional+size+40*i
            self.take(p, 40)
            self.sections.append((self.u32(p+12), self.u32(p+16), self.u32(p+20), self.u32(p+36)))

    def take(self, p, size):
        if p < 0 or size < 0 or p+size > len(self.data):
            raise ValueError('PE 数据越界。')
        return self.data[p:p+size]

    def u16(self, p):
        return struct.unpack('<H', self.take(p, 2))[0]

    def u32(self, p):
        return struct.unpack('<I', self.take(p, 4))[0]

    def offset(self, rva):
        for va, size, raw, flags in self.sections:
            if va <= rva < va+size:
                p = raw+rva-va
                self.take(p, 1)
                return p
        raise ValueError('未映射的 PE 地址。')

    def off_va(self, va):
        return self.offset(va-self.image_base)

    def va(self, p):
        for va, size, raw, flags in self.sections:
            if raw <= p < raw+size:
                return self.image_base+va+p-raw
        raise ValueError('未映射的 PE 文件地址。')

    def resources(self):
        rva, size = self.u32(self.directory+16), self.u32(self.directory+20)
        if not size:
            return {}
        base = self.offset(rva)
        output, seen = {}, set()
        def walk(rel, path, depth):
            if depth > 4 or rel in seen:
                raise ValueError('PE 资源结构无效。')
            seen.add(rel)
            p = base+rel
            count = self.u16(p+12)+self.u16(p+14)
            if count > 8192:
                raise ValueError('PE 资源数量异常。')
            for i in range(count):
                e = p+16+8*i
                name, child = self.u32(e), self.u32(e+4)
                if name & 0x80000000:
                    at = base+(name & 0x7fffffff)
                    name = self.take(at+2, self.u16(at)*2).decode('utf-16le')
                else:
                    name = '#'+str(name)
                next_path = path+'/'+name if path else name
                if child & 0x80000000:
                    walk(child & 0x7fffffff, next_path, depth+1)
                else:
                    leaf = base+child
                    blob = self.take(self.offset(self.u32(leaf)), self.u32(leaf+4))
                    if next_path in output:
                        raise ValueError('重复的 PE 资源。')
                    output[next_path] = blob
        walk(0, '', 0)
        return output


def key31(image):
    keys = [v for k, v in PE(image).resources().items() if k.lower().startswith('#10/reskey/')]
    if len(keys) != 1:
        raise ValueError('原引擎缺少唯一 RESKEY。')
    return keys[0]


def key30(image):
    resources = PE(image).resources()
    forms = [v for k, v in resources.items() if k.startswith('#10/TFORM1/')]
    if not forms:
        raise ValueError('缺少 TFORM1。')
    data, p, key, nodes = forms[0], 4, None, 0
    if data[:4] != b'TPF0':
        raise ValueError('TFORM1 不是 TPF0。')
    def take(n):
        nonlocal p
        if n < 0 or p+n > len(data):
            raise ValueError('TPF0 数据越界。')
        out = data[p:p+n]
        p += n
        return out
    def byte():
        return take(1)[0]
    def integer():
        return struct.unpack('<i', take(4))[0]
    def short():
        return take(byte()).decode('latin1')
    def value(depth=0):
        if depth > 64:
            raise ValueError('TPF0 值嵌套过深。')
        tag = byte()
        if tag == 0:
            return None
        if tag == 1:
            values = []
            while data[p] != 0:
                values.append(value(depth+1))
            byte()
            return values
        if tag in (2, 3, 4, 5, 12, 15, 16, 19):
            return take({2: 1, 3: 2, 4: 4, 5: 10, 12: 4, 15: 8, 16: 8, 19: 8}[tag])
        if tag in (6, 7):
            return short()
        if tag in (8, 9):
            return tag == 9
        if tag in (10, 18, 20):
            return take(integer()*(2 if tag == 18 else 1))
        if tag == 11:
            while short():
                pass
            return None
        raise ValueError('不支持的 TPF0 值类型：' + str(tag))
    def node(depth):
        nonlocal key, nodes
        length = byte()
        if length == 0:
            return False
        nodes += 1
        if depth > 64 or nodes > 100000 or length >= 0xf0:
            raise ValueError('TPF0 节点超限或含未知前缀。')
        take(length)
        name = short()
        while prop := short():
            val = value()
            if name == 'IconKeyImage' and prop == 'Picture.Data' and isinstance(val, bytes):
                if len(val) < 262 or val[:6] != b'\x05TIcon' or key is not None:
                    raise ValueError('无效或重复的 IconKeyImage。')
                key = val[6:262]
        while node(depth+1):
            pass
        return True
    if not node(0) or key is None:
        raise ValueError('TFORM1 缺少 IconKeyImage 密钥。')
    return key


def ensure_unicode(image):
    """Follow the loader's VMT, BOM decoder, and UTF-16 factory call graph."""
    pe = PE(image)
    if pe.machine != 0x14c or pe.magic != 0x10b:
        raise ValueError('Unicode 引擎检查要求 PE32。')
    def match(p, signature):
        parts = signature.split()
        return 0 <= p <= len(image)-len(parts) and all(t == '??' or image[p+i] == int(t, 16) for i, t in enumerate(parts))
    def require(p, signature):
        if not match(p, signature):
            raise ValueError('Unicode 加载器结构不匹配。')
    def call(p):
        if image[p] != 0xe8:
            raise ValueError('缺少相对 CALL。')
        return pe.off_va((pe.va(p)+5+struct.unpack('<i', pe.take(p+1, 4))[0]) & 0xffffffff)
    def ptr(va):
        return pe.u32(pe.off_va(va))
    needed = {'CreateFontIndirectW', 'ExtTextOutW', 'EnumFontFamiliesExW'}
    p = pe.offset(pe.u32(pe.directory+8))
    for _ in range(1024):
        if not pe.u32(p+12):
            break
        thunk = pe.offset(pe.u32(p) or pe.u32(p+16))
        for i in range(65536):
            entry = pe.u32(thunk+4*i)
            if not entry:
                break
            if not entry & 0x80000000:
                at = pe.offset(entry)+2
                end = image.find(b'\0', at)
                if end < at or end-at > 1024:
                    raise ValueError('无效导入名称。')
                needed.discard(image[at:end].decode('ascii'))
        p += 20
    if needed:
        raise ValueError('缺少 Unicode 字体导入。')
    def loader(p):
        vmt = ptr(pe.u32(p+3))
        wrapper, load = pe.off_va(ptr(vmt+0x70)), pe.off_va(ptr(vmt+0x74))
        require(wrapper, '53 33 C9 8B 18 FF 53 74 5B C3')
        require(load+0x70, '8D 55 F4 8B 45 FC E8 ?? ?? ?? ?? 8B D8 8B 45 FC E8 ?? ?? ?? ?? 2B C3 50 8D 45 F0 50 8B CB 8B 55 FC 8B 45 F4 E8 ?? ?? ?? ?? 8B 55 F0 8B 45 F8 8B 08 FF 51 2C')
        bom = call(load+0x76)
        require(bom+0x1f, '33 F6 83 3B 00 0F 85 ?? ?? ?? ?? E8 ?? ?? ?? ?? 8D 55 F8 8B 08 FF 51 18 8B 55 F8 8B C7 E8 ?? ?? ?? ?? 84 C0 74 09 E8 ?? ?? ?? ?? 89 03 EB 4F')
        factory = call(bom+0x2a)
        if call(bom+0x45) != factory:
            raise ValueError('Unicode 编码实例不一致。')
        require(factory, '53 56 83 3D ?? ?? ?? ?? 00 75 27 B2 01 A1 ?? ?? ?? ?? E8 ?? ?? ?? ?? 8B D8 BE ?? ?? ?? ?? 6A 00 53 56 E8 ?? ?? ?? ??')
        unicode = ptr(pe.u32(factory+0xe))
        require(pe.off_va(ptr(unicode+0x18)), '53 8B DA 6A 02 8B C3 B9 01 00 00 00 8B 15 ?? ?? ?? ?? E8 ?? ?? ?? ?? 83 C4 04 8B 03 C6 00 FF 8B 03 C6 40 01 FE 5B C3')
        require(pe.off_va(ptr(unicode+0xc)), '55 8B EC 53 8B 45 08 8B D8 8B C8 03 C9 8B C2 8B 55 0C E8 ?? ?? ?? ?? 8B C3 5B 5D C2 08 00')
    pattern = b'\xb2\x01\xa1....\xe8....\x89\x45\xec\x8b\xd6\x8b\x45\xec\x8b\x08\xff\x51\x70\x8b\xc6\xe8....'
    for va, size, raw, flags in pe.sections:
        if not flags & 0x20000000:
            continue
        for found in re.finditer(pattern, image[raw:raw+size], re.DOTALL):
            try:
                loader(raw+found.start())
                return
            except (ValueError, IndexError, struct.error):
                continue
    raise ValueError('原程序未通过 QLIE Unicode 运行时检查；拒绝向未知或 ANSI 引擎写入 UTF-16 译文。')
