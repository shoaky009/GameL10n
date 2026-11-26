"""System-font Chinese patches for the keyed QLIE ImoScripter layout."""
from dataclasses import dataclass
import json
from pathlib import Path
import re
import unicodedata
from xml.etree import ElementTree

from . import engine_archive as archive
from . import qlie_script, qlie_sfnt


CONFIG = "gamedata/system/config.txt"
TABLE = "system/font/fontdata.dat"
TEXT_TABLE = "system/font/fontdata.txt"
FONT_LIST_FILE = "avg/imoscripter_fontlist.txt"
SETTINGS_FILE = "avg/setting.txt"
DEFAULT_CONFIG_FILE = "script/config/default.txt"
MAPPING = "qlie-l10n/glyphs.json"
LAUNCH = "qlie-l10n/launch.xml"
LAUNCHER = "QlieLauncher.exe"
ASSET_CATALOG = "qlie-systemfont-assets"
FONT_INPUTS = {
    "Sans": "NotoSansSC",
    "Serif": "NotoSerifSC",
}
TOOLS = Path(__file__).resolve().parent
FIRST_SLOT, LAST_SLOT = 0xE000, 0xE757
SLOTS = range(FIRST_SLOT, LAST_SLOT + 1)
PRIVATE_CHARS = re.compile(r"[\ue000-\ue757]")
FONT_LIST = re.compile(r"^Font\[(\d+)\]\.List\[(\d+)\]=([^\r\n]*)", re.I | re.M)
EXTERNAL_FONT = re.compile(r"^ExtFont\[\d+\]=([^\r\n]*)", re.I | re.M)
FONT_NAME = re.compile(r"^Font\[(\d+)\]\.Name=([^\r\n]*)", re.I | re.M)
DEFAULT_FONT_NAME = re.compile(r"^FontName=([^\r\n]*)", re.I | re.M)
FONT_COMMAND = re.compile(r'^[ \t]*(?:\\scp,[^\s,]+[ \t]+)?\\fn2,[^,\r\n]+,"([^"\r\n]*)"', re.I | re.M)
SYSTEM_SELECTORS = frozenset({"system", "system_fix"})
RESERVED_SELECTORS = frozenset({"system", "system_fix", "system_pro", "system_ext", "base"})
SETUP_MEMBER = "misc/misc.s"
SETUP_ROUTINE = re.compile(r"^@@!Misc_FontSetup[ \t]*\r?\n(.*?)(?=^[ \t]*\\ret\b|^@@|\Z)", re.I | re.M | re.S)
TABLE_LOAD = re.compile(
    r'\s*\\var,table,t,p\s+\\scp,t[ \t]+\\l,"Avg\\ImoScripter_FontList\.txt"\s+'
    r'\\scp,p[ \t]+\\l,"Avg\\Setting\.txt"\s+\\scp,t[ \t]+\\comb,p[ \t]*\r?\n(?:[ \t]*\r?\n)*', re.I)
STARTUP_CALL = re.compile(r'^[ \t]*\\sub,@@!Misc_FontSetup,"Misc\\Misc\.s"(?:,\d+)?[ \t]*\r?$', re.I | re.M)
RESET_GATE = re.compile(r'[ \t]*\\if,ParamInt\[0\]==1\\then[ \t]*\r?\n', re.I)
TABLE_FACE = re.compile(r'([ \t]*)\\scp,t[ \t]+\\\?v,"font\[0\]\.name","([^"\r\n]+)"[ \t]*\r?\n'
                        r'[ \t]*\\scp,cfg[ \t]+\\fn2,0,ResultStr\[0\][ \t]*\r?\n', re.I)
