"""ImoScripter replacement strings and indexed OF glyph sheets."""
import base64
from dataclasses import dataclass
import re
import struct

from . import qlie_systemfont as font
from .gamel10n_host import component


TABLE = "avg/imoscripter_reptextlist.txt"
ATLAS = "system/font/外字.of"
BASE = "avg/imoscripter_base.s"
MESSAGE = "avg/imoscripter_custom.s"
CHOICES = "avg/avgsystem_selectobject.s"
TOKEN = re.compile(r"<GL([0-9A-F]{4})>")
RULE = re.compile(r"(<GL[0-9A-F]{4}>)=\[f,6\](.)\[f\]")
NATIVE_RULE = re.compile(r"[^=\r\n]+=\[f,6\].\[f\]")
LOAD = re.compile(r'\\scp,Imo_RepTextTable[ \t]+\\l,"ImoScripter_RepTextList\.txt"[ \t]+\\sort', re.I)
SLOT = re.compile(r'\\cal,n=6\s+\\scp,cfg\s+\\fn2,n,"外字"', re.I)
DIALOGUE = re.compile(r'(?m)^([ \t]*)\\scp,Imo_RepTextTable[ \t]+\\brep,Imo_FormatText[ \t]*\r?\n'
                      r'[ \t]*\\cal,Imo_FormatText=ResultStr\[0\]([ \t]*\r?\n)', re.I)
OPTION = re.compile(r'(?m)^([ \t]*)\\scp,SelectButtonList[ \t]+\\a,ParamStr\[i\]([ \t]*\r?\n)', re.I)
NAME_EXPANSION = r"\scp,Imo_RepTextTable \brep,Imo_FormatName"
CHOICE_EXPANSION = r"\scp,Imo_RepTextTable \brep,ParamStr[i]"
NAME_ASSIGN = r"\cal,Imo_FormatName=ResultStr[0]"
CHOICE_ADD = r"\scp,SelectButtonList \a,ResultStr[0]"
INDEX, WIDTHS, ADVANCES, PALETTE, HEADER, PIXELS = 64, 0x8040, 0xc040, 0x10040, 0x10440, 0x1044a
LIMIT = 16 << 20
CELL, COLUMNS = 64, 16
PAGE = (CELL * COLUMNS) ** 2


def token(char):
    return f"<GL{ord(char):04X}>"


def code_index(char):
    value = char.encode("cp932")
    if len(value) != 2 or not 0x81 <= value[0] <= 0x9f:
        raise ValueError("QLIE 外字需要可索引的标准双字节字符")
    return int.from_bytes(value, "big") - 0x8000


