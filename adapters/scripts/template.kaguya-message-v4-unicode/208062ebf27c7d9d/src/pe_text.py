"""Preserve numeric PE resource leaves and their stable text locators."""
from dataclasses import dataclass
import struct
from pe import Pe, align

DIALOGS = {101, 102, 104, 105, 106, 107, 108, 109, 111, 112, 113, 114, 115, 209, 210, 211, 212}


@dataclass
class Field:
    key: str
    text: str
    context: str


class PeText:
    def __init__(self, data):
        self.pe, self.leaves, self.fields = Pe(data), [], []
        rva = self.pe.u32(self.pe.opt + 112)
        if not rva:
            return
        root = self.pe.offset(rva)

        def walk(at, keys):
            for pos in range(at + 16, at + 16 + 8 * (self.pe.u16(at + 12) + self.pe.u16(at + 14)), 8):
                key, value = self.pe.u32(pos), self.pe.u32(pos + 4)
                if key & 0x80000000:
                    continue
                path = keys + [key]
                if value & 0x80000000:
                    if len(path) > 3:
                        raise ValueError('PE资源树层数异常。')
                    walk(root + (value & 0x7fffffff), path)
                elif len(path) == 3 and (path[:2] == [4, 128] or path[0] == 5 and path[1] in DIALOGS):
                    entry = root + value
                    start = self.pe.offset(self.pe.u32(entry))
                    size = self.pe.u32(entry + 4)
                    if start + size > len(self.pe.data):
                        raise ValueError('PE资源越界。')
                    self.leaves.append((*path, entry, bytes(self.pe.data[start:start + size])))
        walk(root, [])
        for leaf in self.leaves:
            self.transform(leaf, {}, False)

    def transform(self, leaf, edits, font):
        kind, identifier, language, _, data = leaf
        pos, output = 0, bytearray()

        def read(fmt):
            nonlocal pos
            value = struct.unpack_from(fmt, data, pos)[0]
            pos += struct.calcsize(fmt)
            return value

        def raw(size):
            nonlocal pos
            if pos + size > len(data):
                raise ValueError('PE资源越界。')
            output.extend(data[pos:pos + size])
            pos += size

        def word(value):
            output.extend(struct.pack('<H', value))

        def pad():
            nonlocal pos
            old, new = align(pos, 4) - pos, align(len(output), 4) - len(output)
            if old == new:
                raw(old)
            else:
                pos += old
                output.extend(bytes(new))

        def value(key=None, context='', ordinal=True, replacement=None):
            start = pos
            first = read('<H')
            if first == 0xffff and ordinal:
                read('<H')
                output.extend(data[start:pos])
                return ''
            while first:
                first = read('<H')
            text = data[start:pos - 2].decode('utf-16-le')
            next_text = replacement if replacement is not None else edits.get(key, text) if key else text
            if key and text:
                if not any(field.key == key for field in self.fields):
                    self.fields.append(Field(key, text, context))
                if '\0' in next_text or '\ufffd' in next_text:
                    raise ValueError('界面文本包含空字符或替代符。')
            output.extend((next_text + '\0').encode('utf-16-le'))
            return text

        prefix = f'pe:{"menu" if kind == 4 else "dialog"}:{identifier}:{language}'
        if kind == 4:
            version, offset = read('<H'), read('<H')
            if version:
                raise ValueError('不支持此菜单版本。')
            word(version)
            word(offset)
            raw(offset)

            def items(path, depth):
                index = 0
                while True:
                    flags = read('<H')
                    word(flags)
                    popup, command = flags & 0x10, 0
                    if not popup:
                        command = read('<H')
                        word(command)
                    route = f'{path}.{index}' if path else str(index)
                    value(prefix + ':item:' + route if depth else None, f'菜单层级 {route}；命令 {command}')
                    if popup:
                        items(route, depth + 1)
                    index += 1
                    if index > 2000 or depth > 16:
                        raise ValueError('菜单结构异常。')
                    if flags & 0x80:
                        break
            items('', 0)
        else:
            ext = data[:4] == b'\x01\0\xff\xff'
            style = struct.unpack_from('<I', data, 12 if ext else 0)[0]
            count = struct.unpack_from('<H', data, 16 if ext else 8)[0]
            raw(26 if ext else 18)
            value()
            value()
            value(prefix + ':title', '对话框标题/属性页标签', False)
            if style & 0x40:
                raw(2)
                if ext:
                    raw(3)
                    charset = read('<B')
                    output.append(134 if font else charset)
                value(replacement='Microsoft YaHei' if font else None)
            for index in range(count):
                pad()
                start = pos
                raw(24 if ext else 18)
                command = struct.unpack_from('<I' if ext else '<H', data, start + (20 if ext else 16))[0]
                value()
                value(prefix + ':control:' + str(index), f'控件序号 {index}；控件 ID {command}')
                size = read('<H')
                word(size)
                raw(size)
        raw(len(data) - pos)
        return bytes(output)

    def build(self, edits):
        known = {field.key: field.text for field in self.fields}
        if set(edits) - known.keys():
            raise ValueError('未知PE文本字段。')
        changed = {leaf[:3] for leaf in self.leaves if any(key.startswith(f'pe:{"menu" if leaf[0] == 4 else "dialog"}:{leaf[1]}:{leaf[2]}:') and text != known[key] for key, text in edits.items())}
        if not changed:
            return bytes(self.pe.data)
        payload, updates = bytearray(), []
        for leaf in self.leaves:
            payload.extend(bytes(align(len(payload), 4) - len(payload)))
            at = len(payload)
            data = self.transform(leaf, edits, leaf[:3] in changed and leaf[0] == 5)
            payload.extend(data)
            updates.append((leaf[3], at, len(data)))
        pe = self.pe
        sh = pe.section('.l10nui')
        if sh is None:
            rva = pe.add_section(payload, '.l10nui')
        else:
            capacity, raw, rva = pe.u32(sh + 16), pe.u32(sh + 20), pe.u32(sh + 12)
            if len(payload) > capacity:
                rva = pe.next_rva()
                raw = align(len(pe.data), pe.u32(pe.opt + 36))
                capacity = align(len(payload), pe.u32(pe.opt + 36))
                pe.data.extend(bytes(raw + capacity - len(pe.data)))
                for offset, item in ((12, rva), (16, capacity), (20, raw)):
                    pe.w32(sh + offset, item)
            pe.data[raw:raw + capacity] = payload + bytes(capacity - len(payload))
            pe.w32(sh + 8, len(payload))
            pe.w32(pe.opt + 56, pe.next_rva())
        for entry, at, size in updates:
            pe.w32(entry, rva + at)
            pe.w32(entry + 4, size)
        return bytes(pe.data)
