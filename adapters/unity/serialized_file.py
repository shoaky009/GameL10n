"""Unity 2022 SerializedFile v22, preserving type tables and object identities.

This handles ordinary serialized assets, not UnityFS bundles. Object payloads
are opaque unless an adapter supplies a verified replacement.
"""
from dataclasses import dataclass
from collections import Counter
from pathlib import Path
import json
import struct
import sys


class Reader:
    def __init__(self, data, position=0):
        self.data, self.pos = data, position

    def take(self, size):
        if size < 0 or self.pos + size > len(self.data):
            raise ValueError(f"序列化字段越界 @{self.pos} + {size}")
        result = self.data[self.pos:self.pos + size]
        self.pos += size
        return result

    def skip(self, size):
        if size < 0 or self.pos + size > len(self.data):
            raise ValueError(f"序列化字段越界 @{self.pos} + {size}")
        self.pos += size

    def number(self, fmt):
        return struct.unpack('<' + fmt, self.take(struct.calcsize('<' + fmt)))[0]

    def i32(self):
        return self.number('i')

    def align(self, boundary=4):
        self.skip((-self.pos) % boundary)

    def cstring(self):
        end = self.data.find(b'\0', self.pos)
        if end < 0:
            raise ValueError('序列化字符串缺少终止符')
        result = self.take(end - self.pos).decode('utf-8')
        self.skip(1)
        return result

    def string(self):
        result = self.take(self.i32()).decode('utf-8')
        self.align()
        return result

    def boolean(self):
        value = self.i32()
        if value not in (0, 1):
            raise ValueError('序列化布尔字段不匹配')
        return value

    def count(self, stride=1):
        value = self.i32()
        if value < 0 or value * stride > len(self.data) - self.pos:
            raise ValueError('序列化数组越界')
        return value

    def array(self, stride):
        self.skip(self.count(stride) * stride)

    def ptr(self):
        return self.i32(), self.number('q')


def unity_string(value):
    raw = value.encode('utf-8')
    return struct.pack('<i', len(raw)) + raw + bytes((-len(raw)) % 4)


@dataclass(frozen=True)
class Object:
    path_id: int
    class_id: int
    start: int
    size: int
    offset_field: int


@dataclass(frozen=True)
class TextAsset:
    object: Object
    name: str
    script: bytes
    raw: bytes
    script_offset: int
    tail_offset: int

    def replace_script(self, script):
        if not isinstance(script, bytes):
            raise TypeError('TextAsset 内容必须为 bytes，编码由适配器确认')
        if script == self.script:
            return self.raw
        return (self.raw[:self.script_offset] + struct.pack('<i', len(script))
                + script + bytes((-len(script)) % 4) + self.raw[self.tail_offset:])


class SerializedFile:
    def __init__(self, path):
        self.path = Path(path)
        self.data = self.path.read_bytes()
        raw = self.data
        if len(raw) < 64 or struct.unpack_from('>I', raw, 8)[0] != 22 or raw[16] != 0:
            raise ValueError('仅支持小端 Unity SerializedFile v22')
        metadata_size, file_size, self.data_offset = struct.unpack_from('>IQQ', raw, 20)
        if file_size != len(raw) or not 48 + metadata_size <= self.data_offset <= len(raw):
            raise ValueError('Unity 文件头边界不匹配')
        r = Reader(raw[:48 + metadata_size], 48)
        self.unity_version = r.cstring()
        if not self.unity_version.startswith('2022.'):
            raise ValueError('仅支持已验证的 Unity 2022 序列化布局')
        r.i32()  # build target
        tree = r.number('B')
        if tree not in (0, 1):
            raise ValueError('Unity 类型树标记无效')

        def read_type(reference=False):
            class_id = r.i32()
            r.skip(1)
            script_index = r.number('h')
            if class_id == 114 or (reference and script_index >= 0):
                r.skip(16)
            r.skip(16)
            if tree:
                nodes = r.count(32)
                size = r.i32()
                r.skip(nodes * 32)
                r.skip(size)
                if reference:
                    for _ in range(3):
                        r.cstring()
                else:
                    r.array(4)
            return class_id

        types = [read_type() for _ in range(r.count(23))]
        self.objects = {}
        for _ in range(r.count(24)):
            r.align()
            pid = r.number('q')
            offset_field = r.pos
            start = r.number('q') + self.data_offset
            size = r.number('I')
            type_id = r.i32()
            if pid in self.objects or not 0 <= type_id < len(types) or start < self.data_offset or start + size > len(raw):
                raise ValueError('Unity 对象表无效或重复')
            self.objects[pid] = Object(pid, types[type_id], start, size, offset_field)
        for _ in range(r.count(12)):
            r.i32()
            r.align()
            r.skip(8)
        self.externals = []
        for _ in range(r.count(22)):
            r.cstring()
            r.skip(20)
            self.externals.append(r.cstring())
        for _ in range(r.count(23)):
            read_type(True)
        r.cstring()
        if r.pos != 48 + metadata_size:
            raise ValueError('Unity 元数据长度不匹配')
        last = self.data_offset
        for obj in sorted(self.objects.values(), key=lambda item: item.start):
            if obj.start < last:
                raise ValueError('Unity 对象数据重叠')
            last = obj.start + obj.size

    def raw(self, obj):
        if isinstance(obj, int):
            obj = self.objects[obj]
        return self.data[obj.start:obj.start + obj.size]

    def text_asset(self, obj):
        if isinstance(obj, int):
            obj = self.objects[obj]
        if obj.class_id != 49:
            raise ValueError('指定对象不是 TextAsset')
        raw = self.raw(obj)
        reader = Reader(raw)
        name = reader.string()
        script_offset = reader.pos
        script = reader.take(reader.i32())
        reader.align()
        return TextAsset(obj, name, script, raw, script_offset, reader.pos)

    def text_assets(self):
        for obj in self.objects.values():
            if obj.class_id == 49:
                yield self.text_asset(obj)

    def rebuild_text_assets(self, replacements):
        return self.rebuild({pid: self.text_asset(pid).replace_script(script)
                             for pid, script in replacements.items()})

    def rebuild(self, replacements):
        changed = {pid: value for pid, value in replacements.items() if value != self.raw(pid)}
        if not changed:
            return self.data
        result = bytearray(self.data[:self.data_offset])
        previous = self.data_offset
        for obj in sorted(self.objects.values(), key=lambda item: item.start):
            # Preserve opaque inter-object bytes, then satisfy the original alignment.
            gap = self.data[previous:obj.start]
            if any(gap):
                result.extend(gap)
            result.extend(bytes((obj.start - len(result)) % 8))
            value = changed.get(obj.path_id, self.raw(obj))
            struct.pack_into('<qI', result, obj.offset_field, len(result) - self.data_offset, len(value))
            result.extend(value)
            previous = obj.start + obj.size
        result.extend(self.data[previous:])
        struct.pack_into('>Q', result, 24, len(result))
        return bytes(result)


