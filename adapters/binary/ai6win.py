"""AI6WIN / Silky MES dialogue, lossless archive updates and CP936 runtime settings.

The MES instruction and text-range rules follow the pinned VNTextPatch adapter
(arcusmaximus, MIT). The shared archive library supplies AI6WIN/LZSS decoding.
"""
import hashlib
from pathlib import Path
import re
import struct

from binary_common import apply_replacements, files, i32, main, resolve, save, sections, success as _success, translations

COVERAGE = '已覆盖剧情归档内的 MES 对白、旁白与人名；data.arc 等其他归档中的图片文字、菜单/系统文本与可执行文件内文本未提取。'
FONT_WARNING = '中文回填依赖 CP936 代码页与 [Font] Face=SimSun、可执行文件字符集 0x86；字形、断行、居中与存档/菜单显示尚未实机验证。'
PATTERN = b'\xc6\x44\x24\x27'


def success(field, value):
    response = _success(field, value)
    if field != 'probeReads':
        response['error'] = None
    return response


def ini_value(data, section, key):
    current, offset, found = None, 0, None
    for line in data.splitlines(keepends=True):
        content = line.rstrip(b'\r\n')
        stripped = content.strip()
        if stripped.startswith(b'[') and stripped.endswith(b']'):
            current = stripped[1:-1].strip().lower()
        elif current == section.lower().encode() and b'=' in content and not stripped.startswith((b';', b'#')):
            before, value = content.split(b'=', 1)
            if before.strip().lower() == key.lower().encode():
                if found is not None:
                    raise ValueError(f'AI6WIN.ini 中的 [{section}] {key} 重复出现，已拒绝自动修改。')
                start = offset + len(before)+1+len(value)-len(value.lstrip(b' \t'))
                found = start, len(value.strip(b' \t'))
        offset += len(line)
    return found


def charset(data):
    _, items = sections(data)
    text = next((s for s in items if s['name'] == '.text'), None)
    if not text:
        raise ValueError('Executable has no .text section')
    return charset_in(data, text['file'], min(len(data), text['file']+text['size']))


def charset_in(data, start, end):
    found = []
    for value in (0x80, 0x86):
        pattern = PATTERN+bytes([value])
        position = data.find(pattern, start, end)
        if position >= 0:
            if data.find(pattern, position+1, end) >= 0:
                raise ValueError('字体字符集设置在可执行文件中出现多次，已拒绝自动修改。')
            found.append(position+4)
    if len(found) != 1:
        raise ValueError('无法唯一确定 AI6WIN 字体字符集设置。')
    return found[0]


def member_key(name):
    return name.lower() if re.fullmatch(r'[A-Za-z0-9._-]+', name) else hashlib.sha256(name.encode()).hexdigest()[:32]


