"""Preserve MESSAGE v4 shared slots, opaque bytes, voice data and trailing data."""
from collections import Counter
from dataclasses import dataclass, field
import json
from pathlib import Path
import struct

MAGIC = b'[SCR-MESSAGE]ver4.0'


class LosslessGbk:
    def __init__(self):
        self.extra = {bytes.fromhex(key): value for key, value in
                      json.loads(Path(__file__).with_name('cp936-extra.json').read_text('utf-8')).items()}
        self.reverse = {value: key for key, value in self.extra.items()}

    def decode(self, data):
        result = []
        at = 0
        while at < len(data):
            count = 2 if 0x81 <= data[at] <= 0xfe and at + 1 < len(data) and 0x40 <= data[at + 1] <= 0xfe and data[at + 1] != 0x7f else 1
            raw = data[at:at + count]
            text = self.extra.get(raw)
            if text is None:
                try:
                    value = raw.decode('gbk')
                    if value.encode('gbk') == raw:
                        text = value
                except UnicodeError:
                    pass
            if text is None:
                text = chr(0xf0000 + data[at])
                count = 1
            result.append(text)
            at += count
        return ''.join(result)

    def encode(self, text):
        return b''.join(bytes([ord(char) - 0xf0000]) if opaque(char) else
                        self.reverse.get(char) or char.encode('gbk') for char in text)

    def check(self, original, text):
        available = Counter(char for char in original if opaque(char))
        for char in text:
            scalar = ord(char)
            if opaque(char):
                if available[char] == 0:
                    raise ValueError('译文不可新增原始字节标记。')
                available[char] -= 1
            elif scalar == 8:
                pass
            elif scalar in (0, 0xfffd, 0x7f) or scalar < 32 and scalar not in (9, 10, 13):
                raise ValueError('译文包含不支持的控制字符或替代符。')
        if original.count('\b') != text.count('\b'):
            raise ValueError('请保留原文中 0x08 控制字符的数量。')
        before, after = self.encode(original), self.encode(text)
        for trail in (0x40, 0x41):
            if reserved(after, trail) > reserved(before, trail):
                raise ValueError('译文新增了引擎保留编码 F040/F041，请改用其他字符。')
        if self.decode(after) != text:
            raise ValueError('译文与保留原始字节发生了编码边界冲突。')


def opaque(char):
    return 0xf0000 <= ord(char) <= 0xf00ff


def reserved(data, trail):
    count = at = 0
    while at < len(data):
        if data[at] >= 0x80 and at + 1 < len(data):
            count += data[at:at + 2] == bytes([0xf0, trail])
            at += 2
        else:
            at += 1
    return count


@dataclass
class Slot:
    key: str
    kind: str
    index: int
    start: int
    end: int
    suffix: int
    raw: bytes
    text: str = ''
    references: int = 0
    names: set = field(default_factory=set)


class Message:
    def __init__(self, data, codec):
        if not data.startswith(MAGIC) or len(data) < 25:
            raise ValueError('消息表签名不匹配。')
        self.data, self.codec = data, codec
        self.key = data[20] if data[19] else 0
        self.slots = []
        at = 21

        def take(fmt):
            nonlocal at
            values = struct.unpack_from(fmt, data, at)
            at += struct.calcsize(fmt)
            return values[0]

        def count():
            value = take('<i')
            if value < 0 or value > len(data) // 2:
                raise ValueError('消息表计数无效。')
            return value

        by_kind = {}
        for kind in ('name', 'choice', 'dialogue'):
            slots = by_kind[kind] = []
            for index in range(count()):
                start = at
                if kind == 'dialogue':
                    length = count()
                    end = at + length
                    if end > len(data):
                        raise ValueError('消息长度越界。')
                    decoded = xor(data[at:end], self.key)
                    text_length = struct.unpack_from('<i', decoded)[0]
                    if text_length < 0 or 4 + text_length >= len(decoded):
                        raise ValueError('消息文本长度越界。')
                    raw = decoded[4:4 + text_length]
                    suffix = at + 4 + text_length
                    cursor = 4 + text_length
                    voices = decoded[cursor]
                    cursor += 1
                    for _ in range(voices):
                        while decoded[cursor:cursor + 2] != b'\0\0':
                            cursor += 2
                            if cursor + 2 > len(decoded):
                                raise ValueError('消息语音字符串截断。')
                        cursor += 2
                    if cursor != len(decoded):
                        raise ValueError('消息长度不一致。')
                    at = end
                else:
                    length = take('<h')
                    if length < 0 or at + length > len(data):
                        raise ValueError('姓名或选项长度越界。')
                    raw = xor(data[at:at + length], self.key)
                    at += length
                    suffix = 0
                slot = Slot(f'{kind}:{index}', kind, index, start, at, suffix, raw,
                            references=int(kind == 'choice'))
                slots.append(slot)
                self.slots.append(slot)
        for _ in range(count()):
            name = take('<i')
            if name >= 0:
                by_kind['name'][name].references += 1
            for _ in range(take('<B')):
                message = by_kind['dialogue'][take('<i')]
                message.references += 1
                message.names.add(name)
        self.parsed_end = at
        for slot, text in zip(self.slots, codec.decode_many([slot.raw for slot in self.slots])):
            slot.text = text

    def build(self, edits):
        if set(edits) - {slot.key for slot in self.slots}:
            raise ValueError('译文包含未知文本槽。')
        changed = [slot for slot in self.slots if slot.key in edits and edits[slot.key] != slot.text]
        texts = []
        for slot in changed:
            if not slot.references:
                raise ValueError('不能修改未引用的文本。')
            self.codec.check(slot.text, edits[slot.key])
            texts.append(edits[slot.key])
        encoded = self.codec.encode_many(texts)
        if self.codec.decode_many(encoded) != texts:
            raise ValueError('文本无法无损编码。')
        result = bytearray()
        cursor = 0
        for slot, raw in zip(changed, encoded):
            result += self.data[cursor:slot.start]
            if slot.kind == 'dialogue':
                suffix = self.data[slot.suffix:slot.end]
                result += struct.pack('<i', 4 + len(raw) + len(suffix))
                result += xor(struct.pack('<i', len(raw)) + raw, self.key) + suffix
            else:
                if len(raw) > 32767:
                    raise ValueError('姓名或选项超过格式长度限制。')
                result += struct.pack('<h', len(raw)) + xor(raw, self.key)
            cursor = slot.end
        return bytes(result) + self.data[cursor:]


def xor(data, key):
    return bytes(byte ^ key for byte in data)


LosslessGbk.decode_many = lambda self, values: [self.decode(value) for value in values]
LosslessGbk.encode_many = lambda self, texts: [self.encode(text) for text in texts]
