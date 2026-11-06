"""System4 AI2 AIN v14 MSG1 and FNA FNL, including established runtime profiles."""
import base64
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import zlib

from binary_common import files, i32, main, resolve, save, sections, success, translations, u16

RUNTIME_LIMITS = [
    0x1865f,0x189b0,0x18b8e,0x18bfb,0x18c5a,0x2707f,0x27476,0x310fe,
    0x3f0d9,0x3f17c,0x3f4ec,0x3f642,0x3f6c7,0x3f812,0x3f897,0x3f9d4,
    0x3fe49,0x3fed3,0x3ff63,0x447d3,0x81ff2,0x82287,0x83a8c,0x842fa,
    0x846f4,0x847a4,0x8492a,0x84e33,0x84f54,0x84fe3,0x85c61,0xb166f,
    0xb17df,0xfa2a0,0x10e748,0x127061,0x1544f9,0x159099,0x159698,0x15973a,
    0x1599ac,0x1599e2,0x159dc4,0x159ec2,0x15a245,0x15a5e5,0x15af41,0x15b0a9,
    0x16f4f4,0x16f95c,0x16fa07,0x16fd1c,0x16fd52,0x1702c2,0x1703b1,0x170519,
    0x1ba646,0x1bd67f,0x2542fb,0x265327,0x2737b4,0x273ea5,0x2744d4,0x279bc6,
    0x279be8,0x279c31,0x279c59,0x281d60,0x281ff0,0x282110,0x2821c6,0x282280,
    0x28250e,0x2826ee,0x282891,0x282a57,0x282adf,0x283bcf,0x283fad,0x2845da,
    0x28484a,0x284f8e,0x292a20,0x295155,0x296ad0,0x296b63,0x297115,0x2978a6,
    0x29791d,0x2d42ac,0x2d601a,0x2d6a2d,0x2d6a91,0x2d6b59,0x324c91,0x328d62]
WARN_11 = ['AIN v14 MSG1 only; STR0, EX tables and image text are not extracted.',
    'New BMP characters use CP932 private-use slots and added FNL bitmap glyphs (maximum 1880 shared slots). No executable changes. Original runtime rendering is not tested; mixed font style may differ.']
WARN_12 = ['当前支持剧情消息；部分人名、选项、菜单及图片文字可能仍为日文。',
    '安装后请使用生成的 .zh.exe 启动。新增缺字采用宋体；请先试译确认游戏中的显示和排版。']


def runtime_section(data, full=False):
    pe, items = sections(data)
    text = [s for s in items if s['name'] == '.text']
    if (u16(data, pe+4) != 0x14c or struct.unpack_from('<I', data, pe+24+28)[0] != 0x400000
            or len(text) != 1 or text[0]['rva'] != 0x1000 or text[0]['virtualSize'] != 3893792
            or text[0]['size'] != 3894272 or not text[0]['flags'] & 0x20000000):
        raise ValueError('此 System4 运行程序布局尚无已验证的中文修补规则。')
    section = text[0]
    length = section['size'] if full else 32768
    expected = ('002309409cea45ffa79458c7f868a59465f734ceb6bd7fe0cc7a1304ca34d216' if full
                else 'e50ebe9f6f9ee2f649177e5bbfc43286039b99b91736eabdc63639824a97ddce')
    start = section['file']
    if start + length > len(data) or hashlib.sha256(data[start:start+length]).hexdigest() != expected:
        raise ValueError('此 System4 运行程序版本尚无已验证的中文修补规则。')
    return start


def runtime_build(data):
    offset = runtime_section(data, True)
    result = bytearray(data)
    for position in RUNTIME_LIMITS:
        position += offset - 0x400
        if result[position] != 0x0f:
            raise ValueError('运行程序的码位检查指令不匹配。')
        result[position] = 0x19
    return bytes(result)


