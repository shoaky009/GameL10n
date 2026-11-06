"""Fixed JSON helper for Rust-owned adaptation. Subprocesses are not a sandbox."""
import ast
from datetime import datetime, timezone
import hashlib
from html.entities import name2codepoint
import io
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import time
import traceback
import unicodedata
import zipfile


def safe(root, relative):
    root = Path(root).resolve()
    if not isinstance(relative, str) or not relative or '\\' in relative or ':' in relative:
        raise ValueError('请使用目录内的相对路径。')
    parts = relative.split('/')
    if any(p in ('', '.', '..') for p in parts):
        raise ValueError('路径包含目录穿越。')
    path = root.joinpath(*parts)
    for part in (path, *path.parents):
        if part == root.parent:
            break
        if part.is_symlink() or (part.exists() and getattr(part.lstat(), 'st_file_attributes', 0) & 0x400):
            raise ValueError('路径不能使用重解析点。')
    if not path.resolve().is_relative_to(root):
        raise ValueError('路径超出指定目录。')
    return path


def page(text, request):
    # C# offsets and lengths count UTF-16 code units, including in retained tool history.
    encoded = text.encode('utf-16-le', errors='surrogatepass')
    total = len(encoded) // 2
    offset = reference_number(request, 'offset', 0, 0, total)
    length = reference_number(request, 'length', 8000, 1, 16000)
    query = reference_argument(request, 'query', '')
    if len(query.encode('utf-16-le')) // 2 > 256:
        raise ValueError('检索词过长。')
    found = True
    if query:
        remaining = encoded[offset * 2:].decode('utf-16-le', errors='surrogatepass')
        match = re.search(re.escape(query), remaining, re.IGNORECASE)
        found = match is not None
        if found:
            position = offset + len(remaining[:match.start()].encode('utf-16-le', errors='surrogatepass')) // 2
            offset = max(offset, position - 240)
    end = min(total, offset + length)
    content = encoded[offset * 2:end * 2].decode('utf-16-le', errors='replace') if found else ''
    return dict(content=content, offset=offset, nextOffset=end,
                more=end < total, totalCharacters=total, queryFound=found)


def workspace_text(request, path):
    suffix = path.suffix.lower()
    return suffix in ('.cs', '.py', '.ps1', '.md', '.json', '.csproj', '.rsp') or (
        request.get('workspaceRoot', False) and path.relative_to(Path(request['root']).resolve()).as_posix().lower().startswith('work/')
        and suffix in ('.txt', '.log', '.csv', '.jsonl'))


def iter_files(directory):
    for current, directories, files in os.walk(directory, followlinks=False):
        directories[:] = [name for name in directories if not (Path(current) / name).is_symlink()
                          and not (getattr((Path(current) / name).lstat(), 'st_file_attributes', 0) & 0x400)]
        for name in files:
            path = Path(current) / name
            if not path.is_symlink() and not (getattr(path.lstat(), 'st_file_attributes', 0) & 0x400):
                yield path


class InputFiles:
    """Host-selected original inputs; installed files remain a separate diagnostic view."""
    def __init__(self, request):
        self.root = Path(request['root']).resolve()
        self.overrides = {relative.casefold(): (relative, Path(path))
                          for relative, path in request.get('inputFiles', {}).items()}
        self.excluded = {relative.replace('\\', '/').casefold() for relative in request.get('excludedFiles', [])}

    def resolve(self, relative):
        logical = safe(self.root, relative)
        selected = self.overrides.get(relative.casefold())
        if selected:
            return logical, selected[1]
        if relative.casefold() in self.excluded:
            raise ValueError(f'文件不属于制作输入：{relative}')
        return logical, logical

    def files(self, relative=''):
        directory = self.root if relative in ('', '.') else safe(self.root, relative)
        seen = set()
        for logical in iter_files(directory):
            name = logical.relative_to(self.root).as_posix()
            key = name.casefold()
            if key in self.excluded and key not in self.overrides:
                continue
            seen.add(key)
            yield self.resolve(name)
        prefix = '' if relative in ('', '.') else relative.rstrip('/').casefold() + '/'
        for key, (name, _) in self.overrides.items():
            if key not in seen and key.startswith(prefix):
                yield self.resolve(name)


def list_files(request):
    view = InputFiles(request)
    root = view.root
    relative = request.get('path', '').replace('\\', '/')
    offset = reference_number(request, 'offset', 0, 0, 1_000_000)
    limit = reference_number(request, 'limit', 100, 1, 200)
    import itertools
    files = list(itertools.islice(view.files(relative), offset, offset + limit + 1))
    return dict(files=[dict(path=logical.relative_to(root).as_posix(), bytes=physical.stat().st_size) for logical, physical in files[:limit]],
                more=len(files) > limit, nextOffset=offset + min(len(files), limit))


def read_file(request):
    logical, path = InputFiles(request).resolve(request['path'])
    if logical.suffix.lower() in ('.key', '.pem', '.pfx') or logical.name.lower().startswith('.env'):
        raise ValueError('不能读取凭据文件。')
    total = path.stat().st_size
    offset = reference_number(request, 'offset', 0, 0, total)
    length = min(reference_number(request, 'length', 8192, 1, 32768), total - offset)
    encoding = request.get('encoding') or ('utf8' if workspace_text(request, logical) else 'hex')
    if encoding not in ('hex', 'utf8', 'cp932', 'cp936'):
        raise ValueError('encoding 必须是 hex、utf8、cp932 或 cp936。')
    with path.open('rb') as stream:
        stream.seek(offset)
        data = stream.read(length)
    end = offset + len(data)
    return dict(bytes=total, content=data.hex().upper() if encoding == 'hex' else data.decode(encoding, errors='replace'),
                encoding=encoding, offset=offset, length=len(data), more=end < total)