class UnitySet:
    def __init__(self, root, paths):
        self.assets = {path: SerializedFile(Path(root) / path) for path in paths}
        self.scripts = {}
        for path, asset in self.assets.items():
            scripts = self.scripts[path] = {}
            for obj in asset.objects.values():
                if obj.class_id == 115:
                    r = Reader(asset.raw(obj))
                    r.string()
                    r.skip(20)
                    cls, namespace = r.string(), r.string()
                    scripts[obj.path_id] = namespace + '.' + cls

    def resolve(self, source, file_id):
        if file_id == 0:
            return source
        externals = self.assets[source].externals
        if not 1 <= file_id <= len(externals):
            raise ValueError('Unity 外部引用索引错误')
        name = externals[file_id - 1].replace('\\', '/').rsplit('/', 1)[-1]
        near = (Path(source).parent / name).as_posix().casefold()
        matches = [p for p in self.assets if p.replace('\\', '/').casefold() == near]
        if not matches:
            matches = [p for p in self.assets if Path(p).name.casefold() == name.casefold()]
        if len(matches) != 1:
            raise ValueError('缺少或有歧义的 Unity 依赖：' + name)
        return matches[0]

    def script(self, path, raw):
        if len(raw) < 28:
            return ''
        file_id, pid = struct.unpack_from('<iq', raw, 16)
        return self.scripts[self.resolve(path, file_id)].get(pid, '')


def probe(path, *, query='', offset=0, limit=8):
    """Inspect a container and a bounded page of names without returning scripts."""
    if not isinstance(query, str) or len(query) > 256:
        raise ValueError('query 必须为至多 256 字符的名称查询')
    if type(offset) is not int or offset < 0 or type(limit) is not int or not 1 <= limit <= 32:
        raise ValueError('offset 必须为非负整数，limit 必须为 1..32')
    if Path(path).stat().st_size > 512 * 1024 * 1024:
        raise ValueError('探测单个资源不得超过 512 MiB')
    asset = SerializedFile(path)
    selected = []
    count = matches = 0
    for text in asset.text_assets():
        count += 1
        if query.casefold() in text.name.casefold():
            if offset <= matches < offset + limit:
                selected.append(dict(pathId=text.object.path_id, name=text.name,
                                     scriptBytes=len(text.script)))
            matches += 1
    return dict(status='matched', matched=True, format='unity-serialized-file',
                serializedVersion=22, unityVersion=asset.unity_version,
                objectCount=len(asset.objects),
                classCounts=dict(Counter(obj.class_id for obj in asset.objects.values())),
                textAssetCount=count, totalMatches=matches, offset=offset,
                nextOffset=min(matches, offset + len(selected)),
                more=offset + len(selected) < matches, textAssets=selected,
                scope='仅确认容器与 TextAsset 字节布局，不确认文本语义、MonoBehaviour 或游戏运行。')


def main():
    try:
        request = json.load(sys.stdin)
        if not isinstance(request, dict) or request.get('operation', 'probe') != 'probe':
            raise ValueError('仅支持 probe 操作')
        result = probe(request['input'], query=request.get('query', ''),
                       offset=request.get('offset', 0), limit=request.get('limit', 8))
    except (ValueError, KeyError, TypeError) as error:
        result = dict(status='rejected', matched=False, error=str(error))
    except OSError as error:
        print(json.dumps(dict(status='error', matched=False, error=str(error)), ensure_ascii=False))
        return 1
    print(json.dumps(result, ensure_ascii=False))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
