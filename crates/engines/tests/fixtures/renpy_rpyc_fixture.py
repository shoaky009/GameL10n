"""Synthetic inert Ren'Py ASTs; contains no runtime classes or game resources."""
import struct
import zlib


class Raw(bytes):
    pass


def encode(value):
    if isinstance(value, Raw):
        return value
    if value is None:
        return b"N"
    if type(value) is int:
        return b"J" + struct.pack("<i", value)
    if isinstance(value, str):
        data = value.encode("utf-8")
        return b"X" + struct.pack("<I", len(data)) + data
    if isinstance(value, dict):
        return b"}(" + b"".join(encode(k) + encode(v) for k, v in value.items()) + b"u"
    if isinstance(value, (tuple, list)):
        return (b"(" if isinstance(value, tuple) else b"](") + b"".join(map(encode, value)) + (b"t" if isinstance(value, tuple) else b"e")
    raise TypeError(type(value))


def node(kind, line, **fields):
    state = dict(fields, filename="game/story.rpy", linenumber=line, name=("story", 123, line), next=None)
    return Raw(b"crenpy.ast\n" + kind.encode() + b"\n)\x81" + encode((None, state)) + b"b")


def sample(message="こんにちは [player] {b}世界{/b}", code="print('keep original')", second_message=None):
    slots = []
    for transformed in (False, True):
        nodes = [node("TranslateSay" if transformed else "Say", 10, who="e", what=second_message if transformed and second_message else message),
                 node("Say", 11, who=None, what="次の台詞です。"),
                 node("Menu", 20, items=[("選択肢", "flag == 1", [])]),
                 node("TranslateString", 30, language="english", old="Keep lookup key", new="表示文字"),
                 node("Python", 40, code=code)]
        body = encode(({"version": 5003000, "key": "unlocked"}, nodes)) + b"."
        program = b"\x80\x05\x95" + struct.pack("<Q", len(body)) + body
        slots.append(zlib.compress(program))
    offset = 10 + 3 * 12
    table, body = bytearray(), bytearray()
    for number, data in enumerate(slots, 1):
        table.extend(struct.pack("<III", number, offset + len(body), len(data)))
        body.extend(data)
    return b"RENPY RPC2" + table + bytes(12) + body + b"SOURCE-MD5-HASH!"