def search_text(request):
    view = InputFiles(request)
    relative = request['path'].replace('\\', '/')
    logical, path = (view.root, view.root) if relative in ('', '.') else view.resolve(relative)
    query = request['query']
    if not query or len(query.encode('utf-16-le')) // 2 > 256:
        raise ValueError('搜索文本长度必须在 1 到 256 之间。')
    def search_file(logical, current, offset, budget, maximum):
        encoding = request.get('encoding') or ('utf8' if workspace_text(request, logical) else 'cp932')
        if encoding not in ('utf8', 'cp932', 'cp936'):
            raise ValueError('不支持的文字编码。')
        size = current.stat().st_size
        if offset > size:
            raise ValueError('搜索偏移超出文件长度。')
        with current.open('rb') as stream:
            stream.seek(offset)
            data = stream.read(budget)
        content = data.decode(encoding, errors='replace')
        exact = content.encode(encoding, errors='replace') == data
        matches, offsets, consumed = [], [], len(data)
        for match in re.finditer(re.escape(query), content, re.IGNORECASE):
            start = max(0, match.start() - 80)
            matches.append(content[start:start + len(query) + 160])
            offsets.append(offset + len(content[:match.start()].encode(encoding)) if exact else None)
            if len(matches) == maximum:
                consumed = len(content[:match.end()].encode(encoding)) if exact else len(data)
                break
        return dict(matches=matches, byteOffsets=offsets, encoding=encoding, scannedBytes=len(data),
                    nextOffset=offset + consumed, more=offset + consumed < size)
    offset = reference_number(request, 'offset', 0, 0, 1_000_000 if path.is_dir() else (1 << 63) - 1)
    if path.is_file():
        return search_file(logical, path, offset, 4 << 20, 20)
    import itertools
    extensions = {'.cs','.py','.ps1','.md','.txt','.json','.jsonl','.xml','.csv','.log','.ini','.cfg','.ks','.rpy',
                  '.csproj','.rsp','.c','.h','.cpp','.hpp','.yaml','.yml','.toml'}
    files = list(itertools.islice(((logical, physical) for logical, physical in view.files(relative)
                                  if logical.suffix.lower() in extensions and not logical.name.lower().startswith('.env')), offset, offset + 65))
    results, scanned_bytes, scanned_files, next_offset = [], 0, 0, offset
    next_byte = reference_number(request, 'fileByteOffset', 0, 0, (1 << 63) - 1)
    for logical, current in files[:64]:
        if len(results) >= 20 or scanned_bytes >= 4 << 20:
            break
        relative = logical.relative_to(view.root).as_posix()
        found = search_file(logical, current, next_byte, min(256 << 10, (4 << 20) - scanned_bytes), 20 - len(results))
        scanned_files += 1
        scanned_bytes += found['scannedBytes']
        results += [dict(path=relative, byteOffset=at, encoding=found['encoding'], excerpt=excerpt)
                    for excerpt, at in zip(found['matches'], found['byteOffsets'])]
        if found['more']:
            next_byte = found['nextOffset']
            break
        next_offset += 1
        next_byte = 0
    return dict(matches=results, scannedFiles=scanned_files, scannedBytes=scanned_bytes,
                nextOffset=next_offset, nextFileByteOffset=next_byte,
                more=next_byte > 0 or next_offset - offset < len(files))


REFERENCE_STOP_WORDS = frozenset(('archive', 'archives', 'engine', 'engines', 'python', 'py',
                                  'format', 'formats', 'script', 'scripts', 'file', 'files',
                                  'reference', 'references', 'tool', 'tools'))
REFERENCE_ALIASES = {'system4': 'alicesoft', 'system40': 'alicesoft'}
REFERENCE_ROOT = 'https://raw.githubusercontent.com/GalTransl/GalTransl-GalGameReverse-Skill/main/'
REFERENCE_SEARCH_USAGE = ('资料命中不代表已验证支持。优先 inspect_engine_reference(engine=id) 读取当前引擎指南与 API；固定版本共享源码库随完整发行包提供，所有项目本地复用，不在分析时补下载。'
                          '用 run_engine_python 调用现成 Python 模块验证样本，避免阅读/改写整套算法；use_engine_reference 可供正式适配器复用。'
                          '共享库不可用时可按 documentUrl 在线调查。未命中不代表不支持；可按 formats/registrations 继续查实际魔数。'
                          '按文件核对许可并保留来源；不自动部署 UIF、字体或 hook DLL。')
REFERENCE_INSPECT_USAGE = ('先用现成 CLI（run_engine_python 的 module 参数）或 work/*.py 小样本调用 python.engines / python.archives 的现成函数，工具自动提供共享解释器和库。'
                           '模块接口按引擎和版本不同，请按 publicSymbols/指南定位，不要把整个算法改写为 C#。'
                           '只补资源选择、容器与脚本连接及版本差异。需要正式复用时 use_engine_reference 生成固定版本依赖和 Python 组件接口，'
                           '将 JSON CLI 入口放到 src/adapter.py，再通过 test_adapter 验收。可选第三方模块不自动安装，缺依赖需明确报告。')
KIRIKIRI_SUMMARY = 'Kirikiri KS/SOC 内置解析器可读 CP932、带 BOM 的 UTF-8/UTF-16，修改后写 UTF-16；先用 try_builtin_adapter 验证少量解包样本，再 copy_adapter 复用。运行时是否接受 Unicode 须沿实际脚本加载路径确认，未搜到 BOM 常量不能排除支持。不要用 CP932/CP936 混写加私有注释标记代替游戏端解码支持；小样须逐条比较译文与未修改项，编码失败必须明确失败。Unicode 写回后仍要检查原字体是否覆盖中文；有内置日文字体或个别字重叠时，使用 windows-font-metrics 组件并沿字体回退、测宽/绘制及存档设置恢复路径修复，不以全局加字距掩盖缺字。'
KIRIKIRI_FONT_LAYOUT = 'Unicode 脚本可读写不等于原日文字体覆盖中文。检查实际 userFace/defaultFace、System.addFont、mapPrerenderedFont 及 font/reset/restore 路径；先用 windows-font-metrics 在同高度/宽度/charset 下比较原字体和覆盖中文的候选字体。缺字时 getTextWidth 与 drawText 可能使用不同回退或度量，造成个别字重叠；需让测宽和绘制使用同一套已覆盖的字形。优先使用引擎已有字体配置或集中字体选择点，保留原字号、风格及正常日文/符号/ASCII；不要插空格、改译文、全局加 pitch、缩小全文或套 QLIE 专用度量。核对 chDefaultFace 等系统设置及读档恢复是否覆盖新配置，不删除用户存档；必要时只迁移已确认的原字体值。原文构建仍须逐字节一致；中文构建才按已确认条件修改字体配置，并独立核对配置已写入实际生效资源。预渲染字库/GDI 以外后端须单独检查；独立测量通过仍需试译确认游戏内排版。'
BSSCRIPT_GUIDANCE = ('BSScript Rev.6 不是 Bishop GSC 或 KaGuYa 的同一脚本格式，不能按厂商或可见日文直接套解析器。'
                     '先确认头部各段边界、字符串偏移表与池；已有读写和原字节往返结论时直接复用，继续按指令引用区分对白、名称、选择项与资源路径。'
                     '严格 CP932 无法编码简体中文是预期限制；用 inspect_pe_imports 查询实际字体/编码导入和候选调用位置，再确认字节遍历、字符范围及字形路径。'
                     '可检查 cp932-glyph-map 组件，但须验证当前引擎的可用码位和字体映射，不能仅改成 CP936 或把尚未实现的环节判为不支持。')


