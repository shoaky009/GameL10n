"""Synthetic PSB reference aliasing, SCN derived-field and language checks."""
import importlib.util
import struct
import sys
import zlib

from python.engines.kirikiri_psb import Psb, array

spec = importlib.util.spec_from_file_location('component', sys.argv[1])
component = importlib.util.module_from_spec(spec)
spec.loader.exec_module(component)


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


message = r'Hello\n%f123;world.'
cache = 'Helloworld.'
row = ['actor-id', [[None, message, len(cache), cache, cache]], None, None, None]
named_row = ['actor-id', [['Alice', message, len(cache), cache, cache]], None, None, None]
tree = dict(name='sample', instructions='do-not-translate', scenes=[dict(title='Opening', texts=[row, named_row],
            selects=[dict(text='Continue'), dict(selidx=1, storage='sample', target='*end')])])
raw = psb(tree)
scene = component.SceneText(raw)
paths = {p: t for p, t, _, _ in scene.fields}
body = 'scenes/0/texts/0/1/0/1'
assert len(paths) == 5 and paths[body] == message
assert 'scenes/0/texts/0/0' not in paths  # Internal speaker ID, not display name.
assert 'scenes/0/texts/0/1/0/3' not in paths and len(scene.derived_paths) == 4
assert scene.build(paths) is None
assert component.scn.patch(scene.p, scene.records, [dict(r['row']) for r in scene.records])[0] == raw

translated = r'中文\n%f123;正文。'
after = component.SceneText(scene.build({body: translated}))
assert {p: t for p, t, _, _ in after.fields} == paths | {body: translated}
first = after.records[0]
assert [after.locate(path)[0].value for path in [first['length']]] == [len('中文正文。')]
assert [after.p.text(after.locate(path)[0]) for path in first['caches']] == ['中文正文。'] * 2
assert after.records[1]['row']['message'] == message.replace('\\n', '\n')  # Aliased source strings remain local.

changes = {'scenes/0/title': '开场', 'scenes/0/texts/1/1/0/0': '爱丽丝', 'scenes/0/selects/0/text': '继续'}
changed = component.SceneText(scene.build(changes))
assert {p: t for p, t, _, _ in changed.fields} == paths | changes

def rejects(action):
    try:
        action()
    except ValueError:
        return
    raise AssertionError('invalid SCN edit was accepted')

rejects(lambda: scene.build({'scenes/0/texts/0/1/0/3': 'independent-cache-translation'}))
rejects(lambda: scene.build({body: 'control token removed'}))
bad = scene.p.patch({scene.records[0]['message']: 'desynchronized body'})
rejects(lambda: component.SceneText(bad))

# A different language is selected explicitly; unselected slot IDs/data remain intact.
row[1].append([None, 'Bonjour', 7, 'Bonjour', 'Bonjour'])
tree['scenes'][0]['texts'] = [row]
tree['scenes'][0]['selects'] = [dict(language=[dict(text='Continue'), dict(text='Continuer')])]
multilingual = psb(tree)
french = component.SceneText(multilingual, language_index=1)
french_path = 'scenes/0/texts/0/1/1/1'
result = french.build({french_path: '你好'})
assert component.SceneText(result, language_index=0).records[0]['row']['message'] == message.replace('\\n', '\n')
assert component.SceneText(result, language_index=1).records[0]['row']['message'] == '你好'
print('scn-component-ok')