class Mes:
    def __init__(self, data, mode='cp932'):
        self.data, self.mode, self.ranges, self.addresses = data, mode, [], []
        count = i32(data, 0)
        self.code_offset = 4+count*4
        if not count:
            return
        if count < 0 or self.code_offset > len(data):
            raise ValueError('MES message table bounds')
        first = self.code_offset + i32(data, 4)
        self.plus = not (0 <= first <= len(data)-5 and data[first:first+5] == b'\x19\0\0\0\0')
        if self.plus:
            special = i32(data, 4)
            if special < 0:
                raise ValueError('MES special message count')
            self.code_offset = 8+4*(count+special)
            self.addresses = [(p, '<I') for p in range(8, self.code_offset, 4)]
        else:
            self.addresses = [(p, '<I') for p in range(4, self.code_offset, 4)]
        self.escape = 0x1c if self.plus else 0x1b
        message_start, ruby, stack = -1, False, []
        cursor = self.code_offset
        while cursor < len(data):
            start = cursor
            opcode, operand, cursor = self.instruction(cursor, True)
            if opcode in (0x0a, 0x0b):
                if message_start < 0:
                    message_start = start
            elif opcode == self.escape:
                if operand == 1:
                    ruby = True
            elif opcode == 0 and ruby:
                ruby = False
            elif opcode == 0x32 and start+5 < len(data) and data[start+5] == 0xff:
                pass
            elif opcode in (0xff, 0xfc, 0xfd):
                pass
            else:
                if message_start >= 0:
                    self.ranges.append((message_start, start-message_start, 'dialogue'))
                message_start, ruby = -1, False
            if opcode in (0x32, 0x33):
                stack.append(operand)
            elif opcode == 0x34 and len(stack) >= 2:
                a, b = stack.pop(), stack.pop()
                if isinstance(a, int) and isinstance(b, int):
                    stack.append(a+b)
            elif (opcode == 0x18 and len(stack) == 3 and isinstance(stack[-1], int)
                    and isinstance(stack[-2], int) and (stack[-1], stack[-2]) in
                    ([(29, 11), (29, 15)] if self.plus else [(31, 15)]) and isinstance(stack[0], tuple)):
                start, length = stack[0]
                self.ranges.append((start-1, length+1, 'name'))
                stack.clear()
            else:
                stack.clear()

    def instruction(self, position, collect=False):
        data = self.data
        opcode = data[position]
        position += 1
        operand = None
        valid = set(range(0x1c if not self.plus else 0x1d)) | set(range(0x32, 0x44)) | set(range(0xfa, 0x100)) | {0x1d}
        if opcode not in valid or self.plus and opcode == 0x1d:
            raise ValueError(f'Unsupported MES opcode {opcode:02X}')
        addresses = (0x14, 0x15, 0x16, 0x1b) if self.plus else (0x14, 0x15, 0x16, 0x1a)
        integers = (0x19, 0x1a, 0x32) if self.plus else (0x19, 0x32)
        if opcode in addresses or opcode in integers:
            if collect and opcode in addresses:
                self.addresses.append((position, '>I'))
            operand = struct.unpack_from('>i', data, position)[0]
            position += 4
        elif opcode in (0x0a, 0x0b, 0x33):
            end = data.find(b'\0', position)
            if end < 0:
                raise ValueError('Unterminated MES text')
            operand, position = (position, end-position+1), end+1
        elif opcode == self.escape:
            operand = data[position]
            position += 1
        return opcode, operand, position

    def decode(self, value):
        text = value.decode(self.mode)
        return text.replace('·', '・') if self.mode == 'cp936' else text

    def text(self, item):
        start, length, _ = item
        cursor, result = start, []
        while cursor < start+length:
            opcode, operand, cursor = self.instruction(cursor)
            if opcode in (0x0a, 0x0b, 0x33):
                position, size = operand
                value = self.data[position:position+size-1]
                if opcode == 0x0a and self.plus:
                    decoded, p = bytearray(), 0
                    while p < len(value):
                        byte = value[p]
                        p += 1
                        if 0x81 <= byte < 0xa0 or 0xe0 <= byte < 0xf0:
                            decoded.extend((byte, value[p]))
                            p += 1
                        else:
                            decoded.extend(struct.pack('>H', (byte-0x7d62)&0xffff))
                    value = bytes(decoded)
                result.append(self.decode(value))
            elif opcode == self.escape:
                if operand not in (0, 1):
                    raise ValueError('Invalid MES escape sequence')
                result.append('\r\n' if operand == 0 else '[')
            elif opcode == 0:
                result.append(']')
        return ''.join(result)

    def strings(self):
        result = []
        for item in self.ranges:
            text = self.text(item)
            if item[2] == 'dialogue':
                match = re.match(r'^〈(.+?)〉：(.+)', text, re.S)
                if match:
                    result.append((match[1], 'name'))
                    text = match[2]
            result.append((text, item[2]))
        return result

    def code(self, text, kind):
        def encode(value):
            if '\0' in value:
                raise ValueError('MES text cannot contain NUL')
            return value.replace('・', '·').encode('cp936')
        if kind == 'name':
            return b'\x33'+encode(text)+b'\0'
        out = bytearray()
        for index, line in enumerate(text.split('\r\n')):
            if out:
                out.extend((self.escape, 0))
            cursor = 0
            for match in re.finditer(r'\[.+?\]', line):
                if match.start() > cursor:
                    out.extend(b'\x0b'+encode(line[cursor:match.start()])+b'\0')
                out.extend(bytes([self.escape, 1, 0x0b])+encode(match[0][1:-1])+b'\0\0')
                cursor = match.end()
            if cursor < len(line):
                out.extend(b'\x0b'+encode(line[cursor:])+b'\0')
        return bytes(out)

    def build(self, strings):
        iterator, replacements = iter(strings), []
        for start, length, kind in self.ranges:
            text, value_kind = next(iterator)
            if value_kind == 'name' and kind == 'dialogue':
                text = '〈'+text+'〉：'+next(iterator)[0]
            replacements.append((start, length, self.code(text, kind)))
        if next(iterator, None) is not None:
            raise ValueError('Too many strings in translation')
        out, mapping = apply_replacements(self.data, replacements)
        for position, endian in self.addresses:
            original = struct.unpack_from(endian, self.data, position)[0]
            address = mapping(self.code_offset+original)-self.code_offset
            struct.pack_into(endian, out, mapping(position), address)
        return bytes(out)