class Ain:
    def __init__(self, original, mapping):
        self.original, self.mapping = original, mapping
        if len(original) < 18 or original[:8] != b'AI2\0\0\0\0\0' or i32(original, 12) != len(original)-16:
            raise ValueError('Unsupported AI2 header')
        self.data = data = zlib.decompress(original[16:])
        if len(data) != i32(original, 8) or data[:4] != b'VERS' or i32(data, 4) != 14:
            raise ValueError('Only AIN version 14 supported')
        code_end = 16 + i32(data, 12)
        if not 16 <= code_end <= len(data):
            raise ValueError('CODE bounds')
        reverse = {slot: char for char, slot in mapping.items()}
        tables, position = [], code_end
        while True:
            position = data.find(b'MSG1', position, len(data)-12)
            if position < 0:
                break
            try:
                count, cursor = i32(data, position+4), position+12
                if not 1 <= count <= (len(data)-cursor)//4:
                    raise ValueError('MSG1 count')
                raw, texts = [], []
                for _ in range(count):
                    length = i32(data, cursor)
                    cursor += 4
                    if not 0 <= length <= len(data)-cursor:
                        raise ValueError('MSG1 bounds')
                    value = data[cursor:cursor+length]
                    decoded = bytes((b-index-0x60)&255 for index, b in enumerate(value))
                    if 0 in decoded:
                        raise ValueError('MSG1 NUL')
                    text = decoded.decode('cp932')
                    texts.append(''.join(reverse.get(c, c) for c in text))
                    raw.append(value)
                    cursor += length
                if cursor+8 <= len(data) and data[cursor:cursor+4] == b'MAIN':
                    tables.append((position, cursor, raw, texts))
            except (ValueError, UnicodeError, struct.error):
                pass
            position += 1
        if len(tables) != 1:
            raise ValueError('No unique valid MSG1/MAIN table')
        self.start, self.end, self.raw, self.texts = tables[0]

    def build(self, changes):
        if not changes:
            return self.original
        out = bytearray(self.data[:self.start+12])
        for index, text in enumerate(self.texts):
            value = self.raw[index]
            if index in changes and changes[index] != text:
                encoded = ''.join(self.mapping.get(c, c) for c in changes[index]).encode('cp932')
                value = bytes((b+j+0x60)&255 for j, b in enumerate(encoded))
            out.extend(struct.pack('<i', len(value)))
            out.extend(value)
        out.extend(self.data[self.end:])
        packed = zlib.compress(out, 9)
        return self.original[:8] + struct.pack('<ii', len(out), len(packed)) + packed


class Fnl:
    @staticmethod
    def index(slot):
        return 8994 + ord(slot) - 0xe000

    def __init__(self, original):
        self.original, self.mapping, self.fonts = original, {}, []
        if len(original) < 20 or original[:4] != b'FNA\0' or i32(original, 8) != len(original):
            raise ValueError('FNL header/size mismatch')
        self.index_end = 16 + i32(original, 12)
        if not 20 <= self.index_end <= len(original):
            raise ValueError('FNL index bounds')
        count, cursor = i32(original, 16), 20
        if not 1 <= count <= 100:
            raise ValueError('FNL font count')
        for _ in range(count):
            faces, face_count = [], i32(original, cursor)
            cursor += 4
            if not 1 <= face_count <= 200:
                raise ValueError('FNL face count')
            for _ in range(face_count):
                height, uk, glyph_count = struct.unpack_from('<iii', original, cursor)
                cursor += 12
                if not 1 <= height <= 4096 or not 3424 <= glyph_count <= 11438:
                    raise ValueError('FNL face layout')
                glyphs = []
                for _ in range(glyph_count):
                    width, position, size = struct.unpack_from('<Hii', original, cursor)
                    cursor += 10
                    if width > 4096 or size < 0 or not self.index_end <= position <= len(original)-size:
                        raise ValueError('FNL glyph bounds')
                    glyphs.append((width, position, size))
                faces.append((height, uk, glyphs))
            self.fonts.append(faces)
        if cursor != self.index_end:
            raise ValueError('FNL index size')
        if original[-8:] == b'A4CNMAP1':
            length = i32(original, len(original)-12)
            start = len(original)-12-length
            if length < 4 or start < self.index_end:
                raise ValueError('FNL mapping footer')
            count = i32(original, start)
            if not 0 <= count <= 1880 or length != 4+count*4:
                raise ValueError('FNL mapping count')
            for offset in range(start+4, start+length, 4):
                char, slot = (chr(c) for c in struct.unpack_from('<HH', original, offset))
                if (0xd800 <= ord(char) <= 0xdfff or not '\ue000' <= slot <= '\ue757'
                        or char in self.mapping or slot in self.mapping.values()
                        or any(self.index(slot) >= len(face[2]) for face in self.faces())):
                    raise ValueError('FNL mapping invalid')
                self.mapping[char] = slot

    def faces(self):
        return [face for font in self.fonts for face in font]

    def bitmap(self, face, index):
        width, position, size = face[2][index]
        value = zlib.decompress(self.original[position:position+size])
        length = (width+31)//32*4*face[0]
        if len(value) > length:
            raise ValueError('FNL oversized glyph')
        return value.ljust(length, b'\0')

    def build(self, desired, request, legacy_font):
        added = [(c, slot) for c, slot in desired.items() if c not in self.mapping]
        if not added:
            return self.original
        maximum = max(self.index(slot) for slot in desired.values())+1
        new_size = 4 + sum(4+sum(12+max(maximum, len(face[2]))*10 for face in font) for font in self.fonts)
        delta = 16 + new_size - self.index_end
        body, changed_faces = bytearray(self.original[self.index_end:]), []
        jobs = [dict(character=c, width=face[2][3423][0], height=face[0],
                     reference=base64.b64encode(self.bitmap(face, 3423)).decode())
                for face in self.faces() for c, _ in added]
        if legacy_font:
            from system4_glyph import render_noto
            bitmaps = iter(render_noto(jobs, Path(__file__).parent/'tools'/'NotoSansSC-Regular.otf'))
        else:
            import os
            command = (request.get('toolPaths') or {}).get('hostCommand') or json.loads(os.environ.get('GAMEL10N_ADAPTER_HOST', 'null'))
            if not command:
                raise ValueError('缺少固定字形组件，请使用完整发行包。')
            process = subprocess.run([*command, 'component'], input=json.dumps(dict(protocolVersion=1,
                operation='render_glyphs', glyphs=jobs), ensure_ascii=False), encoding='utf-8',
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=300,
                creationflags=0x08000000 if os.name == 'nt' else 0)
            response = json.loads(process.stdout)
            if process.returncode or not response.get('success'):
                raise ValueError(response.get('error') or process.stderr[-4000:])
            bitmaps = iter(base64.b64decode(v) for v in response['result']['values'])
        for height, uk, glyphs in self.faces():
            updated = [(w, p+delta, n) for w, p, n in glyphs]
            updated.extend([updated[0]] * max(0, maximum-len(updated)))
            for char, slot in added:
                index = self.index(slot)
                if index < len(glyphs) and glyphs[index][2] > 0:
                    raise ValueError('Refusing to overwrite an existing private glyph')
                bitmap = next(bitmaps)
                if not any(bitmap):
                    raise ValueError('Generated glyph is empty')
                value = zlib.compress(bitmap, 9)
                updated[index] = (glyphs[3423][0], 16+new_size+len(body), len(value))
                body.extend(value)
            changed_faces.append((height, uk, updated))
        out = bytearray(self.original[:16])
        out.extend(struct.pack('<i', len(self.fonts)))
        iterator = iter(changed_faces)
        for font in self.fonts:
            out.extend(struct.pack('<i', len(font)))
            for _ in font:
                height, uk, glyphs = next(iterator)
                out.extend(struct.pack('<iii', height, uk, len(glyphs)))
                for glyph in glyphs:
                    out.extend(struct.pack('<Hii', *glyph))
        if len(out) != 16+new_size:
            raise ValueError('FNL rebuild index')
        out.extend(body)
        out.extend(struct.pack('<i', len(desired)))
        for char, slot in sorted(desired.items(), key=lambda e: e[1]):
            out.extend(struct.pack('<HH', ord(char), ord(slot)))
        out.extend(struct.pack('<i', 4+len(desired)*4) + b'A4CNMAP1')
        struct.pack_into('<ii', out, 8, len(out), new_size)
        check = Fnl(bytes(out))
        if any(not any(check.bitmap(face, self.index(slot))) for face in check.faces() for _, slot in added):
            raise ValueError('Rebuilt FNL glyph empty')
        return bytes(out)


