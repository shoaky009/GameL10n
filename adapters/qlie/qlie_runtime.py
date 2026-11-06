"""Font/configuration integration for the historical QLIE package variants."""
import hashlib
import html
import json
from pathlib import Path
import re

from qlie_discovery import full, norm, MAP, LAUNCH
from qlie_font import create
from qlie_script import Script, decode, same_encoding


def map_json(codec):
    return json.dumps(dict(sorted(codec.aliases.items())), separators=(',', ':'))


def launch_xml(executable, fonts):
    def attr(value):
        return html.escape(value, quote=False).replace('"', '&quot;').replace('\r', '&#xD;').replace('\n', '&#xA;')
    lines = ['<launch executable="'+attr(executable)+'">']
    for file, face in fonts:
        lines.append('  <font file="'+attr(file)+'"'+(' face="'+attr(face)+'"' if face else '')+' />')
    return ('\r\n'.join(lines+['</launch>'])).encode('utf-8')


def patch_fields(original, regex, replacement):
    text = original.decode('cp932')
    parts, cursor, byte_cursor = [], 0, 0
    for match in regex.finditer(text):
        value = replacement(match)
        if value is None or value == match['value']:
            continue
        begin = byte_cursor+len(text[cursor:match.start('value')].encode('cp932'))
        end = begin+len(match['value'].encode('cp932'))
        parts.extend((original[byte_cursor:begin], value.encode('cp932')))
        cursor, byte_cursor = match.end('value'), end
    return b''.join(parts)+original[byte_cursor:]