CONFIG_SCOPE = re.compile(r"^[ \t]*\\scp,cfg[ \t]*(\r?\n)", re.I | re.M)
INHERIT_BASE = re.compile(r"^([ \t]*)\\\?fn[ \t]+\\fn2,\w+,ResultStr\[0\][ \t]*\r?$", re.I | re.M)
SET_BASE = re.compile(r'\\fn2,0,"([^"\r\n]+)"', re.I)
FACE_TEST = re.compile(r'\\if,ResultStr\[0\]=="[^"]+"(?: \|\| ResultStr\[0\]=="[^"]+")*\\then')
INHERITED = {"", "base", "外字"}
SYSTEM_FACES = {
    "ms gothic": "Sans",
    "ms pgothic": "Sans",
    "ms ui gothic": "Sans",
    "ms ゴシック": "Sans",
    "ms pゴシック": "Sans",
    "ms mincho": "Serif",
    "ms pmincho": "Serif",
    "ms 明朝": "Serif",
    "ms p明朝": "Serif",
}
CONFIG_BYTES = 64 << 10
FONT_BYTES = 64 << 20
UNICODE_FACE = "Microsoft YaHei"


def unicode_runtime(executable):
    """Recognize the IMOSURUME runtime with native UTF-16 scenario loading."""
    if executable.stat().st_size > 64 << 20:
        return False
    image = archive.read_file(executable, 64 << 20)
    marker = "VS_VERSION_INFO\0".encode("utf-16-le")
    at = image.rfind(marker)
    if at < 6:
        return False
    start = at - 6
    record = image[start:start + int.from_bytes(image[start:start + 2], "little")]
    fixed = bytes.fromhex("bd04effe 00000100 02000200 00000000")
    return (record[40:56] == fixed
            and "ProductName\0\0IMOSURUME\0".encode("utf-16-le") in record)


class NeedFontAssets(ValueError):
    def __init__(self, files):
        super().__init__("QLIE 中文回填需要已登记的字体素材")
        self.files = files


def read_map(value):
    """The patch manifest stores Unicode values as integers."""
    if not isinstance(value, dict) or len(value) > len(SLOTS):
        raise ValueError("QLIE 字形映射需要双字节码位到 Unicode 整数的映射")
    mapping = {}
    for slot, code in value.items():
        if (not isinstance(slot, str) or not slot.isascii() or not slot.isdecimal() or len(slot) != 5
                or int(slot) not in SLOTS or type(code) is not int or not 0 < code <= 0x10FFFF
                or 0xD800 <= code <= 0xDFFF or code in SLOTS):
            raise ValueError("QLIE 补丁包含非法字形码位")
        mapping[int(slot)] = code
    if len(set(mapping.values())) != len(mapping):
        raise ValueError("QLIE 字形映射含重复字符")
    return mapping


def map_bytes(mapping):
    return json.dumps(mapping, sort_keys=True, separators=(",", ":")).encode("ascii")


def old_mapping(root, files):
    if MAPPING not in files:
        return {}
    mapping = read_map(json.loads(archive.read_file(root / MAPPING, CONFIG_BYTES)))
    if not mapping:
        raise ValueError("QLIE 补丁缺少有效字形映射")
    return mapping


def family(style, mapping):
    return f"Qlie{style}{archive.digest(map_bytes(mapping))[:10]}"


@dataclass(frozen=True)
class Document:
    path: str
    member: str | None
    text: str
    encoding: str
    bom: bytes

    @classmethod
    def read(cls, path, member, raw):
        return cls(path, member, *qlie_script.decode(raw, unicode_scripts=True))

    def encode(self, text):
        return self.bom + text.encode(self.encoding)


def replace_value(text, matches, group, replacement):
    parts, cursor = [], 0
    for match in matches:
        value = replacement(match)
        if value is None or value == match[group]:
            continue
        start, end = match.span(group)
        parts.extend((text[cursor:start], value))
        cursor = end
    return "".join([*parts, text[cursor:]]) if parts else text


def base_selection(script):
    """Find the cfg block that copies the player-selected base face into dialogue slots."""
    routine = SETUP_ROUTINE.search(script)
    if not routine:
        return None
    copy = INHERIT_BASE.search(script, routine.start(1), routine.end(1))
    if not copy:
        return None
    scopes = list(CONFIG_SCOPE.finditer(script, routine.start(1), copy.start()))
    if not scopes:
        raise ValueError("QLIE 对白继承了基础字体，但未确认其配置作用域")
    scope = scopes[-1]
    return scope.end(), copy.start(), copy[1], scope[1]