def reference_argument(item, name, fallback=None):
    value = item.get(name)
    if isinstance(value, str):
        return value
    if fallback is not None:
        return fallback
    raise ValueError('Missing string argument: ' + name)


def reference_number(item, name, fallback, minimum, maximum):
    value = item.get(name, fallback)
    if type(value) is not int:
        raise ValueError(f'参数 {name} 必须是 {minimum} 到 {maximum} 之间的整数。')
    if not minimum <= value <= maximum:
        raise ValueError(f'参数 {name}={value} 超出范围；允许 {minimum} 到 {maximum}，默认 {fallback}。')
    return value


def reference_limit(text, maximum):
    encoded = text.encode('utf-16-le', errors='surrogatepass')
    if len(encoded) <= maximum * 2:
        return text
    return encoded[:maximum * 2].decode('utf-16-le', errors='replace') + '\n[truncated]'


def kirikiri_scn_support():
    return dict(component='kirikiri-scn-text',
                condition='已解密 PSB v2/v3 的 KAGEX SCN；不能据此认定 XP3 的过滤器相同。Hxv4 与 Adler 等过滤变体分别确认。',
                next='inspect_adapter_component(id=kirikiri-scn-text)，copy_adapter_components 后直接 run_engine_python 运行组件的 verify JSON CLI，先验证一个原始解包样本。',
                semanticLayer='正文、显示名、选项才是独立译文；朗读/搜索缓存与可见长度由现成 kirikiri_scn writer 同步重算，禁止对它们独立翻译。未知布局要报告具体不兼容项，只扩展该层。',
                existingApis=['python.engines.kirikiri_hxv4_text', 'python.engines.kirikiri_scn', 'python.engines.kirikiri_psb'],
                moduleEntry='run_engine_python(module=python.engines.kirikiri_hxv4_text, arguments=[--help]) 可直接查看现成 CLI，无需编写 runpy 脚本。高层 extract/pack 只解决其声明范围；最终整包重建、字体、脚本覆盖顺序仍需验证。',
                fontCheck='Unicode 读写成功后仍需检查原字体的中文覆盖。内置字体或个别字重叠时，inspect_adapter_component(id=windows-font-metrics) 后直接调用诊断脚本，核对字体回退及旧设置恢复。')


def siglus_reference_capability():
    return dict(id='engine.siglus', version='1.0.0', name='Siglus / Scene.pck',
                upstream='SiglusSceneScriptUtility 0.5.7 (Unlicense OR 0BSD)',
                capabilities=['Scene.pck 解包及重封包', '内嵌源码查询', '字节码参数字符串引用', '变长 UTF-16 回填', 'GDI 字体检查及 FKDS 图片字库补字', '独立中文启动入口'],
                templates=['标准 CD_TEXT/CD_NAME', 'FKDS 气泡对白'],
                sources=['gamel10n_siglus.py', 'siglus_binary.py', 'siglus_font.py', 'siglus_ssu/pck.py', 'siglus_ssu/dat.py', 'siglus_ssu/g00.py', 'siglus_ssu/disam.py', 'siglus_ssu/GEI.py'],
                usage=('inspect_siglus_game 直接分析当前游戏，返回结构、已识别文本和待处理调用；source/query 按需读取内嵌源码。'
                       'copy_adapter id=engine.siglus/version=1.0.0 生成固定依赖和 JSON 调用桥。规则放在 src/tools/siglus-rules.json，'
                       "commandRules:[{command:'精确函数名',argument:1,kind:'dialogue'}] 的 argument 从 0 开始。"
                       '仅选择源码或已验证语义确认的显示参数；不要按字符串内容猜测引用。test_adapter 仍须通过。'),
                limits='引擎版本、显示模板和 EXE 补丁特征分别检查；未识别的调用不自动翻译，游戏实际显示需试译确认。')


VALKYRIA_GUIDANCE = (
    'DAT/SDT 先复用固定库的容器与字符串结构，定长回写能力不代表已支持 VM 变长重定位。'
    '采用追加指令时，须用实际指令解析或调度路径核对跳转基准、返回位置和原入口；从输出实际跳转目标解析正文/姓名/选项，再比较预期译文，sidecar 偏移须与该处一致。'
    '生成的 sidecar 能复提译文不证明引擎会执行它；译文放在不可达尾缀而原指令仍含日文的产物应验收失败。'
    '中文字体先检查 EXE 实际创建/测量/绘制调用：若使用 GDI+，运行 windows-font-metrics（renderer="gdiplus"）对比原字体与候选。'
    'VNTextProxy v0.0.41 只钩取 GDI/Direct2D，私有 GDI 注册不等于 GDI+ 字体创建。'
    '若 fromDC/fromLogfont 失败而 privateCollection 成功，应处理创建入口及集合生命周期，不能因 NotTrueTypeFont 就反复下载或转换字体。'
    '此时先 inspect_adapter_component(id="gdiplus-font-bridge")：固定 x86 组件支持普通导入的 GdipCreateFontFromDC/LogfontA/W，复用私有集合并保留原名 EXE、IAT 地址和 ASLR；不生成机器码补丁。动态/延迟调用或特殊字体变换须另查，不能按厂商套用固定地址。')


