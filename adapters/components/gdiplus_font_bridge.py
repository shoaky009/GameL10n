"""Prepare a bounded x86 GDI+ font bridge without running or modifying a game.

Only selected ordinary imports are redirected; existing IAT RVAs, code and
relocations remain intact. The bridge and this helper are copied together by
copy_adapter_components. JSON stdin supports inspect and prepare operations.
"""
import hashlib
import json
import ntpath
from pathlib import Path, PureWindowsPath
import struct
import sys


DLL = "GameL10n.FontBridge.dll"
CONFIG = "GameL10n.FontBridge.ini"
LICENSE = "GameL10n.FontBridge.LICENSE"
FUNCTIONS = ("GdipCreateFontFromDC", "GdipCreateFontFromLogfontA", "GdipCreateFontFromLogfontW")
FAMILY_QUERY = "GdipGetFamily"
MAX_BYTES = 64 * 1024 * 1024


def align(value, boundary):
    return (value + boundary - 1) // boundary * boundary


class PE:
    def __init__(self, data):
        self.data = data
        if not 64 <= len(data) <= MAX_BYTES or data[:2] != b"MZ":
            raise ValueError("需要不超过 64 MiB 的 PE32 文件。")
        pe = self.u32(60)
        if self.span(pe, 4) != b"PE\0\0" or self.u16(pe + 4) != 0x14C:
            raise ValueError("字体桥接仅支持 Windows x86 PE32。")
        self.coff = pe + 4
        self.count = self.u16(pe + 6)
        self.flags = self.u16(pe + 22)
        self.optional = pe + 24
        optional_size = self.u16(pe + 20)
        if optional_size < 224 or self.u16(self.optional) != 0x10B:
            raise ValueError("PE32 可选头不完整。")
        self.span(self.optional, optional_size)
        if self.u32(self.optional + 92) < 16 or not 1 <= self.count < 96:
            raise ValueError("PE 目录或节数量不在支持范围。")
        self.section_table = self.optional + optional_size
        self.file_alignment = self.u32(self.optional + 36)
        self.section_alignment = self.u32(self.optional + 32)
        self.headers = self.u32(self.optional + 60)
        if (not 512 <= self.file_alignment <= 65536
                or self.file_alignment & (self.file_alignment - 1)
                or self.section_alignment < max(4096, self.file_alignment)
                or self.section_alignment & (self.section_alignment - 1)):
            raise ValueError("不支持该 PE 的特殊对齐方式。")
        self.sections = []
        for index in range(self.count):
            pos = self.section_table + index * 40
            self.span(pos, 40)
            virtual_size, rva, raw_size, offset = struct.unpack_from("<4I", data, pos + 8)
            if rva % self.section_alignment or (raw_size and offset % self.file_alignment):
                raise ValueError("PE 节地址未对齐。")
            if rva + max(virtual_size, raw_size) > 0xFFFFFFFF:
                raise ValueError("PE 节地址溢出。")
            if raw_size:
                self.span(offset, raw_size)
            self.sections.append((rva, virtual_size, raw_size, offset))
        raw_sections = sorted((off, off + size) for _, _, size, off in self.sections if size)
        virtual_sections = sorted((rva, rva + max(size, raw)) for rva, size, raw, _ in self.sections)
        for intervals in (raw_sections, virtual_sections):
            if any(a[1] > b[0] for a, b in zip(intervals, intervals[1:])):
                raise ValueError("PE 节发生重叠。")
        if (not raw_sections or self.headers > raw_sections[0][0]
                or self.section_table + self.count * 40 > self.headers):
            raise ValueError("PE 节头范围无效。")
        self.span(0, self.headers)

    def span(self, offset, length):
        if offset < 0 or length < 0 or offset + length > len(self.data):
            raise ValueError("PE 字段越界。")
        return memoryview(self.data)[offset:offset + length]

    def u16(self, offset):
        return struct.unpack("<H", self.span(offset, 2))[0]

    def u32(self, offset):
        return struct.unpack("<I", self.span(offset, 4))[0]

    def offset(self, rva, length=1):
        if 0 <= rva and rva + length <= self.headers:
            return rva
        for start, _, size, off in self.sections:
            if start <= rva and rva + length <= start + size:
                return off + rva - start
        raise ValueError("PE RVA 没有对应的文件数据。")

    def directory(self, index):
        return struct.unpack("<II", self.span(self.optional + 96 + index * 8, 8))

    def ascii(self, rva):
        result = bytearray()
        for index in range(512):
            value = self.data[self.offset(rva + index)]
            if value == 0:
                return result.decode("ascii")
            if not 32 <= value < 127:
                raise ValueError("PE 导入名称不是 ASCII。")
            result.append(value)
        raise ValueError("PE 名称超过读取上限。")

    def imports(self):
        rva, size = self.directory(1)
        if not rva or size < 20:
            raise ValueError("没有可用的普通导入表。")
        descriptors = []
        total = 0
        for index in range(min(size // 20, 256)):
            fields = struct.unpack("<5I", self.span(self.offset(rva + index * 20, 20), 20))
            if not any(fields):
                return descriptors
            lookup, timestamp, forwarder, name_rva, iat = fields
            if not name_rva or not iat or (not lookup and timestamp):
                raise ValueError("导入表缺少名称或仍绑定且没有原始名称表。")
            name = self.ascii(name_rva)
            entries = []
            for slot in range(8192):
                value = self.u32(self.offset((lookup or iat) + slot * 4, 4))
                self.offset(iat + slot * 4, 4)
                if not value:
                    break
                symbol = ("#" + str(value & 0xFFFF) if value & 0x80000000 else self.ascii(value + 2))
                entries.append((value, symbol, iat + slot * 4))
                total += 1
                if total > 8192:
                    raise ValueError("导入函数数量超过上限。")
            else:
                raise ValueError("导入函数表没有结束符。")
            descriptors.append(dict(fields=fields, library=name, entries=entries))
        raise ValueError("导入描述符没有结束符。")


def _functions(functions):
    if (not isinstance(functions, list) or not functions
            or any(name not in FUNCTIONS for name in functions) or len(set(functions)) != len(functions)):
        raise ValueError("functions 必须显式选择不重复的 GDI+ 字体创建入口。")
    return set(functions)


def inspect(data):
    pe = PE(data)
    imports = pe.imports()
    return dict(architecture="x86", sha256=hashlib.sha256(data).hexdigest(),
                constructors=[dict(function=symbol, iatRva=iat)
                              for module in imports if module["library"].lower() == "gdiplus.dll"
                              for _, symbol, iat in module["entries"] if symbol in FUNCTIONS],
                alreadyBridged=any(module["library"].lower() == DLL.lower() for module in imports),
                familyQueryImported=any(symbol == FAMILY_QUERY for module in imports
                                        if module["library"].lower() == "gdiplus.dll"
                                        for _, symbol, _ in module["entries"]),
                delayImportsPresent=bool(pe.directory(13)[0]), runtimeTested=False,
                scope="仅普通导入表线索；确认实际调用、字体匹配与探针结果后选择入口。动态加载和延迟导入不重定向。")


def patch_imports(data, functions):
    """Redirect selected ordinary imports; return patched bytes and IAT evidence."""
    selected = _functions(functions)
    pe = PE(data)
    if any(pe.directory(4)) or any(pe.directory(14)):
        raise ValueError("不修改带签名或 CLR 目录的程序。")
    imports = pe.imports()
    if any(module["library"].lower() == DLL.lower() for module in imports):
        raise ValueError("输入已接入字体桥接，请使用制作输入中的原始程序。")
    # Private-family lookup must return an owned reference to callers that use
    # the normal GDI+ FontFamily destruction contract. Include its companion
    # entry when imported; installed/system font families still pass through.
    if any(symbol == FAMILY_QUERY for module in imports
           if module["library"].lower() == "gdiplus.dll"
           for _, symbol, _ in module["entries"]):
        selected.add(FAMILY_QUERY)
    # Split descriptors into contiguous IAT runs. Code keeps calling its original
    # IAT slots; only the loader's choice of DLL changes for selected functions.
    runs, redirected = [], []
    for module in imports:
        entries = module["entries"]
        flags = [module["library"].lower() == "gdiplus.dll" and symbol in selected
                 for _, symbol, _ in entries]
        if not any(flags):
            runs.append((module, None, False))
            continue
        begin = 0
        for end in range(1, len(entries) + 1):
            if end == len(entries) or flags[end] != flags[begin]:
                runs.append((module, entries[begin:end], flags[begin]))
                if flags[begin]:
                    redirected.extend(dict(function=symbol, iatRva=iat) for _, symbol, iat in entries[begin:end])
                begin = end
    if {entry["function"] for entry in redirected} != selected:
        raise ValueError("所选入口不在普通 GDI+ 导入表中，不能猜测或改写动态调用。")
    new_header = pe.section_table + pe.count * 40
    if new_header + 40 > pe.headers or any(pe.span(new_header, 40)):
        raise ValueError("PE 没有可用节头空间，不能安全加入字体桥接导入。")
    new_rva = align(max(rva + max(size, raw) for rva, size, raw, _ in pe.sections), pe.section_alignment)
    descriptor_size = (len(runs) + 1) * 20
    payload = bytearray(descriptor_size)
    bridge_name = new_rva + len(payload)
    payload.extend(DLL.encode("ascii") + b"\0")
    for index, (module, entries, bridged) in enumerate(runs):
        fields = list(module["fields"])
        fields[1] = 0  # Resolve from names instead of accepting stale bound addresses.
        if entries is not None:
            payload.extend(b"\0" * (-len(payload) % 4))
            fields[0] = new_rva + len(payload)
            fields[3] = bridge_name if bridged else fields[3]
            fields[4] = entries[0][2]
            payload.extend(struct.pack("<" + "I" * (len(entries) + 1),
                                       *[value for value, _, _ in entries], 0))
        struct.pack_into("<5I", payload, index * 20, *fields)
    raw_size = align(len(payload), pe.file_alignment)
    raw_offset = align(len(data), pe.file_alignment)
    image_size = align(new_rva + len(payload), pe.section_alignment)
    initialized = pe.u32(pe.optional + 8) + raw_size
    if max(image_size, initialized) > 0xFFFFFFFF or raw_offset + raw_size > MAX_BYTES:
        raise ValueError("补丁 PE 超出大小范围。")
    output = bytearray(data)
    output.extend(b"\0" * (raw_offset - len(output)))
    output.extend(payload)
    output.extend(b"\0" * (raw_size - len(payload)))
    struct.pack_into("<8s6I2HI", output, new_header, b".glfont", len(payload), new_rva,
                     raw_size, raw_offset, 0, 0, 0, 0, 0x40000040)
    struct.pack_into("<H", output, pe.coff + 2, pe.count + 1)
    struct.pack_into("<I", output, pe.optional + 8, initialized)
    struct.pack_into("<I", output, pe.optional + 56, image_size)
    struct.pack_into("<II", output, pe.optional + 104, new_rva, descriptor_size)
    struct.pack_into("<II", output, pe.optional + 96 + 11 * 8, 0, 0)
    struct.pack_into("<I", output, pe.optional + 64, 0)
    # Ordinary user-mode images do not require a checksum, but keeping it correct
    # also makes the structural result straightforward to inspect.
    checksum = 0
    for offset in range(0, len(output), 2):
        checksum += output[offset] | ((output[offset + 1] if offset + 1 < len(output) else 0) << 8)
        checksum = (checksum & 0xFFFF) + (checksum >> 16)
    checksum = (checksum & 0xFFFF) + (checksum >> 16)
    struct.pack_into("<I", output, pe.optional + 64, checksum + len(output))
    def bindings(modules):
        items = [(iat, module["library"].lower(), symbol) for module in modules
                 for _, symbol, iat in module["entries"]]
        if len({item[0] for item in items}) != len(items):
            raise ValueError("原导入地址槽重叠，不能重定向。")
        return sorted(items)
    expected = [(iat, DLL.lower() if library == "gdiplus.dll" and symbol in selected else library, symbol)
                for iat, library, symbol in bindings(imports)]
    if bindings(PE(output).imports()) != expected or output[pe.headers:len(data)] != data[pe.headers:]:
        raise ValueError("字体桥接改变了原有函数地址或节内容。")
    return bytes(output), redirected


def _relative(value):
    if not isinstance(value, str) or not value or "\0" in value:
        raise ValueError("需要安全的相对文件路径。")
    parts = value.replace("\\", "/").split("/")
    if PureWindowsPath(value).drive or any(part in ("", ".", "..")
                                          or any(ord(ch) < 32 or ch in ':<>"|?*' for ch in part)
                                          or ntpath.isreserved(part)
                                          or part.endswith((".", " ")) for part in parts):
        raise ValueError("文件路径不能越界或使用设备路径。")
    return Path(*parts)


def _face(value, optional=False):
    if optional and value is None:
        return ""
    if (not isinstance(value, str) or not value or value != value.strip()
            or len(value.encode("utf-16-le")) > 62 or any(ch in value for ch in '\0\r\n[]="')):
        raise ValueError("字体名必须为 1..31 个 UTF-16 单元，不能含 INI 控制字符。")
    return value


def _read(path):
    with Path(path).open("rb") as source:
        data = source.read(MAX_BYTES + 1)
    if len(data) > MAX_BYTES:
        raise ValueError("文件超过 64 MiB。")
    return data


def prepare(request):
    source = Path(request["input"]).resolve(strict=True)
    data = _read(source)
    if hashlib.sha256(data).hexdigest() != request.get("expectedSha256"):
        raise ValueError("输入与已检查的程序不一致，请核对制作输入。")
    relative = _relative(request["relativePath"])
    if relative.name.casefold() != source.name.casefold():
        raise ValueError("桥接输出必须保留原程序名。")
    mode = request.get("mode", "fallback")
    if mode not in ("fallback", "replace-matching"):
        raise ValueError("mode 必须为 fallback 或 replace-matching。")
    face = _face(request["fontFace"])
    match_face = _face(request.get("matchFace"), optional=True)
    font = Path(request["fontFile"]).resolve(strict=True)
    _relative(font.name)
    if font.suffix.lower() not in (".ttf", ".ttc", ".otf") or any(ch in font.name for ch in '\r\n[]="'):
        raise ValueError("需要有效的 TTF/TTC/OTF 文件名。")
    runtime = Path(__file__).resolve().parent
    bridge = _read(runtime / DLL)
    if not PE(bridge).flags & 0x2000:
        raise ValueError("字体桥接组件不是 x86 DLL。")
    patched, redirected = patch_imports(data, request["functions"])
    config = b"\xff\xfe" + (f"[Font]\r\nFile={font.name}\r\nFace={face}\r\nMatchFace={match_face}\r\nMode={mode}\r\n").encode("utf-16-le")
    outputs = [(relative, patched), (relative.parent / DLL, bridge),
               (relative.parent / CONFIG, config), (relative.parent / font.name, _read(font)),
               (relative.parent / LICENSE, _read(runtime / LICENSE))]
    root = Path(request["outputDirectory"]).resolve()
    if len({str(path).casefold() for path, _ in outputs}) != len(outputs):
        raise ValueError("补丁文件名冲突。")
    for path, content in outputs:
        dest = root / path
        resolved = dest.resolve()
        if (not resolved.is_relative_to(root) or resolved in (source, font, runtime / DLL, runtime / LICENSE)
                or dest.is_dir() or (dest.exists() and dest.read_bytes() != content)):
            raise ValueError(f"输出路径已有不同内容或与制作输入冲突：{path}")
    for path, content in outputs:
        dest = root / path
        if not dest.exists():
            dest.parent.mkdir(parents=True, exist_ok=True)
            with dest.open("xb") as target:
                target.write(content)
    return dict(success=True, runtimeTested=False, mode=mode, redirected=redirected,
                inputSha256=hashlib.sha256(data).hexdigest(),
                files=[dict(path=path.as_posix(), size=len(content), sha256=hashlib.sha256(content).hexdigest())
                       for path, content in outputs],
                scope="已校验导入地址与原节字节保持一致；字体创建、显示与布局仍需独立探针及游戏试译确认。")


def execute(request):
    if request.get("operation", "inspect") == "inspect":
        return dict(success=True, **inspect(_read(request["input"])))
    if request.get("operation") == "prepare":
        return prepare(request)
    raise ValueError("operation 必须为 inspect 或 prepare。")


def main():
    try:
        data = sys.stdin.buffer.read(32769)
        if len(data) > 32768:
            raise ValueError("请求超过 32 KiB。")
        result = execute(json.loads(data.decode("utf-8-sig")))
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(json.dumps(dict(success=False, error=str(error)), ensure_ascii=True))
        return 1
    print(json.dumps(result, ensure_ascii=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