def handle(request, legacy_font=False):
    source, operation = request['sourceDirectory'], request['operation']
    selected, typed = files(request), []
    for path in selected:
        if Path(path).suffix.lower() not in ('.ain', '.fnl'):
            continue
        with resolve(source, path).open('rb') as stream:
            header = stream.read(18)
        if len(header) >= 18 and header[:8] == b'AI2\0\0\0\0\0' and i32(header, 8)>16 and header[16] == 0x78:
            typed.append((path, 1))
        elif len(header) >= 16 and header[:4] == b'FNA\0' and i32(header, 12) >= 4:
            typed.append((path, 2))
    executables = [p for p in selected if p.lower().endswith('.exe') and not p.lower().endswith('.zh.exe')]
    runtimes = []
    if not legacy_font:
        for path in executables:
            try:
                with resolve(source, path).open('rb') as stream:
                    runtime_section(stream.read(65536))
                runtimes.append(path)
            except (ValueError, struct.error):
                pass
    runtime = runtimes[0] if len(runtimes) == 1 else None
    warnings = WARN_11 if legacy_font else WARN_12
    if operation == 'probe':
        probe = None
        if {t for _, t in typed} == {1, 2} and (legacy_font or runtime):
            probe = dict(engineId='system4-ain', adapterId='system4-ain14-msg1-fnl',
                adapterVersion='1.1.0' if legacy_font else '1.2.0',
                description='System4 AI2 AIN v14 MSG1 + FNA FNL private-glyph extension' if legacy_font else 'System4 AIN v14 剧情消息与 FNL 中文字库',
                requiredFiles=[p for p, _ in typed]+([] if legacy_font else [runtime]), warnings=warnings,
                originalExecutable=runtime)
        return success('probe', probe)
    if not legacy_font:
        if not runtime:
            raise ValueError('没有匹配的 System4 运行程序修补规则，请继续智能适配分析当前版本。')
        original_exe = resolve(source, runtime).read_bytes()
        runtime_section(original_exe, True)
    fonts = {p: Fnl(resolve(source, p).read_bytes()) for p, t in typed if t == 2}
    if not fonts:
        raise ValueError('FNA FNL font is required')
    old = next(iter(fonts.values())).mapping
    if any(f.mapping != old for f in fonts.values()):
        raise ValueError('FNL maps disagree')
    encoding_map = dict(old)
    scripts = {p: Ain(resolve(source, p).read_bytes(), encoding_map) for p, t in typed if t == 1}
    if not scripts:
        raise ValueError('No supported AIN')
    edits, units, needed, reserved = {}, [], set(), set(old.values())
    mapping = translations(request) if operation == 'build' else {}
    for script in scripts.values():
        reserved.update(c for text in script.texts for c in text if '\ue000' <= c <= '\ue757')
        position = script.data.find(b'STR0', script.end, len(script.data)-8)
        if position >= 0:
            count, cursor = i32(script.data, position+4), position+8
            if 0 <= count <= len(script.data)-cursor:
                for _ in range(count):
                    end = script.data.find(b'\0', cursor)
                    if end < 0:
                        break
                    try:
                        reserved.update(c for c in script.data[cursor:end].decode('cp932') if '\ue000' <= c <= '\ue757')
                    except UnicodeError:
                        pass
                    cursor = end+1
    for path, script in scripts.items():
        edits[path] = {}
        for index, text in enumerate(script.texts):
            key = path.replace('\\', '/')+'#msg1:'+str(index)
            if text.strip():
                units.append(dict(id=key, resourcePath=path, locator=f'MSG1[{index}]', sourceText=text,
                                  context='AIN v14 message table', speaker=None, kind='dialogue'))
            if key not in mapping or mapping[key] == text:
                continue
            value = mapping[key]
            edits[path][index] = value
            for char in value:
                code = ord(char)
                if code > 0xffff or 0xd800 <= code <= 0xdfff or code == 0 or (code < 32 or 127 <= code < 160) and char not in '\r\n\t':
                    raise ValueError('Only BMP text without NUL/invalid controls is supported')
                if '\ue000' <= char <= '\ue757':
                    reserved.add(char)
                try:
                    encodable = char.encode('cp932').decode('cp932') == char
                except UnicodeError:
                    encodable = False
                if char not in old and not encodable:
                    needed.add(char)
    if operation == 'extract':
        return success('extraction', dict(units=units, warnings=warnings))
    if operation == 'build':
        desired = dict(old)
        for char in sorted(needed):
            for code in range(0xe000, 0xe758):
                slot = chr(code)
                if slot in reserved or any(Fnl.index(slot)<len(face[2]) and face[2][Fnl.index(slot)][2]>0
                    for font in fonts.values() for face in font.faces()):
                    continue
                break
            else:
                raise ValueError('No free FNL/CP932 private-use slot remains')
            desired[char] = encoding_map[char] = slot
            reserved.add(slot)
        output, outputs = request['outputDirectory'], []
        for path, script in scripts.items():
            save(output, path, script.build(edits[path]))
            outputs.append(path)
        for path, font in fonts.items():
            save(output, path, font.build(desired, request, legacy_font) if needed else font.original)
            outputs.append(path)
        launch = runtime
        if not legacy_font:
            for path in executables:
                save(output, path, resolve(source, path).read_bytes())
                outputs.append(path)
            if desired:
                launch = str(Path(runtime).with_suffix('.zh.exe')).replace('\\', '/')
                save(output, launch, runtime_build(original_exe))
                outputs.append(launch)
        return success('build', dict(files=outputs, warnings=warnings, launchExecutable=launch))
    if operation == 'validate':
        if not legacy_font:
            output = request['outputDirectory']
            built = [Fnl(resolve(output, p).read_bytes()) for p in fonts]
            actual_map = built[0].mapping
            if any(f.mapping != actual_map for f in built):
                raise ValueError('补丁字库的中文映射不一致。')
            if any(not any(font.bitmap(face, Fnl.index(slot))) for font in built for face in font.faces() for slot in actual_map.values()):
                raise ValueError('补丁字库存在空的中文字形。')
            if actual_map and resolve(output, str(Path(runtime).with_suffix('.zh.exe'))).read_bytes() != runtime_build(original_exe):
                raise ValueError('中文启动程序与资源使用的码位不匹配。')
            for path in scripts:
                Ain(resolve(output, path).read_bytes(), actual_map)
        return success('validation', dict(passed=True, issues=[], runtimeStatus='not-tested'))
    raise ValueError('Unsupported operation: '+operation)


if __name__ == '__main__':
    main(handle)