def reference_guidance(query):
    guidance = [dict(topic='缺口定位', url=REFERENCE_ROOT + 'guides/extending-engines.md',
                     note='分别确认容器、解码、文本语义、回填、打包、运行显示。缺 writer 或已知算法尚未实现时继续补齐当前层；没有搜索结果不是不支持的证据。')]
    if query == 'kag' or any(term in query for term in ('kirikiri', 'krkr', 'xp3', '吉里吉里', 'hxv4', 'kagex', 'psb', 'scn')):
        guidance.append(dict(topic='Kirikiri 脚本编码', url=REFERENCE_ROOT + 'engines/kirikiri.md', note=KIRIKIRI_SUMMARY))
    if 'qlie' in query or 'imos' in query:
        guidance.append(dict(topic='QLIE 方言', url=REFERENCE_ROOT + 'engines/qlie.md',
                             note='先确认 PACK/Hash 版本与脚本方言。核对内部姓名和显示姓名（＠）、多行对白、^select 与跳转命令、^savetext；[pc,...] 可能是布局命令或显示文本，按实际语法判断，不能一律翻译或冻结。'))
    if 'bishop' in query or 'bsscript' in query or query == 'bsx':
        guidance.append(dict(topic='Bishop 格式区别', url=REFERENCE_ROOT + 'engines/bishop.md', note=BSSCRIPT_GUIDANCE))
    if 'nekosdk' in query or 'nekopack' in query:
        guidance.append(dict(topic='NekoSDK 的解析与显示分别验证', url=REFERENCE_ROOT + 'engines/nekosdk.md',
                             note='先调用 python.archives.nekopack 的 open_pak/read_member/build_pak，以及 python.engines.nekosdk 的 extract_fields/replace_fields。字段为空可能是命令记录变体，不代表无文本；用已解包样本确认结构后只补变体。容器和往返通过后集中处理当前显示路径，避免重新搜索已解决的解包算法。若采用 VNTextProxy，固定代理版本，核对映射表索引、过滤字节和实际 EXE 字符遍历边界，再批量测试边界码位及容量；单个字形探针不证明游戏运行兼容。'))
    if 'valkyria' in query or query == 'sdt':
        guidance.append(dict(topic='Valkyria SDT 与字体创建', url=REFERENCE_ROOT + 'engines/valkyria.md',
                             note=VALKYRIA_GUIDANCE))
    if any(term in query for term in ('cp932', 'sjis', 'shiftjis', 'jis', '字体', '编码', 'alicesoft', 'ain', 'qlie', 'imos')):
        guidance.append(dict(topic='中文编码与字符映射', url=REFERENCE_ROOT + 'guides/jis-substitution.md',
                             note='先证明脚本确为 CP932。映射在整个补丁范围统一规划，避开未翻译正文、姓名、界面字及已有映射；译文缓存保留真实中文。重提取实际产物并比对修改与未修改项。编码可逆不证明原引擎接受私用码位或游戏内字形正确；不能默认部署参考项目的 DLL。'))
    return guidance


def reference_path(path):
    if (not isinstance(path, str) or not path or path.startswith('/')
            or any(part in ('', '.', '..') for part in path.split('/'))
            or any(not (c.isascii() and (c.isalnum() or c in '-_./')) for c in path)):
        raise ValueError('参考索引含无效资源路径。')
    return path


def reference_text(source, path):
    path = reference_path(path)
    try:
        entry = source.getinfo(path)
    except KeyError:
        raise FileNotFoundError('工具库中没有该文件：' + path) from None
    if entry.file_size > 4 << 20:
        raise ValueError('请读取更具体的参考文件。')
    return source.read(entry).decode('utf-8-sig', errors='replace')


def reference_catalog(source):
    index = json.loads(reference_text(source, 'catalog/engines.json'))
    engines = index.get('engines') if isinstance(index, dict) else None
    if not isinstance(engines, list) or any(not isinstance(item, dict) for item in engines):
        raise ValueError('引擎参考索引结构已变化。')
    return engines


def reference_strings(item, name):
    values = item.get(name, [])
    return [value for value in values if isinstance(value, str)] if isinstance(values, list) else []


def reference_key(text):
    return ''.join(c for c in text if reference_character(c)).lower()


def reference_character(character):
    category = unicodedata.category(character)
    return ord(character) <= 0xffff and (category.startswith('L') or category == 'Nd')


def reference_terms(text):
    words = ''.join(c if reference_character(c) else ' ' for c in text.lower()).split()
    return list(dict.fromkeys(term for term in words
                              if len(term) >= 2 and term not in REFERENCE_STOP_WORDS))


def reference_rank(item, key, terms):
    identity = reference_argument(item, 'id', '')
    title = reference_argument(item, 'title', '')
    aliases = reference_strings(item, 'aliases')
    if identity == 'kirikiri':
        aliases += ['Hxv4', 'KAGEX', 'PSB SCN']
    exact = (3 if reference_key(identity) == key else
             2 if any(reference_key(alias) == key for alias in aliases) else
             1 if terms and reference_key(title) == key else 0)
    fields = [(identity, 12), (title, 6)]
    fields += [(alias, 10) for alias in aliases]
    fields += [(path, 8) for path in reference_strings(item, 'python_references')]
    fields = [(reference_terms(value), weight) for value, weight in fields]
    matched, score = 0, 0
    for term in terms:
        weight = max(weight if term in words else
                     weight // 2 if len(term) >= 3 and any(word.startswith(term) for word in words) else 0
                     for words, weight in fields)
        if weight:
            matched += 1
            score += weight
    return exact, matched, score


def reference_summary(item, root):
    identity = reference_argument(item, 'id')
    return dict(id=identity, title=reference_limit(reference_argument(item, 'title'), 300),
                aliases=reference_strings(item, 'aliases')[:12],
                documentUrl=root + reference_path(reference_argument(item, 'document')),
                sourceUrls=[root + reference_path(path) for path in reference_strings(item, 'python_references')[:16]],
                publicSymbols=item.get('public_symbols'),
                scope=reference_limit(reference_argument(item, 'scope', 'Read the engine page for version and stage limits.'), 1000),
                evidence=reference_limit(reference_argument(item, 'evidence', 'Reference only; no game verification implied.'), 1000),
                readySupport=kirikiri_scn_support() if identity == 'kirikiri' else None)