def apply(discovery, output, variant, entries, items, codec, font_item, layout, slots, current, replace, put, outputs, tools):
    def tool(name):
        path = tools/name
        if not path.is_file():
            raise ValueError('适配包缺少运行依赖：' + name)
        return path
    def font(source, path, family, aliases, coverage=None):
        # This historical selector requested Regular in config but omitted it
        # from name ID 4. Match its requested face for the native launcher.
        create(tool(source), full(output, path), family, aliases, variant['fontRevision'], coverage,
               regular_full_name=variant['version'] == '2915811a559386ed')
        outputs.add(path)
    def licenses(directory, extra=()):
        for name in ('NotoSans-LICENSE.txt', 'NotoSerif-LICENSE.txt', 'FONT-DERIVATION.txt', *extra):
            put(directory+name, tool(name).read_bytes())
    cfg_path = discovery.config
    cfg = full(discovery.root, cfg_path).read_bytes()
    if variant['unicode']:
        sans, serif = 'Qlie Han Sans U31', 'Qlie Han Serif U31'
        def map_face(face):
            f = face.strip()
            if not f or f.lower() == 'base' or f in (sans+' Regular', serif+' Regular'):
                return face
            if '明朝' in f or f.lower() in ('ms mincho', 'ms pmincho'):
                return serif+' Regular'
            if 'ゴシック' in f or f.lower() in ('ms gothic', 'ms pgothic', 'ms ui gothic'):
                return sans+' Regular'
            raise ValueError('未验证的 QLIE 系统字体：'+f)
        pattern = re.compile(r'^(?P<prefix>\s*font\[\d+\]\.name=)(?P<value>[^\r\n]*)(?P<cr>\r?)$', re.M | re.I)
        def map_names(text):
            return pattern.sub(lambda m: m['prefix']+map_face(m['value'])+m['cr'], text)
        required = ['system/font/fontdata.txt', 'avg/imoscripter_fontlist.txt', 'avg/setting.txt']
        chosen = []
        for name in required:
            matches = [it for it in entries if norm(it.entry.name).lower() == name]
            if len(matches) != 1:
                raise ValueError('需要唯一的系统字体定义、ImoScripter 字体表和设置表。')
            chosen.append(matches[0])
        for it in chosen[1:]:
            before = current(it)
            text = decode(before)
            mapped = map_names(text)
            if text != mapped:
                replace(it, same_encoding(before, mapped))
        it = chosen[0]
        before = current(it)
        text = decode(before)
        count = 0
        def map_list(m):
            nonlocal count
            count += 1
            if m['value'] not in ('base', 'system'):
                raise ValueError('不支持的 QLIE 字体列表：'+m['value'])
            return m['prefix']+'system'+m['cr']
        mapped = re.sub(r'^(?P<prefix>Font\[\d+\]\.List\[0\]=)(?P<value>[^\r\n]*)(?P<cr>\r?)$', map_list, text, flags=re.M | re.I)
        if count < 8:
            raise ValueError('QLIE 系统字体列表数量不足。')
        if text != mapped:
            replace(it, same_encoding(before, mapped))
        cfg_text = decode(cfg)
        mapped = map_names(cfg_text)
        if mapped != cfg_text:
            put(cfg_path, same_encoding(cfg, mapped))
        coverage = [s.text for it in items for s in Script(current(it), unicode=True).segments]
        font('NotoSansSC-VF.ttf', 'qlie-l10n/sans.ttf', sans, {}, coverage)
        font('NotoSerifSC-VF.ttf', 'qlie-l10n/serif.ttf', serif, {}, coverage)
        licenses('qlie-l10n/')
        put('qlie-l10n/launch.xml', launch_xml(discovery.executable, [('qlie-l10n/sans.ttf', sans+' Regular'), ('qlie-l10n/serif.ttf', serif+' Regular')]))
    elif discovery.modern:
        if set(slots) != set(range(8)):
            raise ValueError('此适配器要求 0..7 号系统字体布局。')
        if any(m[1] for m in re.finditer(r'(?m)^Font\[\d+\]\.List\[[1-9]\]=([^\r\n]*)', layout)):
            raise ValueError('此布局还有额外字体列表，不能安全替换。')
        if any(v not in ('system', 'system_fix') and not (k == 6 and v == '外字') for k, v in slots.items()):
            raise ValueError('此布局使用未支持的位图字体。')
        mapping = map_json(codec)
        token = hashlib.sha256(('QLIE-SFNT-4|'+mapping).encode()).hexdigest().upper()[:10]
        serif, sans = 'QlieSerif'+token, 'QlieSans'+token
        names, literals = {}, {}
        def map_name(m):
            slot, old = int(m['slot']), m['value']
            if not 0 <= slot <= 7 or slot in names:
                raise ValueError('字体配置槽位越界或重复。')
            if '明朝' in old or old.startswith('QlieSerif'):
                face = serif+' Regular'
            elif 'ゴシック' in old or old.startswith('QlieSans'):
                face = sans+' Regular'
            else:
                raise ValueError('未识别原字体风格：'+old)
            names[slot] = face
            if old in literals and literals[old] != face:
                raise ValueError('字体配置存在不一致映射。')
            literals[old] = face
            return face
        mapped_cfg = patch_fields(cfg, re.compile(r'(?m)^Font\[(?P<slot>\d+)\]\.Name=(?P<value>[^\r\n]+)'), map_name)
        if len(names) != 8:
            raise ValueError('外置配置缺少 0..7 号字体名称。')
        if slots[6] == '外字':
            literals['外字'] = names[6]
        replace(font_item, patch_fields(current(font_item), re.compile(r'(?m)^Font\[(?P<slot>\d+)\]\.List\[0\]=(?P<value>[^\r\n]+)'), lambda m: 'system'))
        put(cfg_path, mapped_cfg)
        for it in items:
            before = current(it)
            after = patch_fields(before, re.compile(r'\\fn2,[^,\r\n]+,"(?P<value>[^"\r\n]+)"'), lambda m: literals.get(m['value']))
            if before != after:
                replace(it, after)
        put(MAP, mapping.encode())
        serif_file, sans_file = 'qlie-l10n/fonts/'+serif+'.ttf', 'qlie-l10n/fonts/'+sans+'.ttf'
        font('NotoSerifSC-VF.ttf', serif_file, serif, codec.aliases)
        font('NotoSansSC-VF.ttf', sans_file, sans, codec.aliases)
        licenses('qlie-l10n/licenses/', ('sources.json',))
        put('qlie-l10n/launch.xml', launch_xml(discovery.executable, [(serif_file, serif+' Regular'), (sans_file, sans+' Regular')]))
    else:
        version = variant['version']
        if version not in ('6cd70ca295b40fb4', '8b3b903685e13fca'):
            replace(font_item, re.sub(r'(?m)^(Font\[[0-46-7]\]\.List\[0\]=)system_fix(?=\r?$)', r'\1system', layout).encode('cp932'))
        mapping = map_json(codec)
        prefix = variant['fontFingerprintPrefix']
        fingerprint = hashlib.sha256((prefix+mapping).encode()).hexdigest().upper()[:8]
        sans, serif = 'QLie Han Sans '+fingerprint, 'QLie Han Serif '+fingerprint
        font('NotoSansSC-VF.ttf', 'qlie-l10n/Sans.ttf', sans, codec.aliases)
        font('NotoSerifSC-VF.ttf', 'qlie-l10n/Serif.ttf', serif, codec.aliases)
        suffix = '' if version in ('6cd70ca295b40fb4', '8b3b903685e13fca') else ' Regular'
        def map_name(m):
            slot, old = int(m[2]), m[3]
            if slot == 5:
                if old != '外字登録':
                    raise ValueError('字体槽5不是预期的独立外字字体。')
                return m[0]
            if old.startswith('QLie Han Sans ') or old == 'ＭＳ ゴシック':
                return m[1]+sans+suffix
            if old.startswith('QLie Han Serif ') or old == 'ＭＳ 明朝':
                return m[1]+serif+suffix
            raise ValueError('遇到尚未支持的字体风格：'+old)
        cfg_text = re.sub(r'(?m)^(Font\[(\d+)\]\.Name=)([^\r\n]*)', map_name, cfg.decode('cp932'))
        put(cfg_path, cfg_text.encode('cp932'))
        put(MAP, mapping.encode())
        faces = (sans+suffix, serif+suffix)
        put('qlie-l10n/launch.xml', launch_xml(discovery.executable, [('qlie-l10n/Sans.ttf', faces[0]), ('qlie-l10n/Serif.ttf', faces[1])]))
        licenses('qlie-l10n/')
    launcher = tool('QlieLauncher.exe').read_bytes()
    if len(launcher) < 128 or launcher[:2] != b'MZ':
        raise ValueError('汉化启动器不是有效的 Windows 程序。')
    put(LAUNCH, launcher)
    instructions = variant['instructions']
    instructions = instructions.replace('转区启动器可能提前退出，因此成功启动后字体保留至注销／重启。',
                                        '启动器临时加载字体，并在所启动的进程结束后卸载。')
    instructions = instructions.replace('字体注册在当前登录会话内保留，以兼容转区启动器提前退出；注销后自动失效。',
                                        '启动器临时加载字体，并在所启动的进程结束后卸载。')
    if variant['version'] == '6cd70ca295b40fb4':
        instructions = '请将补丁安装到完整游戏目录，并从根目录的“启动汉化.exe”启动。保留原 EXE 及原日语转区配置。启动器仅为当前 Windows 会话加载字体；游戏启动和显示尚未验证。\r\n'
    put('qlie-l10n/使用说明.txt', instructions.encode('utf-8'))