def base_faces(commands):
    lines = [line.strip() for line in commands.splitlines() if line.strip()]
    if not lines:
        return set()
    if (len(lines) == 6 and lines[0] == r"\?fn" and FACE_TEST.fullmatch(lines[1])
            and lines[3] == r"\else" and lines[5] == r"\end"):
        lines = [lines[2], lines[4]]
    elif len(lines) != 1:
        raise ValueError("QLIE 基础字体继承前含未识别的设置指令")
    assignments = [SET_BASE.fullmatch(line) for line in lines]
    if not all(assignments):
        raise ValueError("QLIE 对白的基础字体无法从设置脚本确认")
    return {match[1] for match in assignments}


@dataclass
class Resources:
    config: Document
    table: Document
    overrides: list[Document]
    bitmap_names: frozenset[str] = frozenset({"外字"})

    def is_bitmap_slot(self, slot, selector):
        return ((slot, selector) == (6, "外字")
                or (bool(selector) and selector.casefold() not in RESERVED_SELECTORS
                    and selector.casefold() in self.bitmap_names))

    def is_ignored_face(self, face):
        folded = face.casefold()
        return (face in INHERITED or folded in self.bitmap_names
                or (folded.endswith(".of") and folded[:-3] in self.bitmap_names))

    def slots(self):
        entries = FONT_LIST.findall(self.table.text)
        lists = {(int(slot), int(index)): value for slot, index, value in entries}
        primary = {slot: value for (slot, index), value in lists.items() if index == 0}
        extra = {(slot, index): value for (slot, index), value in lists.items() if index and value}
        if (len(lists) != len(entries) or set(primary) != set(range(8))
                or any(slot not in primary for slot, index in lists)
                or any(EXTERNAL_FONT.findall(self.table.text))):
            raise ValueError("QLIE 字体表需要八个字体列表和空的外部字库")
        entries = FONT_NAME.findall(self.config.text)
        names = {int(slot): name for slot, name in entries}
        if len(names) != len(entries):
            raise ValueError("QLIE 外置配置含重复字体槽")
        if not extra and set(names) == set(primary):
            if any(value not in SYSTEM_SELECTORS for slot, value in primary.items()
                   if not self.is_bitmap_slot(slot, value)):
                raise ValueError("QLIE 单层字体表含未知选择方式")
            return {slot: (selector, names[slot]) for slot, selector in primary.items()
                    if not self.is_bitmap_slot(slot, selector)}
        if (extra != {(0, 1): "system_ext"} or primary[0] != "system"
                or any(primary[i] != "base" for i in range(1, 8)) or set(names) != set(range(16))):
            raise ValueError("QLIE 多层字体需要 system/system_ext、继承 base 的七个槽及启动字体列表")
        has_base_face = False
        for doc in self.overrides:
            base = [face for slot, face in FONT_NAME.findall(doc.text) if int(slot) == 0]
            is_list = doc.member.replace("\\", "/").casefold() == FONT_LIST_FILE
            if (is_list or base) and (len(base) != 1 or base[0] in INHERITED):
                raise ValueError(f"QLIE 启动配置需要明确的 0 号基础字体：{doc.member}")
            has_base_face |= is_list
        if not has_base_face:
            raise ValueError("QLIE 多层字体缺少启动字体列表")
        return {slot: ("system", name) for slot, name in names.items()}

    def selected_faces(self):
        faces = {name for doc in self.overrides for _, name in FONT_NAME.findall(doc.text)}
        for doc in self.overrides:
            if doc.member and doc.member.replace("\\", "/").casefold() == DEFAULT_CONFIG_FILE:
                faces.update(DEFAULT_FONT_NAME.findall(doc.text))
        return {face for face in faces if not self.is_ignored_face(face)}


