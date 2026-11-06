"""Small protocol, path and binary helpers shared by the binary adapters."""
import json
import os
from pathlib import Path
import struct
import sys


def i32(data, offset):
    return struct.unpack_from('<i', data, offset)[0]


def u16(data, offset):
    return struct.unpack_from('<H', data, offset)[0]


def resolve(root, relative):
    root = Path(root).resolve()
    relative = relative.replace('\\', '/')
    if ':' in relative or relative.startswith('/') or any(p in ('.', '..') for p in relative.split('/')):
        raise ValueError('Unsafe resource path: ' + relative)
    target = root.joinpath(relative)
    if not target.resolve().is_relative_to(root):
        raise ValueError('Resource path escapes source directory: ' + relative)
    for parent in (target, *target.parents):
        if parent == root:
            break
        if parent.is_symlink() or (hasattr(parent, 'is_junction') and parent.is_junction()):
            raise ValueError('Resource path traverses a link: ' + relative)
    return target


def files(request):
    return request.get('resourceFiles') or [p.relative_to(request['sourceDirectory']).as_posix()
        for p in Path(request['sourceDirectory']).rglob('*') if p.is_file()]


def save(root, relative, data):
    path = resolve(root, relative)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)


def translations(request):
    value = request.get('translations', {})
    if request.get('translationsFile'):
        value = json.loads(Path(request['translationsFile']).read_text(encoding='utf-8-sig'))
    if isinstance(value, list):
        value = {item['id']: item.get('text', item.get('targetText', item.get('translation'))) for item in value}
    if not isinstance(value, dict):
        raise ValueError('Translations must be an ID to text object or an entry array')
    result = {}
    for key, text in value.items():
        if isinstance(text, dict):
            text = text.get('text')
        if text is not None:
            if not isinstance(text, str):
                raise ValueError('Translation must be a string: ' + key)
            result[key] = text
    return result


def success(field, value):
    return dict(protocolVersion=1, success=True, **{field: value})


def sections(data):
    if len(data) < 64 or data[:2] != b'MZ':
        raise ValueError('Invalid PE image')
    pe = i32(data, 0x3c)
    if pe < 64 or pe + 24 > len(data) or data[pe:pe+4] != b'PE\0\0':
        raise ValueError('Invalid PE header')
    count, optional = u16(data, pe+6), u16(data, pe+20)
    start = pe + 24 + optional
    if not 1 <= count <= 96 or start + count * 40 > len(data):
        raise ValueError('Invalid PE section table')
    result = []
    for offset in range(start, start + count * 40, 40):
        name = data[offset:offset+8].rstrip(b'\0').decode('ascii')
        virtual_size, rva, size, file = struct.unpack_from('<IIII', data, offset+8)
        result.append(dict(name=name, virtualSize=virtual_size, rva=rva, size=size, file=file,
                           flags=struct.unpack_from('<I', data, offset+36)[0]))
    return pe, result


def apply_replacements(data, replacements):
    """Keep unchanged spans and map addresses exactly as VNTextPatch BinaryPatcher."""
    out, previous, mappings = bytearray(), 0, []
    for start, length, value in sorted(replacements):
        if start < previous or length <= 0 or start + length > len(data):
            raise ValueError('Overlapping or invalid script replacement')
        out.extend(data[previous:start])
        mapped_start = len(out)
        out.extend(value)
        if length != len(value):
            mappings.append((start, start+length, mapped_start, len(out)))
        previous = start + length
    out.extend(data[previous:])

    def map_offset(offset):
        if not 0 <= offset <= len(data):
            raise ValueError('Script address is out of bounds')
        delta = 0
        for start, end, new_start, new_end in mappings:
            if offset < start:
                break
            if offset == start:
                return new_start
            if offset < end:
                raise ValueError('Cannot map an address inside rewritten text')
            delta = new_end - end
        return offset + delta
    return out, map_offset


def main(handle):
    request = {}
    try:
        request = json.loads(sys.stdin.read())
        if request.get('protocolVersion', 1) != 1:
            raise ValueError('Unsupported adapter protocol')
        library = (request.get('toolPaths') or {}).get('engineLibrary')
        if library:
            sys.path.insert(0, library)
        response = handle(request)
    except Exception as error:
        response = dict(protocolVersion=1, success=False, error=str(error))
    text = json.dumps(response, ensure_ascii=False)
    if request.get('responseFile'):
        Path(request['responseFile']).write_text(text, encoding='utf-8')
    else:
        sys.stdout.write(text)
