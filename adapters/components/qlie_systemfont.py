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
FONT_COMMAND = re.compile(r'^[ \t]*(?:\\scp,[^\s,]+[ \t]+)?\\fn2,[^,\r\n]+,"([^"\r\n]*)"', re.I | re.M)
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


@dataclass
class Resources:
    config: Document
    table: Document
    overrides: list[Document]

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
            if any(value not in {"system", "system_fix"} for slot, value in primary.items()
                   if (slot, value) != (6, "外字")):
                raise ValueError("QLIE 单层字体表含未知选择方式")
            return {slot: (selector, names[slot]) for slot, selector in primary.items()
                    if (slot, selector) != (6, "外字")}
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
        return {name for doc in self.overrides for _, name in FONT_NAME.findall(doc.text)} - INHERITED


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
            by_face = {**by_face, "tahoma": "Sans"}
        faces = {}
        for original in {face for _, face in slots.values()} | resources.selected_faces():
            lookup = original if mapping else unicodedata.normalize("NFKC", original).casefold()
            if lookup not in by_face:
                raise ValueError(f"QLIE 字体槽使用了未识别的字体 {original[:80]}")
            faces[original] = by_face[lookup]
        return cls(resources, slots, faces)

    def resource_edits(self, faces):
        for doc in [self.resources.config, *self.resources.overrides]:
            matches = (m for m in FONT_NAME.finditer(doc.text) if doc.member or int(m[1]) in self.slots)
            text = replace_value(doc.text, matches, 2, lambda m: faces.get(m[2]))
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
            if original in INHERITED:
                return None
            if original not in faces:
                raise ValueError(f"QLIE 脚本使用了未识别的字体 {original[:80]}")
            return faces[original]

        return replace_value(script, FONT_COMMAND.finditer(script), 1, face)

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
