"""JSON adapter entry point for keyed QLIE FilePack 3.0 and Unicode 3.1."""
from dataclasses import dataclass
import json
import os
from pathlib import Path
import re
import shutil
import sys


def engine_library(request):
    library = (request.get('toolPaths') or {}).get('engineLibrary')
    if library:
        sys.path.insert(0, library)
        return
    # Repository execution uses the same pinned library as packaged execution.
    for parent in Path(__file__).resolve().parents:
        current = parent/'third_party/engine-tools/current.json'
        if current.is_file():
            revision = json.loads(current.read_text(encoding='utf-8'))['revision']
            sys.path.insert(0, str(current.parent/'references'/revision/'library.zip'))
            return
    raise ValueError('缺少固定版本的引擎 Python 库。')


@dataclass
class Item:
    pack: str
    archive: object
    entry: object
    script: object = None
    data: bytes | None = None

    def read(self):
        if self.data is None:
            self.data = self.archive.read(self.entry)
        return self.data


def execute(request):
    engine_library(request)
    from qlie_discovery import Discovery, norm, full, unique, MAP, LAUNCH
    from qlie_script import Encoding, Script
    from qlie_runtime import apply
    here = Path(__file__).resolve().parent
    variants = json.loads((here/'variants.json').read_text(encoding='utf-8'))
    selector = here/'variant.json'
    version = json.loads(selector.read_text(encoding='utf-8'))['version'] if selector.exists() else request.get('qlieVariant', '9a44e166fa6aff9d')
    variant = variants[version]
    root, operation = request['sourceDirectory'], request['operation']
    resource_files = request.get('resourceFiles')
    if resource_files is None:
        resource_files = [str(p.relative_to(root)) for p in Path(root).rglob('*') if p.is_file()]
    files = [norm(p) for p in resource_files]
    if variant['unicode'] or version == '9a44e166fa6aff9d':
        files = unique(files)
    if operation == 'probe':
        return {'probe': Discovery(root, files, variant, probe=True).probe()}
    discovery = Discovery(root, files, variant)
    aliases = {}
    if not variant['unicode']:
        map_file = next((p for p in files if p.lower() == MAP), None)
        if map_file:
            aliases = json.loads(full(root, map_file).read_text(encoding='utf-8-sig'))
    codec = Encoding(aliases)
    winners = {}
    for pack, archive in discovery.archives:
        for entry in archive.entries:
            winners[norm(entry.name).lower()] = Item(pack, archive, entry)
    entries = list(winners.values())
    if discovery.modern or variant['unicode']:
        selected = sorted((it for it in entries if it.entry.name.lower().endswith('.s')), key=lambda it: norm(it.entry.name).lower())
    else:
        # Original packages enumerate archive order; this affects cache unitOrder.
        selected = [Item(discovery.archives[0][0], discovery.archives[0][1], e)
                    for e in discovery.archives[0][1].entries if e.name.lower().endswith('.s')]
    items, reserved = [], set()
    for it in selected:
        data = it.read()
        if not variant['unicode']:
            reserved.update(ord(c) for c in data.decode('cp932') if 0xe000 <= ord(c) <= 0xe757 and ord(c) not in codec.aliases)
        it.script = Script(data, codec, discovery.modern, variant['unicode'])
        items.append(it)
    if not items or ((discovery.modern or variant['unicode']) and not any(it.script.segments for it in items)):
        raise ValueError('封包中没有可识别的正文或选项。')
    font_item, layout, slots = None, None, {}
    if not variant['unicode']:
        fonts = [it for it in entries if norm(it.entry.name).lower() == 'system/font/fontdata.dat']
        if len(fonts) != 1:
            raise ValueError('缺少唯一的系统字体布局定义。')
        font_item = fonts[0]
        layout = font_item.read().decode('cp932')
        matches = list(re.finditer(r'(?m)^Font\[(\d+)\]\.List\[0\]=([^\r\n]+)', layout))
        slots = {int(m[1]): m[2] for m in matches}
        if discovery.modern:
            if len(slots) != len(matches) or not slots or not any(v in ('system', 'system_fix') for v in slots.values()):
                raise ValueError('此字体布局尚不支持系统字体替换。')
        else:
            for i in range(8):
                values = '外字登録' if i == 5 else ('system_fix' if version in ('6cd70ca295b40fb4', '8b3b903685e13fca') else '(?:system_fix|system)')
                if not re.search(rf'(?m)^Font\[{i}\]\.List\[0\]={values}\r?$', layout):
                    raise ValueError('字体布局与已支持的系统字体／独立外字方案不符。')
    def identity(it, segment):
        return it.pack+'|'+norm(it.entry.name)+'|'+segment.id
    if operation == 'extract':
        units = []
        for it in items:
            for s in it.script.segments:
                name = norm(it.entry.name)
                units.append(dict(id=identity(it, s), resourcePath=it.pack, locator=name+':'+s.id,
                                  sourceText=s.text, context=name+('；内部身份：'+s.identity if s.identity is not None else ''),
                                  speaker=s.speaker, kind='dialogue' if s.kind == 'text' else s.kind))
        return {'extraction': {'units': units, 'warnings': []}}
    if operation == 'validate':
        return {'validation': {'passed': True, 'issues': [], 'runtimeStatus': 'not-tested'}}
    if operation != 'build':
        raise ValueError('不支持的操作。')
    edits = json.loads(Path(request['translationsFile']).read_text(encoding='utf-8-sig')) if request.get('translationsFile') else request.get('translations', {})
    known = {identity(it, s): s for it in items for s in it.script.segments}
    if set(edits)-known.keys():
        raise ValueError('译文含未知文本标识：'+next(iter(set(edits)-known.keys())))
    changed = any(value != known[key].text for key, value in edits.items())
    output = Path(request['outputDirectory']).resolve()
    source = Path(root).resolve()
    if output == source or output.is_relative_to(source) or source.is_relative_to(output):
        raise ValueError('构建输出必须与输入目录分开。')
    output.mkdir(parents=True, exist_ok=True)
    outputs = set()
    def put(path, data):
        target = full(output, path)
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
        outputs.add(path)
    for p in files:
        target = full(output, p)
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(full(root, p), target)
        outputs.add(p)
    if changed:
        if not variant['unicode']:
            for it in items:
                for s in it.script.segments:
                    key = identity(it, s)
                    if key in edits and edits[key] != s.text:
                        if not isinstance(edits[key], str):
                            raise ValueError('译文不能为空值。')
                        codec.add_text(edits[key], reserved)
        replacements = {}
        def replace(it, data):
            replacements.setdefault(it.pack, {})[it.entry.name] = data
        def current(it):
            changes = replacements.get(it.pack, {})
            return changes[it.entry.name] if it.entry.name in changes else it.read()
        for it in items:
            local = {s.id: edits[identity(it, s)] for s in it.script.segments if identity(it, s) in edits and edits[identity(it, s)] != s.text}
            if local:
                replace(it, it.script.build(local))
        tools = here.parent/'tools' if (here.parent/'tools').is_dir() else here/'tools'
        apply(discovery, output, variant, entries, items, codec, font_item, layout, slots, current, replace, put, outputs, tools)
        archives = dict(discovery.archives)
        for pack, changes in replacements.items():
            archives[pack].build(full(output, pack), changes)
    warnings = variant['buildWarnings']
    if version == '6cd70ca295b40fb4':
        warnings = ['请从 启动汉化.exe 启动，并保留原程序和日语转区配置；游戏启动及显示尚未验证。']
    return {'build': {'files': sorted(outputs), 'warnings': warnings if changed else [],
                      'launchExecutable': LAUNCH if changed else None}}


def main():
    request = {}
    try:
        request = json.load(sys.stdin)
        response = dict(protocolVersion=1, success=True, **execute(request))
        status = 0
    except Exception as error:
        response = dict(protocolVersion=1, success=False, error='QLIE 适配失败：'+str(error))
        print(response['error'], file=sys.stderr)
        status = 1
    encoded = json.dumps(response, ensure_ascii=False)
    if request.get('responseFile'):
        Path(request['responseFile']).write_text(encoded, encoding='utf-8')
    else:
        print(encoded)
    return status


if __name__ == '__main__':
    if hasattr(sys.stdin, 'reconfigure'):
        sys.stdin.reconfigure(encoding='utf-8')
        sys.stdout.reconfigure(encoding='utf-8')
        sys.stderr.reconfigure(encoding='utf-8')
    sys.exit(main())