def detect(request):
    from python.archives import silky_arc as arc
    source = request['sourceDirectory']
    selected = sorted(set(p.replace('\\', '/') for p in files(request)))
    for ini in (p for p in selected if p.lower().endswith('.ini')):
        data = resolve(source, ini).read_bytes()
        value = ini_value(data, 'Script', 'File')
        if not value:
            continue
        start, size = value
        wanted = '/'.join(p for p in data[start:start+size].decode('latin1').replace('\\', '/').strip().split('/') if p)
        archive = next((p for p in selected if p.lower() == wanted.lower()), None)
        if not archive:
            continue
        raw = resolve(source, archive).read_bytes()
        index = arc.open_ai6win(raw)
        if not any(e.name.lower().endswith('.mes') for e in index.entries):
            continue
        for exe in sorted((p for p in selected if p.lower().endswith('.exe')), key=len):
            try:
                image = resolve(source, exe).read_bytes()
                position = charset(image)
            except (ValueError, struct.error):
                continue
            profile = next((p for p in selected if p.lower() == (exe+'.le.config').lower()), None)
            return dict(archive=archive, ini=ini, exe=exe, profile=profile, raw=raw, index=index,
                        mode='cp936' if image[position] == 0x86 else 'cp932', image=image, charset=position)
    raise ValueError('未找到 AI6WIN 剧情归档、配置及唯一字体字符集设置。')


def repack(raw, index, edits):
    from python.archives import silky_arc as arc
    directory = bytearray(struct.pack('<i', len(index.entries)))
    body = bytearray()
    for ordinal, entry in enumerate(index.entries):
        payload = edits.get(entry.name)
        if payload is None:
            stored, size = raw[entry.offset:entry.offset+entry.size], entry.unpacked_size
        else:
            stored = arc.lzss_compress(payload) if payload else b''
            if not stored or len(stored) >= len(payload):
                stored = payload
            size = len(payload)
        directory.extend(raw[4+ordinal*272:4+ordinal*272+260])
        directory.extend(struct.pack('>III', len(stored), size, index.index_size+len(body)))
        body.extend(stored)
    return bytes(directory+body)


