"""Verified v31 display literals; preserve literal indexes and nonliteral tables."""
import hashlib
import struct

TABLES_HASH = '0a1ddbb1c993470ed5094755f8574fc4c1de9570c6678bd4a265dc5005631daa'


class Metadata:
    def __init__(self, raw, selection):
        self.raw, self.selection = raw, selection
        def integer(at): return struct.unpack_from('<i', raw, at)[0]
        if len(raw) < 256 or raw[:8] != struct.pack('<II', 0xFAB11BAF, 31):
            raise ValueError('未支持的 IL2CPP 元数据版本')
        self.table, table_size, self.pool, self.pool_size = struct.unpack_from('<iiii', raw, 8)
        if self.table != 256 or table_size != 17876 * 8 or self.pool != self.table + table_size or self.pool_size < 0 or self.pool + self.pool_size > len(raw):
            raise ValueError('IL2CPP 字面量表布局不匹配')
        end = previous = self.pool + self.pool_size
        for at in range(24, 256, 8):
            offset, length = integer(at), integer(at + 4)
            if length < 0 or offset < previous or offset + length > len(raw):
                raise ValueError('IL2CPP 元数据表边界或顺序无效')
            previous = offset + length
        if hashlib.sha256(raw[end:]).hexdigest() != TABLES_HASH:
            raise ValueError('IL2CPP 元数据与已审核的显示文字规则不匹配')
        self.texts = {}
        for index in range(table_size // 8):
            length, offset = struct.unpack_from('<ii', raw, self.table + index * 8)
            if length < 0 or offset < 0 or offset + length > self.pool_size:
                raise ValueError('IL2CPP 字面量越界')
            if str(index) in selection:
                self.texts[index] = raw[self.pool + offset:self.pool + offset + length].decode('utf-8')

    def units(self, path, version):
        for index, text in self.texts.items():
            context = self.selection[str(index)]
            dialogue = context.startswith('新手') or (version == '5d49d9aca8ffa194' and context.startswith('角色对白'))
            result = dict(id=f'{path}::literal:{index}', resourcePath=path, locator=f'StringLiteral:{index}',
                          sourceText=text, context='IL2CPP / ' + context, speaker=None,
                          kind='dialogue' if dialogue else 'description')
            tokens = ['##', '\n•'] + ([] if version == 'fd8e47622d43624d' else ['//', '((x0.50))'])
            tokens = [token for token in tokens if token in text]
            if tokens: result['protectedTokens'] = tokens
            yield result

    def rebuild(self, path, translations):
        added, updates = bytearray(), []
        for index, original in self.texts.items():
            text = translations.get(f'{path}::literal:{index}', original)
            if text == original: continue
            if not isinstance(text, str) or not text.strip() or '\0' in text:
                raise ValueError('字面量译文为空或含空字符')
            data = text.encode('utf-8')
            updates.append((index, self.pool_size + len(added), len(data)))
            added.extend(data)
        if not updates: return self.raw
        added.extend(bytes((-(self.pool_size + len(added))) % 4))
        end = self.pool + self.pool_size
        result = bytearray(self.raw[:end]) + added + self.raw[end:]
        struct.pack_into('<i', result, 20, self.pool_size + len(added))
        for at in range(24, 256, 8):
            struct.pack_into('<i', result, at, struct.unpack_from('<i', self.raw, at)[0] + len(added))
        for index, offset, length in updates:
            struct.pack_into('<ii', result, self.table + index * 8, length, offset)
        Metadata(result, self.selection)
        return bytes(result)
