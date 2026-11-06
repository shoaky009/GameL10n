"""MESSAGE v4 shared slots plus hash-gated Unicode Graphics and PE UI."""
import base64
import hashlib
import json
from pathlib import Path
import struct
import sys
import traceback
from gamel10n_host import component
from kaguya_message import MAGIC, Message
import kaguya_runtime
from pe import Pe
from pe_text import PeText
from pe_native import PeNative

EXE_HASH = 'E688672BB224622195A4EC239C9111332AD479A920237965643903F26F191582'
EXE_HEAD = '643EA6E0FB35D7C23FFDF2CAC1BE5256E567928E9382AAADD41E090E2A9DE3BA'
DLL_HEAD = '36014AB0ECF298F9BB780FA5DE679C130F87179BB3A05115279397A319D6DED4'
TOKENS = ('{playerName1}', '{playerName2}')
WARNINGS = ['支持已校验的 MESSAGE v4.0 / Graphics Unicode 模板；未运行原游戏，启动、换行、存档及实际选字仍需实机验证。',
            '已覆盖消息表中实际引用的对白、姓名、选项，原生菜单和配置/启动/存档对话框，以及21条已证实的原生提示/标题；图片/影片文字不在范围。',
            '尚未覆盖动态设置标签和params角色音量名；这些内容不会被错误地当成已完成。',
            '原生ANSI启动环境和现有转区配置保持不变。游戏输入的新玩家姓名、动态ANSI标签仍有原程序编码限制；中文正文及已覆盖的确认提示改用Unicode绘制。',
            '仅分配原生CP932用户造字区F042..F9FC，保留F040/F041姓名替换控制。码位不足会明确拒绝，不会替换为问号。']


def at(root, path):
    normalized = path.replace('\\', '/')
    if Path(normalized).is_absolute() or any(part in ('', '..') for part in normalized.split('/')):
        raise ValueError('资源路径不安全。')
    result = Path(root).resolve() / normalized
    if not result.resolve().is_relative_to(Path(root).resolve()):
        raise ValueError('资源路径超出目录。')
    return result


def unique(paths):
    return list({path.replace('\\', '/').casefold(): path.replace('\\', '/') for path in paths}.values())


def digest(data):
    return hashlib.sha256(data).hexdigest().upper()


def has_section(data, name):
    try:
        return Pe(data).section(name) is not None
    except (ValueError, IndexError, struct.error):
        return False


def select(request, probe):
    root = Path(request['sourceDirectory'])
    paths = [file['path'] for file in request.get('probeFiles', [])] if probe and request.get('probeFiles') else request.get('resourceFiles')
    paths = unique(paths or [file.relative_to(root).as_posix() for file in root.rglob('*') if file.is_file()])
    message = exe = dll = None
    for path in paths:
        with at(root, path).open('rb') as stream:
            header = stream.read(65536)
        if len(header) >= 25 and header.startswith(MAGIC):
            message = path
        if len(header) >= 65536:
            hash_value = digest(header)
            if hash_value == EXE_HEAD:
                exe = path
            if hash_value == DLL_HEAD or has_section(header, '.kguw'):
                dll = path
    if not all((message, exe, dll)):
        return None
    patched = exe[:-4] + '.l10n.exe'
    ui = next((path for path in paths if path.casefold() == patched.casefold() and any(has_section(at(root, path).read_bytes()[:65536], name) for name in ('.l10nui', '.kgui'))), exe)
    files = unique([message, exe, dll] + ([ui] if ui != exe else []) + [path for path in paths if path.casefold() in {(exe + '.le.config').casefold(), (ui + '.le.config').casefold()}])
    if not probe and digest(at(root, exe).read_bytes()) != EXE_HASH:
        raise ValueError('主程序不是已验证的模板版本。')
    return message, exe, dll, ui, files


class UnicodeCodec:
    def __init__(self, request, mapping):
        self.request, self.mapping = request, mapping

    def decode_many(self, values):
        if not values:
            return []
        texts = component('glyph_decode', self.request, map=self.mapping,
                          values=[base64.b64encode(value).decode('ascii') for value in values])['texts']
        return [text.replace('\ue000', TOKENS[0]).replace('\ue001', TOKENS[1]) for text in texts]

    def encode_many(self, texts):
        if not texts:
            return []
        texts = [text.replace(TOKENS[0], '\ue000').replace(TOKENS[1], '\ue001') for text in texts]
        return [base64.b64decode(value) for value in component('glyph_encode', self.request, map=self.mapping, texts=texts)['values']]

    def check(self, original, text):
        if '\0' in text:
            raise ValueError('消息文本不能包含空字符。')
        if any(char in text for char in ('\ufffd', '\ue000', '\ue001')):
            raise ValueError('消息文本含保留字形或无效Unicode字符。')
        if any(original.count(token) != text.count(token) for token in TOKENS):
            raise ValueError('必须保留玩家姓名替换标记。')