def handle(request):
    from python.archives import silky_arc as arc
    operation, source = request['operation'], request['sourceDirectory']
    if operation == 'probe':
        return probe(request)
    layout = detect(request)
    raw, archive, index = layout['raw'], layout['archive'], layout['index']
    members = [e for e in index.entries if e.name.lower().endswith('.mes')]
    if operation == 'extract':
        units, warnings, failures = [], [], 0
        for entry in members:
            try:
                values = Mes(arc.read_member(raw, entry), layout['mode']).strings()
            except Exception as error:
                failures += 1
                if failures <= 5:
                    warnings.append(f'剧情脚本 {entry.name} 解析失败：{error}')
                continue
            for position, (text, kind) in enumerate(values):
                if not text.strip():
                    continue
                unit = dict(id=f'vntp:{member_key(entry.name)}:{position}', resourcePath=archive,
                    locator=f'{entry.name}:string:{position}', sourceText=text,
                    context=f'AI6WIN 剧情脚本 {entry.name} 第 {position+1} 条', speaker=None, kind=kind)
                tokens = [c for c in '[]' if c in text]
                if tokens:
                    unit['protectedTokens'] = tokens
                units.append(unit)
        if failures:
            warnings.insert(0, f'有 {failures} 个剧情脚本未能提取，请检查归档完整性。')
        return success('extraction', dict(units=units, warnings=warnings+[COVERAGE, FONT_WARNING]))
    selected = [archive, layout['ini'], layout['exe']]+([layout['profile']] if layout['profile'] else [])
    if operation == 'build':
        maps, per_member, unknown = translations(request), {}, []
        known = {member_key(entry.name): entry for entry in members}
        cached = {}
        changed = False
        for key, value in maps.items():
            match = re.fullmatch(r'vntp:(.+):(\d+)', key)
            if not match or match[1] not in known:
                unknown.append(key)
                continue
            entry, position = known[match[1]], int(match[2])
            if entry.name not in cached:
                script = Mes(arc.read_member(raw, entry), layout['mode'])
                cached[entry.name] = script, script.strings()
            values = cached[entry.name][1]
            if position >= len(values):
                raise ValueError('译文序号超出脚本范围：'+entry.name)
            per_member.setdefault(entry.name, {})[position] = value
            changed |= bool(value.strip()) and value != values[position][0]
        if unknown:
            raise ValueError(f'有 {len(unknown)} 条译文无法匹配当前剧情脚本（例如 {unknown[0]}）。')
        output, outputs = request['outputDirectory'], []
        warnings = []
        if not changed:
            for path in selected:
                save(output, path, resolve(source, path).read_bytes())
                outputs.append(path)
        else:
            edits = {}
            for entry in members:
                if entry.name not in cached:
                    script = Mes(arc.read_member(raw, entry), layout['mode'])
                    cached[entry.name] = script, script.strings()
                script, values = cached[entry.name]
                replacements = per_member.get(entry.name, {})
                expected = [(replacements.get(i, text) if replacements.get(i, '').strip() else text, kind)
                            for i, (text, kind) in enumerate(values)]
                if not values or expected == values and layout['mode'] == 'cp936':
                    continue
                value = script.build(expected)
                actual = Mes(value, 'cp936').strings()
                if actual != expected:
                    raise ValueError('剧情脚本回填校验失败：'+entry.name)
                edits[entry.name] = value
            save(output, archive, repack(raw, index, edits))
            original = resolve(source, layout['ini']).read_bytes()
            font = ini_value(original, 'Font', 'Face')
            if not font:
                raise ValueError('AI6WIN.ini 缺少 [Font] Face')
            start, size = font
            save(output, layout['ini'], original[:start]+b'SimSun'+original[start+size:])
            image = bytearray(layout['image'])
            image[layout['charset']] = 0x86
            save(output, layout['exe'], image)
            outputs.extend(selected[:3])
            if layout['profile']:
                save(output, layout['profile'], resolve(source, layout['profile']).read_bytes())
                outputs.append(layout['profile'])
        warnings.extend([COVERAGE, '已写入 [Font] Face=SimSun 与可执行文件字符集 0x86（GB2312）；字体字形、断行、居中与存档/菜单显示尚未实机验证。'])
        return success('build', dict(files=outputs, warnings=warnings, launchExecutable=layout['exe']))
    if operation == 'validate':
        issues, output = [], request['outputDirectory']
        def fail(code, message):
            issues.append(dict(code=code, message=message, unitId=None))
        for path in selected:
            if not resolve(output, path).is_file():
                fail('missing-resource', '输出目录缺少资源：'+path)
        if not issues:
            original_ini = resolve(source, layout['ini']).read_bytes()
            out_ini = resolve(output, layout['ini']).read_bytes()
            field = ini_value(original_ini, 'Font', 'Face')
            if out_ini != original_ini and (not field or out_ini != original_ini[:field[0]]+b'SimSun'+original_ini[sum(field):]):
                fail('ini-content', '只允许把 [Font] Face 改为 SimSun。')
            image = bytearray(layout['image'])
            image[layout['charset']] = 0x86
            output_image = resolve(output, layout['exe']).read_bytes()
            if output_image not in (layout['image'], image):
                fail('exe-unexpected', '可执行文件存在字符集以外的修改。')
            built_raw = resolve(output, archive).read_bytes()
            built_index = arc.open_ai6win(built_raw)
            if [e.name for e in index.entries] != [e.name for e in built_index.entries]:
                fail('archive-index', '剧情归档成员顺序或数量变化。')
            else:
                mode = 'cp936' if output_image[layout['charset']] == 0x86 else 'cp932'
                for before, after in zip(index.entries, built_index.entries):
                    if not before.name.lower().endswith('.mes'):
                        if raw[before.offset:before.offset+before.size] != built_raw[after.offset:after.offset+after.size]:
                            fail('non-script-changed', '非剧情成员字节变化：'+before.name)
                        continue
                    old_values = Mes(arc.read_member(raw, before), layout['mode']).strings()
                    new_values = Mes(arc.read_member(built_raw, after), mode).strings()
                    if len(old_values) != len(new_values) or [v[1] for v in old_values] != [v[1] for v in new_values]:
                        fail('mes-shape', '剧情文本数量或类型变化：'+before.name)
            if layout['profile'] and resolve(source, layout['profile']).read_bytes() != resolve(output, layout['profile']).read_bytes():
                fail('locale-changed', '区域模拟配置文件必须保持原样。')
        return success('validation', dict(passed=not issues, issues=issues, runtimeStatus='not-tested'))
    raise ValueError('Unsupported operation: '+operation)