def search_engine_references(request):
    query = reference_argument(request, 'query').strip()
    if not 2 <= len(query.encode('utf-16-le')) // 2 <= 128:
        raise ValueError('请使用 2 到 128 字符的引擎名称、别名或格式标识查询。')
    key = reference_key(query)
    if key in ('siglus', 'siglusengine', 'scenepck'):
        return dict(query=query, status='bundled', capability=siglus_reference_capability())
    if len(key) < 2:
        raise ValueError('查询必须包含引擎名称或格式标识。')
    key = REFERENCE_ALIASES.get(key, key)
    terms = [REFERENCE_ALIASES.get(term, term) for term in reference_terms(query)]
    root = REFERENCE_ROOT
    if request.get('revision'):
        root = root.replace('/main/', '/' + request['revision'] + '/')
    url = root + 'catalog/engines.json'
    error, content_hash, retrieved_at, reused, stale = None, None, None, None, None
    matches, total = [], 0
    try:
        if request.get('referenceIndexError'):
            raise OSError(request['referenceIndexError'])
        if 'referenceIndexContent' in request:
            # Rust owns HTTP/cache/cancellation; both sources use the same catalog ranking.
            content = reference_argument(request, 'referenceIndexContent')
            if len(content.encode('utf-8')) > 8 << 20:
                raise ValueError('引擎参考索引超过 8 MiB 读取上限。')
            metadata = request.get('referenceIndexMetadata') or {}
            root = REFERENCE_ROOT
            url = metadata.get('url', root + 'catalog/engines.json')
            content_hash = metadata.get('contentSha256') or hashlib.sha256(content.encode('utf-8')).hexdigest()
            retrieved_at, reused, stale = (metadata.get(name) for name in ('retrievedAt', 'reused', 'stale'))
        else:
            with zipfile.ZipFile(request['library']) as source:
                content = reference_text(source, 'catalog/engines.json')
            content_hash = hashlib.sha256(content.encode('utf-8')).hexdigest()
            retrieved_at = datetime.now(timezone.utc).isoformat()
            reused, stale = True, False
        index = json.loads(content)
        engines = index.get('engines') if isinstance(index, dict) else None
        if not isinstance(engines, list) or any(not isinstance(item, dict) for item in engines):
            raise ValueError('引擎参考索引结构已变化。')
        ranked = [(reference_rank(item, key, terms), item) for item in engines]
        ranked = [(rank, item) for rank, item in ranked if rank[0] or rank[1]]
        ranked.sort(key=lambda pair: (*(-value for value in pair[0]), pair[1]['id']))
        total = len(ranked)
        matches = [reference_summary(item, root) for _, item in ranked[:6]]
    except (OSError, zipfile.BadZipFile, KeyError, ValueError) as exception:
        error = reference_limit(str(exception), 500)
    return dict(query=query, status='queried' if error is None else 'unavailable', error=error,
                indexUrl=url, contentSha256=content_hash,
                retrievedAt=retrieved_at, reused=reused, stale=stale,
                totalMatches=total, matches=matches, more=total > len(matches),
                hint='请改用具体引擎名、别名或格式标识；archive、engine 等泛词不作为检索依据。未命中不代表不支持。' if error is None and total == 0 else None,
                guidance=reference_guidance(key),
                furtherResearch=dict(formats=REFERENCE_ROOT + 'catalog/formats.json',
                                     registrations=REFERENCE_ROOT + 'catalog/source-registries.json',
                                     formatNotes=REFERENCE_ROOT + 'catalog/format-research.md',
                                     provenance=REFERENCE_ROOT + 'provenance/NOTICE.md',
                                     sources=REFERENCE_ROOT + 'provenance/sources.json'),
                usage=REFERENCE_SEARCH_USAGE)


def reference_patch_delivery(engine):
    if engine == 'kirikiri':
        return dict(module='python.archives.xp3',
                    call="build([(name, bytes), ...], filter_name='none')",
                    condition='现有证据已确认无过滤器及覆盖加载优先级。过滤型 XP3 沿用已验证的对应 writer，不能默认用明文包替代。')
    if engine == 'artemis':
        return dict(module='python.archives.pfs',
                    call='build([(name, bytes, unknown), ...], version=observed_version, name_encoding=observed_encoding)',
                    condition='现有证据已确认独立 PFS 数字卷及加载优先级，沿用已确认的卷版本、命名和路径规则。')
    return None


def inspect_engine_reference(request):
    engine = reference_argument(request, 'engine')
    with zipfile.ZipFile(request['library']) as source:
        selected = next((item for item in reference_catalog(source)
                         if reference_argument(item, 'id', '').lower() == engine.lower()), None)
        if selected is None:
            raise ValueError('请使用 search_engine_references 返回的精确引擎 ID。')
        path = reference_path(reference_argument(request, 'path', reference_argument(selected, 'document')))
        if not path.startswith(('python/', 'engines/', 'guides/', 'catalog/', 'provenance/', 'LICENSE')):
            raise ValueError('请读取工具库中的指南、目录或 Python 源码。')
        result = dict(path=path, revision=request.get('revision'), **page(reference_text(source, path), request))
        result.update(engine=engine, publicSymbols=selected.get('public_symbols'),
                      pythonModules=reference_strings(selected, 'python_references'),
                      readySupport=kirikiri_scn_support() if engine == 'kirikiri' else None,
                      fontLayout=KIRIKIRI_FONT_LAYOUT if engine == 'kirikiri' else None,
                      usage=REFERENCE_INSPECT_USAGE)
        if engine == 'valkyria':
            result['runtimeGuidance'] = VALKYRIA_GUIDANCE
        delivery = reference_patch_delivery(reference_argument(selected, 'id').lower())
        if delivery:
            result['patchDelivery'] = dict(delivery,
                usage='仅输出修改成员及必要伴随资源，原资源＋补丁联合复提并保留文本身份；沿用现有验收阶段。',
                fallback='规则或复用能力不明时保持 replace；不为体积增加研究调用、下载或 Hook 开发。')
        return result


def library(request):
    if request.get('engine'):
        return inspect_engine_reference(request)
    if request.get('path'):
        path = reference_path(request['path'])
        with zipfile.ZipFile(request['library']) as source:
            return dict(path=path, **page(reference_text(source, path), request))
    return search_engine_references(request)