def table_selection(script):
    routine = SETUP_ROUTINE.search(script)
    if not routine or not (load := TABLE_LOAD.match(script, routine.start(1), routine.end(1))):
        return None
    start = load.end()
    gate = RESET_GATE.match(script, start)
    face = TABLE_FACE.match(script, gate.end() if gate else start)
    if not face:
        raise ValueError("QLIE 启动字体表已加载，但无法确认 0 号字体的赋值位置")
    return gate, face


def startup_table(scripts, *, patched):
    starts, setups = [], []
    for member, script in scripts:
        if member == "startup.s":
            starts.append(bool(STARTUP_CALL.search(script)))
        elif member == SETUP_MEMBER:
            selection = table_selection(script)
            if patched and selection and selection[0]:
                raise ValueError("QLIE 普通启动时会跳过基础字体赋值，导致对白继续使用玩家配置中的字体")
            setups.append(selection)
    if starts and all(starts) and setups and all(setups):
        return {face[2] for _, face in setups}
    return set()


@dataclass(frozen=True)
class Layout:
    resources: Resources
    slots: dict[int, tuple[str, str]]
    faces: dict[str, str]

    @classmethod
    def read(cls, resources, mapping):
        slots = resources.slots()
        by_face = ({family(style, mapping) + " Regular": style for style in FONT_INPUTS}
                   if mapping else SYSTEM_FACES)
        if len(slots) == 16 and not mapping:
            by_face = {**by_face, "tahoma": "Sans", UNICODE_FACE.casefold(): "Sans"}
        faces = {}
        for original in {face for _, face in slots.values()} | resources.selected_faces():
            lookup = original if mapping else unicodedata.normalize("NFKC", original).casefold()
            if lookup not in by_face:
                raise ValueError(f"QLIE 字体槽使用了未识别的字体 {original[:80]}")
            faces[original] = by_face[lookup]
        return cls(resources, slots, faces)

    def resource_edits(self, faces, table_startup=False):
        docs = self.resources.overrides if table_startup else [self.resources.config, *self.resources.overrides]
        for doc in docs:
            matches = (m for m in FONT_NAME.finditer(doc.text) if doc.member or int(m[1]) in self.slots)
            text = replace_value(doc.text, matches, 2, lambda m: faces.get(m[2]))
            if doc.member and doc.member.replace("\\", "/").casefold() == DEFAULT_CONFIG_FILE:
                text = replace_value(text, DEFAULT_FONT_NAME.finditer(text), 1, lambda m: faces.get(m[1]))
            if text != doc.text:
                yield doc, doc.encode(text)
        doc = self.resources.table
        text = replace_value(doc.text, FONT_LIST.finditer(doc.text), 3,
                             lambda m: "system" if m[3] == "system_fix" else None)
        if text != doc.text:
            yield doc, doc.encode(text)

    def script_faces(self, script, faces):
        def face(match):
            original = match[1]
            if self.resources.is_ignored_face(original):
                return None
            if original not in faces:
                raise ValueError(f"QLIE 脚本使用了未识别的字体 {original[:80]}")
            return faces[original]

        return replace_value(script, FONT_COMMAND.finditer(script), 1, face)

    def restore_base(self, script, faces, *, native_unicode=False):
        if native_unicode and (table := table_selection(script)):
            gate, face = table
            if gate:
                # A normal boot has no reset argument. Apply only the font name on both paths.
                indent = gate[0][:len(gate[0]) - len(gate[0].lstrip())]
                default = faces[self.slots[0][1]]
                lines = face[0].replace(f'"{face[2]}"', f'"{default}"').splitlines(keepends=True)
                assign = "".join(indent + line.lstrip(" \t") for line in lines)
                return script[:gate.start()] + assign + gate[0] + script[face.end():]
            return script
        selection = base_selection(script)
        if selection is None:
            return script
        start, end, indent, newline = selection
        base_faces(script[start:end])
        default = faces[self.slots[0][1]]
        lines = [f'\\fn2,0,"{default}"']
        alternate = next((name for name in faces.values() if name != default), None)
        if alternate:
            # The game can restore an MS font from the player's settings, including after the title screen.
            # Every branch selects a registered face before the existing slot inheritance runs.
            stock = (["ＭＳ ゴシック", "MS Gothic", "ＭＳ Ｐゴシック", "MS PGothic", "MS UI Gothic"]
                     if alternate.startswith("QlieSans") else ["ＭＳ 明朝", "MS Mincho", "ＭＳ Ｐ明朝", "MS PMincho"])
            originals = sorted(name for name, target in faces.items() if target == alternate)
            names = dict.fromkeys([*stock, *originals, alternate])
            condition = " || ".join(f'ResultStr[0]=="{name}"' for name in names)
            lines = [r"\?fn", f"\\if,{condition}\\then", f'\t\\fn2,0,"{alternate}"',
                     r"\else", f'\t\\fn2,0,"{default}"', r"\end"]
        return script[:start] + "".join(indent + line + newline for line in lines) + script[end:]

    def runtime_files(self):
        files = [MAPPING, LAUNCH, LAUNCHER]
        for style in sorted(set(self.faces.values())):
            files.extend((f"qlie-l10n/{style}.ttf", f"qlie-l10n/{FONT_INPUTS[style]}-OFL.txt"))
        return files


