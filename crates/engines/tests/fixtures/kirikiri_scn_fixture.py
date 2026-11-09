"""Synthetic, game-independent PSB trees for KiriKiri format tests."""
import struct
import zlib

from python.engines.kirikiri_psb import array


def psb(tree):
    keys = {}
    strings = {}

    def collect(value):
        if isinstance(value, dict):
            for key, child in value.items():
                keys.setdefault(key, len(keys))
                collect(child)
        elif isinstance(value, list):
            for child in value:
                collect(child)
        elif isinstance(value, str):
            strings.setdefault(value, len(strings))
    collect(tree)
    trie = {}
    for key, index in keys.items():
        node = trie
        for char in key.encode('utf-8'):
            node = node.setdefault(char, {})
        node[0] = index
    a, b, used = [0], [0xffffffff], {0}
    pending = [(0, trie)]
    while pending:
        index, children = pending.pop()
        base = 1
        while any(base + char in used for char in children):
            base += 1
        size = base + max(children) + 1
        a.extend([0] * max(0, size - len(a)))
        b.extend([0xffffffff] * max(0, size - len(b)))
        a[index] = base
        for char, child in children.items():
            at = base + char
            used.add(at)
            b[at] = index
            if char == 0:
                a[at] = child
            else:
                pending.append((at, child))

    def encode(value):
        if value is None:
            return b'\x01'
        if isinstance(value, int):
            return b'\x04' if value == 0 else b'\x06' + value.to_bytes(2, 'little', signed=True)
        if isinstance(value, str):
            return b'\x16' + strings[value].to_bytes(2, 'little')
        values = value.values() if isinstance(value, dict) else value
        pieces = [encode(child) for child in values]
        offsets = []
        at = 0
        for piece in pieces:
            offsets.append(at)
            at += len(piece)
        if isinstance(value, dict):
            return b'\x21' + array([keys[key] for key in value]) + array(offsets) + b''.join(pieces)
        return b'\x20' + array(offsets) + b''.join(pieces)

    names = array(a) + array(b)
    body = encode(tree)
    offsets, pool = [], bytearray()
    for text in strings:
        offsets.append(len(pool))
        pool.extend(text.encode('utf-8') + b'\0')
    index = array(offsets)
    root_at = 44 + len(names)
    strings_at = root_at + len(body)
    pool_at = strings_at + len(index)
    co = pool_at + len(pool)
    cl = co + len(array([]))
    cd = cl + len(array([]))
    header = bytearray(b'PSB\0\x03\0\0\0' + struct.pack('<9I', 44, 44, strings_at, pool_at, co, cl, cd, root_at, 0))
    struct.pack_into('<I', header, 40, zlib.adler32(header[8:40]) & 0xffffffff)
    return bytes(header) + names + body + index + pool + array([]) + array([])