def builtin_sources(request):
    parser = reference_argument(request, 'parserType').removeprefix('VNTextPatch.Shared.Scripts.')
    namespace, separator, _ = parser.rpartition('.')
    engine_path = 'vendor/Scripts/' + (namespace.replace('.', '/') + '/' if separator else parser + '.cs')
    def selected(path):
        return path.endswith('.cs') and (
            path in ('worker/Worker.cs', 'worker/Protocol.cs', 'worker/Serialization/VnTextPatchJson.cs',
                     'vendor/Scripts/PlainTextScript.cs', 'vendor/Scripts/Range.cs')
            or namespace == 'Yuris' and path == 'worker/YurisTextEncoding.cs'
            or path.startswith('vendor/Util/')
            or path.startswith('vendor/') and '/' not in path[7:]
            or (path.startswith(engine_path) if separator else path == engine_path))
    with zipfile.ZipFile(request['bundle']) as archive:
        entries = sorted((item for item in archive.infolist() if selected(item.filename)), key=lambda item: item.filename)
        if 'path' not in request:
            return dict(sourceFiles=[dict(path=item.filename, bytes=item.file_size) for item in entries])
        path = reference_argument(request, 'path').replace('\\', '/')
        entry = next((item for item in entries if item.filename == path), None)
        if entry is None:
            raise ValueError('该路径不在所选解析器的源码索引中，请先读取 inspect_builtin_adapter 返回的 sourceFiles。')
        if entry.file_size > 4 * 1024 * 1024:
            raise ValueError('内置解析器单个源码文件超过读取上限。')
        content = archive.read(entry).decode('utf-8-sig')
        encoded = content.encode('utf-16-le')
        total = len(encoded) // 2
        offset = min(reference_number(request, 'offset', 0, 0, 2**31-1), total)
        length = reference_number(request, 'length', 12000, 1, 24000)
        end = min(total, offset + length)
        return dict(format=request.get('format'), path=path, offset=offset, length=end-offset,
                    totalLength=total, nextOffset=end, truncated=end < total,
                    content=encoded[2*offset:2*end].decode('utf-16-le', errors='replace'))


def prepare_siglus(request):
    assets = Path(request['assets'])
    wheel = assets / 'third_party/SiglusSSU/siglus_ssu-0.5.7-cp314-cp314-win_amd64.whl'
    wheel_hash = hashlib.sha256(wheel.read_bytes()).hexdigest()
    if wheel_hash != '709752e8048400d326824729dddb167b21005cd3a7878c5827b370dfae2df23e':
        raise ValueError('内置 Siglus 上游工具哈希不一致。')
    patches = json.loads((assets / 'adapters/siglus/upstream-patches.json').read_text(encoding='utf-8-sig'))
    files = {}
    with zipfile.ZipFile(wheel) as source:
        for entry in source.infolist():
            if entry.is_dir():
                continue
            name, data = entry.filename, source.read(entry)
            if name in patches:
                text = data.decode('utf-8').replace('\r\r\n', '\n').replace('\r\n', '\n')
                for old, new in patches[name]:
                    if text.count(old) != 1:
                        raise ValueError('Siglus 上游接口发生变化。')
                    text = text.replace(old, new)
                data = text.encode('utf-8')
            files[name] = data
    for name in ('gamel10n_siglus.py', 'siglus_binary.py', 'siglus_font.py'):
        files[name] = (assets / 'adapters/siglus' / name).read_bytes()
    identity = hashlib.sha256(''.join(name + ':' + hashlib.sha256(data).hexdigest() + '\n' for name, data in sorted(files.items())).encode()).hexdigest()
    root = Path(request['shared']) / 'siglus' / identity[:20]
    for name, data in files.items():
        path = safe(root, name)
        if path.exists():
            if hashlib.sha256(path.read_bytes()).digest() != hashlib.sha256(data).digest():
                raise ValueError('共享 Siglus 文件已变化：' + name)
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
    entry = root / 'adapter.py'
    content = b'import sys\nfrom gamel10n_siglus import main\nsys.exit(main())\n'
    if entry.exists() and entry.read_bytes() != content:
        raise ValueError('共享 Siglus 入口已变化。')
    entry.write_bytes(content)
    return dict(entry=str(entry), library=str(root), revision=identity[:40], librarySha256=identity, sourceSha256=wheel_hash, kind='siglus')


def prepare_siglus_zip(request):
    library = Path(request['library'])
    root = Path(request['shared']) / 'references' / request['reference']['revision']
    target = root / 'library.zip'
    if target != library:
        target.parent.mkdir(parents=True, exist_ok=True)
        data = library.read_bytes()
        if target.exists() and target.read_bytes() != data:
            raise ValueError('共享 Siglus 工具内容不一致。')
        if not target.exists(): target.write_bytes(data)
    name = 'siglus_ssu/native_accel.cp314-win_amd64.pyd'
    with zipfile.ZipFile(library) as archive:
        data = archive.read(name)
        if len(data) > 8 << 20: raise ValueError('Siglus 原生加速模块大小异常。')
    native = safe(root / 'native', name)
    native.parent.mkdir(parents=True, exist_ok=True)
    if native.exists() and native.read_bytes() != data:
        raise ValueError('Siglus 原生加速模块内容已变化。')
    if not native.exists(): native.write_bytes(data)
    entry = root / 'adapter.py'
    content = b'import sys\nfrom gamel10n_siglus import main\nsys.exit(main())\n'
    if entry.exists() and entry.read_bytes() != content: raise ValueError('共享 Siglus 入口已变化。')
    if not entry.exists(): entry.write_bytes(content)
    return dict(request['reference'], entry=str(entry), library=str(target))


