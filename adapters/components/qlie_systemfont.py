"""System-font Chinese patches for the keyed QLIE ImoScripter layout."""
from dataclasses import dataclass
import json
from pathlib import Path
import re
import unicodedata
from xml.etree import ElementTree

from . import engine_archive as archive
from . import qlie_sfnt


CONFIG = "gamedata/system/config.txt"
TABLE = "system/font/fontdata.dat"
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
FONT_COMMAND = re.compile(r'^[ \t]*\\fn2,[^,\r\n]+,"([^"\r\n]+)"', re.M)
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


def cp932(raw):
    text = raw.decode("cp932")
    if text.encode("cp932") != raw:
        raise ValueError("QLIE 字体配置含无法保持原字节的 CP932 字符")
    return text


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


@dataclass(frozen=True)
class Layout:
    config_path: str
    config: str
    table_volume: str
    table_member: str
    table: str
    styles: dict[int, str]
    faces: dict[str, str]

    @classmethod
    def read(cls, config_path, config_raw, volume, member, table_raw, mapping):
        config, table = cp932(config_raw), cp932(table_raw)
        entries = FONT_LIST.findall(table)
        lists = {(int(slot), int(index)): value for slot, index, value in entries}
        primary = {slot: value for (slot, index), value in lists.items() if index == 0}
        if (len(lists) != len(entries) or set(primary) != set(range(8))
                or any(slot not in primary or index != 0 and value for (slot, index), value in lists.items())
                or any(value for value in EXTERNAL_FONT.findall(table))):
            raise ValueError("QLIE 字体表需要 0 到 7 号单层字体列表")
        entries = FONT_NAME.findall(config)
        names = {int(slot): face for slot, face in entries}
        if set(names) != set(primary) or len(names) != len(entries):
            raise ValueError("QLIE 外置配置缺少唯一的八个字体名称")
        supported = ({"system"} if mapping else {"system", "system_fix"})
        by_face = ({family(style, mapping) + " Regular": style for style in FONT_INPUTS}
                   if mapping else SYSTEM_FACES)
        styles, faces = {}, {}
        for slot, selector in primary.items():
            if slot == 6 and selector == "外字":
                continue  # This slot uses its own bitmap, outside the system font glyph aliases.
            original = names[slot]
            lookup = original if mapping else unicodedata.normalize("NFKC", original).casefold()
            if selector not in supported or lookup not in by_face:
                raise ValueError("QLIE 字体槽需要已识别的 MS Gothic 或 MS Mincho 系统布局")
            styles[slot] = faces[original] = by_face[lookup]
        return cls(config_path, config, volume, member, table, styles, faces)

    def resource_edits(self, mapping):
        names = {slot: family(style, mapping) + " Regular" for slot, style in self.styles.items()}
        config = replace_value(self.config, FONT_NAME.finditer(self.config), 2,
                               lambda match: names.get(int(match[1])))
        table = replace_value(self.table, FONT_LIST.finditer(self.table), 3,
                              lambda match: "system" if int(match[2]) == 0 and int(match[1]) in names else None)
        return config.encode("cp932"), table.encode("cp932")

    def script_faces(self, script, faces):
        def face(match):
            original = match[1]
            if original == "外字":
                return None
            if original not in faces:
                raise ValueError(f"QLIE 脚本使用了未识别的字体 {original[:80]}")
            return faces[original]

        return replace_value(script, FONT_COMMAND.finditer(script), 1, face)

    def runtime_files(self):
        files = [MAPPING, LAUNCH, LAUNCHER]
        for style in sorted(set(self.styles.values())):
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
    occupied.update(PRIVATE_CHARS.findall(layout.config))
    occupied.update(PRIVATE_CHARS.findall(layout.table))
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
    names = [f"{FONT_INPUTS[style]}-{suffix}" for style in sorted(set(layout.styles.values()))
             for suffix in ("VF.ttf", "OFL.txt")]
    assets = (request.get("toolPaths") or {}).get("assets", {})
    if any(name not in assets for name in names):
        raise NeedFontAssets(names)
    return {name: Path(assets[name]) for name in names}


def make_runtime(layout, executable, mapping, assets):
    launch = ElementTree.Element("launch", executable=executable)
    artifacts = {MAPPING: map_bytes(mapping), LAUNCHER: archive.read_file(TOOLS / LAUNCHER, 8 << 20)}
    for style in sorted(set(layout.styles.values())):
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