def probe(request):
    """Identify from staged headers and request bounded index/code ranges."""
    source, candidates = request['sourceDirectory'], request.get('probeFiles', [])
    needs, index, archive, ini = [], None, None, None
    ranges = request.get('probeRanges', [])
    def read_range(path, offset, length):
        for item in ranges:
            if item['path'].lower() == path.lower() and item['offset'] <= offset and item['offset']+item['length'] >= offset+length:
                value = Path(item['dataPath']).read_bytes()
                start = offset-item['offset']
                if start+length <= len(value):
                    return value[start:start+length]
        return None
    for item in candidates:
        if not item['path'].lower().endswith('.ini'):
            continue
        data = resolve(source, item['path']).read_bytes()
        value = ini_value(data, 'Script', 'File')
        if value:
            name = '/'.join(p for p in data[value[0]:sum(value)].decode('latin1').replace('\\', '/').split('/') if p)
            candidate = next((p for p in candidates if p['path'].lower() == name.lower()), None)
            if candidate:
                ini, archive = item['path'], candidate['path']
                break
    if not archive:
        return success('probe', None)
    header = resolve(source, archive).read_bytes()
    if len(header) < 4 or not 1 <= i32(header, 0) <= 16384:
        return success('probe', None)
    length = 4+i32(header, 0)*272
    directory = read_range(archive, 0, length)
    if directory is None:
        needs.append(dict(path=archive, offset=0, length=length))
    else:
        from python.archives.silky_arc import decrypt_ai6_name
        names = []
        for offset in range(4, length, 272):
            raw = directory[offset:offset+260].split(b'\0', 1)[0]
            names.append(decrypt_ai6_name(raw).decode('cp932'))
        if not any(name.lower().endswith('.mes') for name in names):
            return success('probe', None)
    exe = None
    for item in sorted((p for p in candidates if p['path'].lower().endswith('.exe')), key=lambda p: len(p['path'])):
        try:
            _, entries = sections(resolve(source, item['path']).read_bytes())
            text = next(s for s in entries if s['name'] == '.text')
            size = min(text['size'], 1<<20)
            value = read_range(item['path'], text['file'], size)
            if value is None:
                needs.append(dict(path=item['path'], offset=text['file'], length=size))
                continue
            charset_in(value, 0, len(value))
            exe = item['path']
            break
        except (ValueError, StopIteration, struct.error):
            continue
    if needs:
        return success('probeReads', needs)
    if not exe:
        raise ValueError('未找到包含 AI6WIN 字体字符集设置的原始可执行文件。')
    required = [archive, ini, exe]+[p['path'] for p in candidates if p['path'].lower() == (exe+'.le.config').lower()]
    return success('probe', dict(engineId='silkys-ai6win-mes', adapterId='silkys-ai6win-mes', adapterVersion='1.0.0',
        description='AI6WIN / Silky MES 剧情脚本（CP932 提取、CP936 中文回填、字体与字符集补丁）',
        requiredFiles=required, warnings=[COVERAGE, FONT_WARNING], originalExecutable=exe))


if __name__ == '__main__':
    main(handle)