def run(request):
    workspace = Path(request['workspace']).resolve()
    script = request.get('script')
    module = request.get('module')
    if bool(script) == bool(module):
        raise ValueError('script 和 module 必须且只能指定一个。')
    if script:
        script = script.replace('\\', '/')
        if not script.startswith(('work/', 'src/tools/')) or not script.lower().endswith('.py'):
            raise ValueError('脚本必须是 work/*.py 或已存在的 src/tools/*.py。')
        path = safe(workspace, script)
        if 'content' in request:
            if not script.startswith('work/'):
                raise ValueError('内联分析脚本必须位于 work/；正式源码请用 write_file 修改。')
            if not isinstance(request['content'], str) or len(request['content'].encode('utf-8')) > 512 << 10:
                raise ValueError('单次写入文件过大。')
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(request['content'], encoding='utf-8', newline='\n')
        target = [str(path)]
    else:
        if not re.fullmatch(r'(?:python\.(?:engines|archives)|siglus_ssu)\.[A-Za-z_][A-Za-z_0-9]*(?:\.[A-Za-z_][A-Za-z_0-9]*)*', module):
            raise ValueError('只能调用已固定的引擎库模块。')
        target = ['-m', module]
    extra = [str(workspace / 'src'), str(workspace), request.get('library', '')]
    code = "import runpy,sys;sys.dont_write_bytecode=True;sys.path[:0]=" + repr([p for p in extra if p]) + ";a=sys.argv[1:];sys.argv=a[1:] if a[0]=='-m' else a;runpy.run_module(a[1],run_name='__main__') if a[0]=='-m' else runpy.run_path(a[0],run_name='__main__')"
    arguments = request.get('arguments', [])
    if not isinstance(arguments, list) or len(arguments) > 128 or not all(isinstance(v, str) for v in arguments) or sum(len(v.encode('utf-16-le')) // 2 for v in arguments) > 65536:
        raise ValueError('参数无效。')
    environment = os.environ.copy()
    paths = request.get('toolPaths') or {}
    if paths.get('hostCommand'): environment['GAMEL10N_ADAPTER_HOST'] = json.dumps(paths['hostCommand'])
    if paths.get('vnTextPatchCommand'): environment['GAMEL10N_VNTEXTPATCH_COMMAND'] = json.dumps(paths['vnTextPatchCommand'])
    return run_process([sys.executable, '-I', '-X', 'utf8', '-c', code, *target, *arguments], workspace, request, environment)


def run_process(command, workspace, request, environment=None):
    relative = request.get('workingDirectory', '.')
    directory = workspace if relative == '.' else safe(workspace, relative.replace('\\', '/'))
    directory.mkdir(parents=True, exist_ok=True)
    timeout = reference_number(request, 'timeoutSeconds', 120, 1, 600)
    with tempfile.TemporaryFile() as out, tempfile.TemporaryFile() as err:
        process = subprocess.Popen(command, cwd=directory, stdin=subprocess.PIPE, stdout=out, stderr=err,
                                   env=environment, creationflags=0x08000000 if os.name == 'nt' else 0,
                                   start_new_session=os.name != 'nt')
        started = time.monotonic()
        stdin = request.get('stdin', '').encode('utf-8')
        failure = None
        try:
            while True:
                try:
                    process.communicate(stdin, timeout=0.05)
                    break
                except subprocess.TimeoutExpired:
                    stdin = None
                    if os.fstat(out.fileno()).st_size + os.fstat(err.fileno()).st_size > 1 << 20:
                        failure = '工具输出超过 1 MiB 限制。'
                        break
                    if time.monotonic() - started >= timeout:
                        failure = '工具执行超过时间限制。'
                        break
        finally:
            if process.poll() is None:
                if os.name == 'nt':
                    subprocess.run(['taskkill', '/PID', str(process.pid), '/T', '/F'], stdout=subprocess.DEVNULL,
                                   stderr=subprocess.DEVNULL, creationflags=0x08000000)
                else:
                    import signal
                    os.killpg(process.pid, signal.SIGKILL)
                process.kill()
                process.wait()
        if os.fstat(out.fileno()).st_size + os.fstat(err.fileno()).st_size > 1 << 20:
            failure = failure or '工具输出超过 1 MiB 限制。'
        out.seek(0); err.seek(0)
        stdout = out.read(1 << 20)
        stderr = err.read((1 << 20) - len(stdout))
    return dict(success=failure is None and process.returncode == 0, exitCode=process.returncode,
                stdout=stdout.decode('utf-8', errors='replace'), stderr=stderr.decode('utf-8', errors='replace'),
                phase='run', timedOut=failure is not None and '时间' in failure, error=failure)


def run_tool(request):
    workspace = Path(request['workspace']).resolve()
    relative = request['executable']
    if relative == 'dotnet':
        raise ValueError('Rust 宿主不提供共享 .NET 运行时；分析逻辑请用 Python，解析器使用固定原生工具。')
    powershell = relative == 'powershell'
    if powershell:
        executable = Path(os.environ.get('SystemRoot', 'C:/Windows')) / 'System32/WindowsPowerShell/v1.0/powershell.exe'
    else:
        if not relative.lower().endswith('.exe'):
            raise ValueError('只能运行已准备的工作区 EXE 或 powershell；分析脚本请用 Python。')
        executable = safe(workspace, relative.replace('\\', '/'))
    arguments = request.get('arguments', [])
    if not isinstance(arguments, list) or len(arguments) > 128 or not all(isinstance(value, str) for value in arguments) or sum(len(value.encode('utf-16-le')) // 2 for value in arguments) > 65536:
        raise ValueError('arguments 必须是最多 128 项的字符串数组。')
    if powershell:
        arguments = ['-NoLogo', '-NoProfile', '-NonInteractive', *arguments]
    return run_process([str(executable), *arguments], workspace, request)


def archive_component(request):
    sys.path[:0] = [request['components'], request['library']]
    import engine_archive
    payload = request['request']
    try:
        report = engine_archive.execute(payload)
    except Exception as error:
        report = dict(protocolVersion=1, success=False, component=payload['component'], error=str(error))
    manifest = Path(payload['responseFile'])
    manifest.parent.mkdir(parents=True, exist_ok=True)
    with manifest.open('x', encoding='utf-8', newline='\n') as stream:
        json.dump(report, stream, ensure_ascii=False)
    return dict(report, entries=report.get('entries', [])[:12])


def system_ansi_code_page():
    if os.name != 'nt':
        return None
    import ctypes
    return ctypes.windll.kernel32.GetACP()


def launch_text_sample(root, relative, fallback, tail=False):
    try:
        with safe(root, relative).open('rb') as stream:
            total = os.fstat(stream.fileno()).st_size
            size = min(total, 4096)
            header = stream.read(3)
            unicode = 'utf-16-le' if header.startswith(b'\xff\xfe') else 'utf-16-be' if header.startswith(b'\xfe\xff') else None
            offset = total - size if tail else 0
            if unicode and offset % 2:
                offset += 1
                size -= 1
            stream.seek(offset)
            data = stream.read(size)
        if not unicode and offset > 0 and b'\n' in data:
            data = data.split(b'\n', 1)[1]
        encoding = ('utf-16' if unicode == 'utf-16-le' else 'utf-16BE') if unicode else 'utf8'
        try:
            content = data.decode(unicode, errors='replace') if unicode else data.decode('utf-8', errors='strict')
            content = content.lstrip('\ufeff')
        except UnicodeDecodeError:
            if fallback is None:
                return dict(path=relative, encoding='unknown', content='', truncated=total > size,
                            error='编码未确认，请用 read_file 指定编码读取。')
            content = data.decode('cp' + str(fallback), errors='replace')
            encoding = f'cp{fallback} (inferred; verify if garbled)'
        units = content.encode('utf-16-le', errors='surrogatepass')
        truncated = total > size or len(units) > 2400
        if len(units) > 2400:
            units = units[-2400:] if tail else units[:2400]
            content = units.decode('utf-16-le', errors='replace')
        return dict(path=relative, encoding=encoding, content=content, truncated=truncated, error=None)
    except (OSError, LookupError) as error:
        return dict(path=relative, encoding='unknown', content='', truncated=False, error=str(error))


def inspect_game_launch(request):
    from itertools import islice
    from xml.etree import ElementTree
    from xml.parsers import expat
    root = Path(request['root']).resolve()
    files = list(islice((path for path in iter_files(root)
                         if not path.relative_to(root).as_posix().lower().startswith('.avg-translator/')), 20001))
    partial = len(files) > 20000
    files = [path.relative_to(root).as_posix() for path in files[:20000]]
    profiles = []
    candidates = [path for path in files if path.lower().endswith('.exe.le.config')]
    partial |= len(candidates) > 8
    for relative in candidates[:8]:
        executable = relative[:-len('.le.config')]
        record = dict(path=relative, executable=executable, executableExists=safe(root, executable).is_file(),
                      location=None, runAsAdmin=None, runWithSuspend=None, error=None)
        try:
            with safe(root, relative).open('rb') as stream:
                data = stream.read(32769)
            if len(data) > 32768:
                raise ValueError('转区配置超过 32 KB，请单独检查。')
            # Expat rejects DTDs before ElementTree can resolve internal entities.
            parser = expat.ParserCreate()
            def reject_dtd(*_):
                raise ValueError('转区配置不允许 DTD 或实体声明。')
            parser.StartDoctypeDeclHandler = reject_dtd
            parser.ExternalEntityRefHandler = reject_dtd
            parser.Parse(data, True)
            document = ElementTree.fromstring(data)
            profile = document.find('./Profiles/Profile') if document.tag == 'LEConfig' else None
            if profile is None:
                raise ValueError('未找到 Locale Emulator 配置项。')
            def value(name):
                node = profile.find(name)
                return ''.join(node.itertext()).strip() if node is not None else None
            def flag(name):
                text = value(name)
                return text.lower() == 'true' if text is not None and text.lower() in ('true', 'false') else None
            record.update(location=value('Location'), runAsAdmin=flag('RunAsAdmin'), runWithSuspend=flag('RunWithSuspend'))
        except (OSError, ValueError, ElementTree.ParseError, expat.ExpatError) as error:
            record['error'] = str(error)
        profiles.append(record)
    code_page = system_ansi_code_page()
    fallback = 932 if any(profile['executableExists'] and profile['location'] == 'ja-JP' for profile in profiles) else code_page
    order = lambda path: (path.count('/'), path)
    scripts = sorted((path for path in files if Path(path).suffix.lower() in ('.cmd', '.bat', '.ps1')), key=order)
    logs = sorted((path for path in files if Path(path).name.lower() in ('errorlog.txt', 'error.log', 'error.txt')), key=order)
    partial |= len(scripts) > 4 or len(logs) > 2
    return dict(systemAnsiCodePage=code_page, localeProfiles=profiles,
                launchScripts=[launch_text_sample(root, path, fallback) for path in scripts[:4]],
                errorLogs=[launch_text_sample(root, path, fallback, tail=True) for path in logs[:2]], partial=partial)


def decode_reference_html(request):
    source = Path(request['source'])
    if source.stat().st_size > 8 * 1024 * 1024:
        raise ValueError('资料响应超过 8 MiB。')
    content = source.read_bytes().decode('utf-8', errors='replace')
    content = re.sub(r'<(script|style)\b[^>]*>[\s\S]*?</\1>', '', content, flags=re.I)
    content = re.sub(r'<[^>]+>', ' ', content)

    def entity(match):
        value = match.group(1)
        if value.startswith('#'):
            hexadecimal = value[1:2] in ('x', 'X')
            digits = (value[2:] if hexadecimal else value[1:]).lstrip('0') or '0'
            if len(digits) > (6 if hexadecimal else 7):
                return match.group(0)
            number = int(digits, 16 if hexadecimal else 10)
            return chr(number) if 0 <= number <= 0x10ffff and not 0xd800 <= number <= 0xdfff else match.group(0)
        number = 39 if value == 'apos' else name2codepoint.get(value)
        return chr(number) if number is not None else match.group(0)

    content = re.sub(r'&(#(?:[xX][0-9a-fA-F]+|[0-9]+)|[a-zA-Z][a-zA-Z0-9]+);', entity, content)
    content = re.sub(r'[ \t]{2,}', ' ', content)
    Path(request['output']).write_text(content, encoding='utf-8', newline='')
    return dict(characters=len(content.encode('utf-16-le')) // 2)


def execute(request):
    operation = request['operation']
    if operation == 'list_files': return list_files(request)
    if operation == 'archive_component': return archive_component(request)
    if operation == 'inspect_game_launch': return inspect_game_launch(request)
    if operation == 'decode_reference_html': return decode_reference_html(request)
    if operation == 'builtin_sources': return builtin_sources(request)
    if operation == 'read_file': return read_file(request)
    if operation == 'search_text': return search_text(request)
    if operation == 'library': return library(request)
    if operation == 'search_engine_references': return search_engine_references(request)
    if operation == 'inspect_engine_reference': return inspect_engine_reference(request)
    if operation == 'prepare_siglus': return prepare_siglus(request)
    if operation == 'prepare_siglus_zip': return prepare_siglus_zip(request)
    if operation == 'run': return run(request)
    if operation == 'run_tool': return run_tool(request)
    if operation == 'syntax':
        for path in request['files']:
            ast.parse(Path(path).read_text(encoding='utf-8-sig'), filename=path)
        return dict(passed=True)
    raise ValueError('未知辅助操作：' + operation)


def main():
    try:
        result = execute(json.load(sys.stdin))
        print(json.dumps(dict(success=True, result=result), ensure_ascii=False))
    except Exception as error:
        print(json.dumps(dict(success=False, error=str(error), errorType=type(error).__name__, traceback=traceback.format_exc()[-4000:]), ensure_ascii=False))


if __name__ == '__main__':
    main()
