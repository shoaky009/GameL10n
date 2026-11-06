"""Hash-gated MESSAGE v4 GBK runtime; shared slots retain their legacy IDs."""
import hashlib
import json
from pathlib import Path
import sys
import traceback

from kaguya_message import LosslessGbk, MAGIC, Message

EXE_HASH = '56612E4E3338EC756564129B00042A91EB3B46D11A162D938826E02279EF2413'
DLL_HASH = 'F18055FD45B70D04DD2E141F80BD93BBD18382B638FE36B4A7F6711E1277A23C'
EXE_HEAD = '48E6610C2DD03F916C02C5375AD40E9D6CA4C4255887990ABC15226B967D74A9'
DLL_HEAD = '4526DFC0E71CD957E5AC90A2FE74D374CAE647867B0A157147001305CCBC2D60'
THRESHOLD = 0x4c255
WARNINGS = ['仅支持已校验的 Kaguya MESSAGE v4.0 GBK 程序组合；不适用于其他版本的 EXE 或图形库。',
            '中文构建保留程序原有 SimHei/GB2312_CHARSET 字体路径及字号；运行系统须提供黑体并保持原有中文 ANSI 启动环境。',
            '资源回填不等同于游戏运行验证；启动、字形、换行与存档仍须实机确认。姓名按共享槽统一修改。']


def at(root, path):
    normalized = path.replace('\\', '/')
    if Path(normalized).is_absolute() or any(part in ('', '..') for part in normalized.split('/')):
        raise ValueError('资源路径不安全。')
    result = Path(root).resolve() / normalized
    if not result.resolve().is_relative_to(Path(root).resolve()):
        raise ValueError('资源路径超出目录。')
    return result


def digest(data):
    return hashlib.sha256(data).hexdigest().upper()


def select(request, probe):
    root = Path(request['sourceDirectory'])
    paths = [file['path'] for file in request.get('probeFiles', [])] if probe and request.get('probeFiles') else request.get('resourceFiles')
    paths = paths or [file.relative_to(root).as_posix() for file in root.rglob('*') if file.is_file()]
    paths = sorted({path.replace('\\', '/').casefold(): path.replace('\\', '/') for path in paths}.values())
    messages, exes, dlls = [], [], []
    for path in paths:
        extension = Path(path).suffix.lower()
        if extension not in ('.dat', '.exe', '.dll'):
            continue
        with at(root, path).open('rb') as stream:
            header = stream.read(65536)
        if extension == '.dat' and len(header) >= 25 and header.startswith(MAGIC):
            messages.append(path)
        if extension == '.exe' and digest(header) == EXE_HEAD:
            exes.append(path)
        if extension == '.dll' and digest(header) == DLL_HEAD:
            dlls.append(path)
    if not messages or not exes or not dlls:
        return None
    if len(messages) != 1 or len(exes) != 1 or len(dlls) != 1:
        raise ValueError('发现多套兼容资源，请分别选择游戏目录。')
    message, exe, dll = messages[0], exes[0], dlls[0]
    files = [message, exe, dll] + [path for path in paths if path.casefold() == (exe + '.le.config').casefold()]
    if not probe:
        if digest(at(root, exe).read_bytes()) != EXE_HASH:
            raise ValueError('主程序完整校验失败；请使用已验证的 GBK 版本。')
        data = bytearray(at(root, dll).read_bytes())
        if len(data) <= THRESHOLD or data[THRESHOLD] not in (0xa1, 0x81):
            raise ValueError('图形库的 GBK 补丁位置不匹配。')
        data[THRESHOLD] = 0xa1
        if digest(data) != DLL_HASH:
            raise ValueError('图形库完整校验失败；不能应用此模板。')
    return message, exe, dll, files


def extract(message, path):
    units = []
    for slot in message.slots:
        if not slot.references:
            continue
        units.append(dict(id=path + '|' + slot.key, resourcePath=path, locator=slot.key, sourceText=slot.text,
                          context=f'{slot.kind} 槽 {slot.index}；引用 {slot.references} 次' +
                          ('；此槽的修改将影响全部引用' if slot.references > 1 else ''),
                          speaker='name:' + str(next(iter(slot.names))) if slot.kind == 'dialogue' and len(slot.names) == 1 and next(iter(slot.names)) >= 0 else None,
                          kind=slot.kind))
    return units


def execute(request):
    if request.get('protocolVersion') != 1:
        raise ValueError('适配器协议版本不匹配。')
    operation = request['operation']
    selected = select(request, operation == 'probe')
    if operation == 'probe':
        return dict(probe=None if selected is None else dict(engineId='kaguya', adapterId='kaguya-message-v4-gbk',
                    adapterVersion='1.0.0', description='Kaguya MESSAGE v4.0 GBK 无损回填模板（校验程序组合）',
                    requiredFiles=selected[3], warnings=WARNINGS, originalExecutable=selected[1]))
    if selected is None:
        raise ValueError('未找到匹配的消息表、主程序和图形库组合。')
    path, exe, dll, files = selected
    source = Path(request['sourceDirectory'])
    message = Message(at(source, path).read_bytes(), LosslessGbk())
    if operation == 'extract':
        return dict(extraction=dict(units=extract(message, path), warnings=WARNINGS))
    if operation == 'validate':
        if message.build({}) != message.data:
            raise ValueError('消息表原字节回填验证失败。')
        return dict(validation=dict(passed=True, issues=[], runtimeStatus='not-tested'))
    if operation != 'build':
        raise ValueError('未知的适配器操作。')
    output = Path(request['outputDirectory']).resolve()
    source = source.resolve()
    if source.is_relative_to(output) or output.is_relative_to(source):
        raise ValueError('输出目录必须独立于输入目录，禁止原地修改。')
    translations = request.get('translations') or {}
    if request.get('translationsFile'):
        translations = json.loads(Path(request['translationsFile']).read_text('utf-8-sig'))
    by_id = {path + '|' + slot.key: slot for slot in message.slots if slot.references}
    edits = {}
    for identifier, text in translations.items():
        if identifier not in by_id:
            raise ValueError('译文包含未知或未引用的文本槽。')
        if not isinstance(text, str):
            raise ValueError('译文不能是空值。')
        edits[by_id[identifier].key] = text
    built = message.build(edits)
    checked = Message(built, message.codec)
    if [(slot.key, slot.text) for slot in checked.slots] != [(slot.key, edits.get(slot.key, slot.text)) for slot in message.slots]:
        raise ValueError('重建后文本槽验证失败；可能存在编码边界冲突。')
    changed = any(edits.get(slot.key, slot.text) != slot.text for slot in message.slots)
    files = list({file.casefold(): file for file in files + request.get('resourceFiles', [])}.values())
    for file in files:
        data = built if file == path else at(source, file).read_bytes()
        if changed and file == dll:
            data = bytearray(data)
            data[THRESHOLD] = 0x81
        target = at(output, file)
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
    return dict(build=dict(files=files, warnings=WARNINGS, launchExecutable=exe))


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