def units(message, path, exe, ui, native):
    result = []
    for slot in message.slots:
        if slot.references:
            result.append(dict(id=path + '|' + slot.key, resourcePath=path, locator=slot.key, sourceText=slot.text,
                               context=f'{slot.kind} 槽 {slot.index}；引用 {slot.references} 次' + ('；此槽的修改将影响全部引用' if slot.references > 1 else ''),
                               speaker='name:' + str(next(iter(slot.names))) if slot.kind == 'dialogue' and len(slot.names) == 1 and next(iter(slot.names)) >= 0 else None,
                               kind=slot.kind, protectedTokens=[token for token in TOKENS if token in slot.text]))
    for owner, kind in ((ui, 'ui'), (native, 'system')):
        for field in owner.fields:
            result.append(dict(id=exe + '|' + field.key, resourcePath=exe, locator=field.key, sourceText=field.text,
                               context=field.context, speaker=None, kind=kind, protectedTokens=[]))
    return result


def execute(request):
    if request.get('protocolVersion') != 1:
        raise ValueError('适配器协议版本不匹配。')
    operation = request['operation']
    selected = select(request, operation == 'probe')
    if operation == 'probe':
        return dict(probe=None if selected is None else dict(engineId='kaguya', adapterId='template.kaguya-message-v4-unicode',
                    adapterVersion='1.1.0', description='Kaguya MESSAGE v4.0 / Unicode绘制与PE界面',
                    requiredFiles=selected[4], warnings=WARNINGS, originalExecutable=selected[1]))
    if selected is None:
        raise ValueError('缺少已验证的消息表、主程序或Graphics库。')
    path, exe, dll, ui_path, files = selected
    source = Path(request['sourceDirectory'])
    mapping = kaguya_runtime.read_map(at(source, dll).read_bytes())
    message = Message(at(source, path).read_bytes(), UnicodeCodec(request, mapping))
    source_exe = at(source, ui_path).read_bytes()
    ui, native = PeText(source_exe), PeNative(source_exe)
    extracted = units(message, path, exe, ui, native)
    if operation == 'extract':
        return dict(extraction=dict(units=extracted, warnings=WARNINGS))
    if operation == 'validate':
        return dict(validation=dict(passed=True, issues=[], runtimeStatus='not-tested'))
    if operation != 'build':
        raise ValueError('未知操作。')
    output = Path(request['outputDirectory']).resolve()
    source = source.resolve()
    if source.is_relative_to(output) or output.is_relative_to(source):
        raise ValueError('输出目录必须独立于原资源目录。')
    translations = request.get('translations') or {}
    if request.get('translationsFile'):
        translations = json.loads(Path(request['translationsFile']).read_text('utf-8-sig'))
    known = {unit['id']: unit for unit in extracted}
    edits, ui_edits, native_edits = {}, {}, {}
    for identifier, text in translations.items():
        if identifier not in known:
            raise ValueError('译文包含未知或未引用的文本单元。')
        if not isinstance(text, str):
            raise ValueError('译文不能为空值。')
        unit = known[identifier]
        if unit['resourcePath'] == path:
            message.codec.check(unit['sourceText'], text)
            edits[unit['locator']] = text
        elif unit['kind'] == 'ui':
            ui_edits[unit['locator']] = text
        else:
            native_edits[unit['locator']] = text
    changed = any(edits.get(slot.key, slot.text) != slot.text for slot in message.slots)
    mapping = component('glyph_plan', request, texts=[edits.get(slot.key, slot.text) for slot in message.slots],
                        allowedSlots=list(range(0xe002, 0xe758)), preservedTexts=[slot.text for slot in message.slots] + [field.text for field in ui.fields], map=mapping)['map']
    mapping = {int(key): value for key, value in mapping.items()}
    message.codec = UnicodeCodec(request, mapping)
    built = message.build(edits)
    checked = Message(built, message.codec)
    if [slot.text for slot in checked.slots] != [edits.get(slot.key, slot.text) for slot in message.slots]:
        raise ValueError('回填后文本不一致。')
    files = unique(files + request.get('resourceFiles', []))
    payload = {file: at(source, file).read_bytes() for file in files}
    payload[path] = built
    if changed:
        payload[dll] = kaguya_runtime.apply(payload[dll], mapping)
    built_exe = PeNative(ui.build(ui_edits)).build(native_edits)
    launch = ui_path
    if built_exe != source_exe:
        launch = exe[:-4] + '.l10n.exe'
        payload[launch] = built_exe
        if exe + '.le.config' in payload:
            payload[launch + '.le.config'] = payload[exe + '.le.config']
    for file, data in payload.items():
        target = at(output, file)
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
    return dict(build=dict(files=list(payload), warnings=WARNINGS, launchExecutable=launch))


def main():
    request = {}
    try:
        request = json.load(sys.stdin)
        response = dict(protocolVersion=1, success=True, **execute(request))
    except Exception as error:
        traceback.print_exc(file=sys.stderr)
        response = dict(protocolVersion=1, success=False, error=str(error))
    text = json.dumps(response, ensure_ascii=False)
    if request.get('responseFile'):
        Path(request['responseFile']).write_text(text, 'utf-8')
    else:
        print(text)


if __name__ == '__main__':
    main()