def glyphs_needed(characters, mapping):
    needed = set()
    for char in characters - set(map(chr, mapping.values())):
        try:
            if char.encode("cp932").decode("cp932") == char:
                continue
        except UnicodeEncodeError:
            pass
        needed.add(char)
    return needed


def plan(layout, mapping, needed, occupied, characters):
    for doc in (layout.resources.config, layout.resources.table, *layout.resources.overrides):
        occupied.update(PRIVATE_CHARS.findall(doc.text))
    # A rebuilt source already contains its own aliases. Keep those assignments
    # and exclude any other occupied positions from the CP932 user-defined area.
    reserved = set(map(ord, occupied)).difference(mapping)
    reserved.update(code for code in map(ord, characters) if code in SLOTS)
    if reserved.intersection(mapping):
        raise ValueError("译文与已有 QLIE 字形码位冲突")
    codes = sorted(map(ord, needed))
    if any(0xd800 <= code <= 0xdfff for code in codes):
        raise ValueError("QLIE 译文含不完整的 Unicode 字符")
    available = [code for code in SLOTS if code not in reserved and code not in mapping]
    if len(codes) > len(available):
        raise ValueError("QLIE 可用的双字节字形码位不足")
    return mapping | dict(zip(available, codes))


def resolve_assets(request, layout):
    if not (TOOLS / LAUNCHER).is_file():
        raise ValueError("QLIE 中文回填缺少启动器，请准备系统字体模板组件")
    names = [f"{FONT_INPUTS[style]}-{suffix}" for style in sorted(set(layout.faces.values()))
             for suffix in ("VF.ttf", "OFL.txt")]
    assets = (request.get("toolPaths") or {}).get("assets", {})
    if any(name not in assets for name in names):
        raise NeedFontAssets(names)
    return {name: Path(assets[name]) for name in names}


def make_runtime(layout, executable, mapping, assets):
    launch = ElementTree.Element("launch", executable=executable)
    artifacts = {MAPPING: map_bytes(mapping), LAUNCHER: archive.read_file(TOOLS / LAUNCHER, 8 << 20)}
    for style in sorted(set(layout.faces.values())):
        filename = f"{style}.ttf"
        relative = f"qlie-l10n/{filename}"
        name = family(style, mapping)
        license_name = f"{FONT_INPUTS[style]}-OFL.txt"
        source = archive.read_file(assets[f"{FONT_INPUTS[style]}-VF.ttf"], FONT_BYTES)
        artifacts[f"qlie-l10n/{license_name}"] = archive.read_file(assets[license_name], CONFIG_BYTES)
        artifacts[relative] = qlie_sfnt.derive(source, name, mapping)
        ElementTree.SubElement(launch, "font", file=relative, face=name + " Regular")
    artifacts[LAUNCH] = ElementTree.tostring(launch, encoding="utf-8", xml_declaration=True)
    return artifacts