def tile_rows(tile):
    y, x = divmod(tile, COLUMNS)
    start = PIXELS + y * CELL * CELL * COLUMNS + x * CELL
    return range(start, start + PAGE // COLUMNS, CELL * COLUMNS)


class Atlas:
    def __init__(self, raw):
        self.data = bytearray(raw)
        if len(raw) < PIXELS or raw[:5] != b"\x04" + "外字".encode("cp932"):
            raise ValueError("QLIE 外字位图名称或头部无效")
        w, h, pages, cols, rows = struct.unpack_from("<5H", raw, HEADER)
        if (w, h, cols, rows) != (CELL, CELL, 4, 4) or len(raw) != PIXELS + pages * PAGE or not pages:
            raise ValueError("QLIE 外字需要 64 像素方格及完整的索引色字形页")
        self.tiles = struct.unpack_from("<16384H", raw, INDEX)
        if max(self.tiles) >= pages * COLUMNS ** 2:
            raise ValueError("QLIE 外字索引超出位图页")
        colors = [raw[k:k + 4] for k in range(PALETTE, HEADER, 4)]
        if colors[0][3] != 0 or bytes.fromhex("ffffffff") not in colors:
            raise ValueError("QLIE 外字调色板缺少透明背景或白色文字")
        self.ink = colors.index(bytes.fromhex("ffffffff"))

    def draw(self, char, tile, mask):
        index = code_index(char)
        struct.pack_into("<H", self.data, INDEX + 2 * index, tile)
        fullwidth = code_index("あ")
        for at in (WIDTHS, ADVANCES):
            self.data[at + index] = self.data[at + fullwidth]
        while tile >= (len(self.data) - PIXELS) // (CELL ** 2):
            if len(self.data) + PAGE > LIMIT:
                raise ValueError("QLIE 外字位图超过可回包的容量")
            self.data.extend(bytes(PAGE))
        struct.pack_into("<H", self.data, HEADER + 4, (len(self.data) - PIXELS) // PAGE)
        for y, at in enumerate(tile_rows(tile)):
            row = (CELL - 1 - y) * 8
            bits = int.from_bytes(mask[row:row + 8], "big")
            self.data[at:at + CELL] = bytes(self.ink if bits & (1 << bit) else 0 for bit in range(63, -1, -1))

    def mask(self, char):
        index = code_index(char)
        tile = self.tiles[index]
        if not tile or any(self.data[at + index] != self.data[at + code_index("あ")] for at in (WIDTHS, ADVANCES)):
            raise ValueError(f"QLIE 外字「{char}」缺少全角字形")
        rows = []
        for at in tile_rows(tile):
            bits = 0
            for pixel in self.data[at:at + CELL]:
                bits = (bits << 1) | bool(self.data[PALETTE + pixel * 4 + 3])
            rows.append(bits.to_bytes(8, "big"))
        return b"".join(reversed(rows))


def rasterize(chars, paths):
    chars = sorted(chars)
    # FNL's fixed GDI rasterizer accepts bottom-up, DWORD-aligned monochrome cells.
    full_row = ((1 << 58) - 1 << 3).to_bytes(8, "big")
    reference = base64.b64encode(bytes(24) + full_row * 58 + bytes(24)).decode()
    for start in range(0, len(chars), 128):
        batch = chars[start:start + 128]
        specs = [dict(character=c, width=CELL, height=CELL, reference=reference) for c in batch]
        values = component("render_glyphs", dict(toolPaths=paths), glyphs=specs)["values"]
        for char, value in zip(batch, values, strict=True):
            mask = base64.b64decode(value, validate=True)
            if len(mask) != CELL * 8 or not any(mask):
                raise ValueError(f"QLIE 中文外字「{char}」没有有效位图")
            yield char, mask


def apply_routes(name, script):
    if name == MESSAGE:
        match = DIALOGUE.search(script)
        if not match:
            raise ValueError("QLIE 对白未经过外字替换表")
        indent, nl = match[1], match[2].lstrip(" \t")
        extra = indent + NAME_EXPANSION + nl + indent + NAME_ASSIGN + nl
        if script.startswith(extra, match.end()):
            return script
        return script[:match.end()] + extra + script[match.end():]
    if name == CHOICES:
        if match := OPTION.search(script):
            return OPTION.sub(lambda m: m[1] + CHOICE_EXPANSION + m[2] + m[1] + CHOICE_ADD + m[2], script, count=1)
        if not re.search(re.escape(CHOICE_EXPANSION) + r"[ \t]*\r?\n[ \t]*" + re.escape(CHOICE_ADD), script):
            raise ValueError("QLIE 选项缺少可以应用外字替换表的显示入口")
    return script


@dataclass
class BitmapFont:
    install: object
    members: dict
    table: font.Document
    rules: dict

    @classmethod
    def find(cls, install):
        resource = install.font_resources
        if not resource or ("6", "0", "外字") not in font.FONT_LIST.findall(resource.table.text):
            return None
        required = {ATLAS, TABLE, BASE, MESSAGE, CHOICES}
        members = {}
        for path, (_, index) in install.archives.items():
            for entry in index.entries:
                key = entry.name.replace("\\", "/").casefold()
                if key in required or key in ("startup.s", font.SETUP_MEMBER):
                    members.setdefault(key, []).append((path, entry))
        if not required <= members.keys():
            return None
        if len(members[TABLE]) != 1 or len(members[ATLAS]) != 1:
            raise ValueError("QLIE 外字替换表和位图需要唯一的来源卷")
        path, entry = members[TABLE][0]
        table = font.Document.read(path, entry.name, install.read(path, entry, 256 << 10))
        if not any(NATIVE_RULE.fullmatch(line) for line in table.text.splitlines()):
            return None
        rules = {}
        for line in table.text.splitlines():
            if not TOKEN.match(line):
                continue
            rule = RULE.fullmatch(line)
            if not rule or rule[1] in rules:
                raise ValueError("QLIE 中文外字替换表含无效或重复的字形规则")
            rules[rule[1]] = rule[2]
        if len(set(rules.values())) != len(rules):
            raise ValueError("QLIE 中文外字重复占用位图字符")
        return cls(install, members, table, rules)

    def read(self, key, limit):
        path, entry = self.members[key][0]
        return self.install.read(path, entry, limit)

    def routes(self, changes=None):
        # During build a display script can already contain translated fields.
        def document(path, entry):
            raw = changes.get(path, {}).get(entry.name) if changes is not None else None
            if raw is None:
                raw = self.install.read(path, entry, 1 << 20)
            return font.Document.read(path, entry.name, raw)

        if not all(LOAD.search(document(p, e).text) for p, e in self.members[BASE]):
            raise ValueError("QLIE 初始化未加载外字替换表")
        starts = self.members.get("startup.s", [])
        if not starts or not all(font.STARTUP_CALL.search(document(p, e).text) for p, e in starts):
            raise ValueError("QLIE 启动脚本缺少外字槽的字体初始化调用")
        setups = self.members.get(font.SETUP_MEMBER, [])
        if not setups or not all(
            any(SLOT.search(body) for body in font.SETUP_ROUTINE.findall(document(p, e).text))
            for p, e in setups
        ):
            raise ValueError("QLIE 初始化缺少第 6 号字体槽的外字选择")
        for key in (MESSAGE, CHOICES):
            for path, entry in self.members[key]:
                doc = document(path, entry)
                edited = apply_routes(key, doc.text)
                if edited != doc.text:
                    if changes is None:
                        raise ValueError(f"QLIE 外字替换尚未接入 {path}/{entry.name} 的显示入口")
                    changes.setdefault(path, {})[entry.name] = doc.encode(edited)

    def add_edits(self, changes, chars, paths):
        self.routes(changes)
        fresh = {c for c in chars if token(c) not in self.rules}
        if not fresh:
            return
        if any(ord(c) > 0xffff for c in fresh):
            raise ValueError("QLIE 中文外字当前只支持 BMP 字符")
        atlas = Atlas(self.read(ATLAS, LIMIT))
        # JIS level-one kanji are ordinary two-byte characters in the bitmap reader.
        available = (n.to_bytes(2, "big").decode("cp932") for n in range(0x889f, 0x9873)
                     if 0x40 <= n % 256 <= 0xfc and n % 256 != 0x7f and not atlas.tiles[n - 0x8000])
        lines = []
        tile = max(atlas.tiles)
        for char, mask in rasterize(fresh, paths):
            target = next(available, None)
            if not target:
                raise ValueError("QLIE 外字没有足够的空闲字符")
            tile += 1
            atlas.draw(target, tile, mask)
            lines.append(f"{token(char)}=[f,6]{target}[f]")
        path, entry = self.members[ATLAS][0]
        changes.setdefault(path, {})[entry.name] = bytes(atlas.data)
        text = self.table.text
        nl = "\r\n" if "\r\n" in text else "\n"
        if text and not text.endswith("\n"):
            text += nl
        changes.setdefault(self.table.path, {})[self.table.member] = self.table.encode(text + nl.join(lines) + nl)

    def decode(self, value):
        return TOKEN.sub(lambda m: chr(int(m[1], 16)) if m[0] in self.rules else m[0], value)
