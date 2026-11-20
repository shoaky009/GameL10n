"""Keyed QLIE FilePack 3.0 and ImoScripter FormatType=1."""
from contextlib import ExitStack
from dataclasses import dataclass
import json
from pathlib import Path
import shutil
import struct
import sys

from python.archives import qlie, qlie_keys, qlie_writer
from python.engines import qlie_imos

from . import engine_archive as archive
from . import qlie_samples as support
from . import qlie_systemfont as font
from .gamel10n_host import write_response


TEMPLATE_ID = "qlie-pack3-imos"
WARNING = "支持已识别的 ImoScripter 脚本字段与严格 CP932 回填，中文编码、系统文本和游戏显示需另行验收"
FONT_WARNING = "已识别 QLIE 系统字体布局，脚本以外的文字及游戏内中文显示需要单独确认"
LAUNCH_WARNING = "通过 QlieLauncher.exe 启动以加载补丁字体，保留原游戏的日语转区配置"
MAX_SCRIPT = 8 << 20
MAX_TOTAL = 64 << 20
MAX_EXE = 64 << 20
MAX_INDEX = 2 << 20
MAX_TAIL = 4 << 20
RANGE_BYTES = 1 << 20


class NeedRanges(Exception):
    def __init__(self, ranges):
        self.ranges = ranges


class ProbeFile:
    """Expose only the host-provided header and ranges at their original offsets."""
    def __init__(self, path, length, fragments):
        self.path, self.length = path, length
        self.fragments = sorted(fragments)
        self.position = 0

    def __len__(self):
        return self.length

    def __getitem__(self, part):
        return self.at(part.start, part.stop - part.start)

    def seek(self, offset, whence=0):
        self.position = (0, self.position, self.length)[whence] + offset
        return self.position

    def tell(self):
        return self.position

    def read(self, length):
        result = self.at(self.position, length)
        self.position += length
        return result

    def at(self, offset, length):
        end = offset + length
        if not 0 <= offset <= end <= self.length:
            raise ValueError("QLIE 探测片段超出资源范围")
        parts, missing = [], []

        def need(start, stop):
            for at in range(start, stop, RANGE_BYTES):
                missing.append(dict(path=self.path, offset=at, length=min(RANGE_BYTES, stop - at)))

        for start, data in self.fragments:
            stop = min(start + len(data), end)
            if stop <= offset or start >= end:
                continue
            if start > offset:
                need(offset, start)
                offset = start
            parts.append(data[offset - start:stop - start])
            offset = stop
        need(offset, end)
        if missing:
            raise NeedRanges(missing)
        return b"".join(parts)


def probe_files(root, request, files):
    if "probeFiles" not in request:
        return {}
    metadata = {info["path"]: info for info in request["probeFiles"]}
    fragments = {}
    for part in request.get("probeRanges", []):
        length = part["length"]
        data = archive.read_file(part["dataPath"], RANGE_BYTES)
        if len(data) != length:
            raise ValueError("QLIE 探测片段长度发生变化")
        fragments.setdefault(part["path"], []).append((part["offset"], data))
    return {
        path: ProbeFile(path, metadata[path]["length"],
                        [(0, archive.read_file(root / path, 64 << 10)), *fragments.get(path, [])])
        for path in files
    }


def read_index(stream):
    if isinstance(stream, ProbeFile):
        footer = stream.at(len(stream) - 28, 28)
        if footer[:16] != b"FilePackVer3.0\0\0":
            raise ValueError("封包版本需要 FilePackVer3.0")
        start, = struct.unpack_from("<Q", footer, 20)
        size = len(stream) - start
        if not 0x41C < size <= MAX_INDEX + MAX_TAIL + 28:
            raise ValueError("QLIE 封包索引超出读取范围")
        stream.at(start, size)
    index = qlie.read_index(stream, max_entries=16_000, max_index_bytes=MAX_INDEX)
    if index.file_size - index.index_offset > MAX_INDEX + MAX_TAIL + 28:
        raise ValueError("QLIE 封包索引超出读取范围")
    archive.validate_names(index.entries)
    return index


def script_members(index):
    return [entry for entry in index.entries if entry.name.lower().endswith(".s")]


def pending_ranges(request, missing):
    ranges = request.get("probeRanges", []) + missing
    if len(ranges) > 64 or sum(part["length"] for part in ranges) > 8 << 20:
        raise ValueError("QLIE 封包与 EXE 的探测片段超出读取范围")
    return dict(probeReads=missing)


@dataclass
class Installation:
    files: list[str]
    executable: str
    archives: dict
    game_key: bytes
    external_key: bytes
    internal_keys: dict[str, bytes]
    fingerprints: dict[str, str]
    index_bytes: int
    font_layout: font.Layout | None
    font_diagnostic: str
    glyphs: dict[int, int]

    def read(self, path, entry, limit):
        return support.read_member(self.archives[path], entry, self.internal_keys[path], self.game_key, limit)


def system_font(root, request, files, indices):
    config = next((path for path in files if path.casefold() == font.CONFIG), None)
    if config is None:
        return None, "缺少外置的 QLIE 系统字体配置"
    tables = [(path, entry) for path, (_, index) in indices.items() for entry in index.entries
              if entry.name.replace("\\", "/").casefold() == font.TABLE]
    if len(tables) != 1:
        return None, "需要唯一的 QLIE 系统字体表"
    path, entry = tables[0]
    _, index = indices[path]
    key = index.entries[0]
    if (max(entry.size, entry.unpacked_size) > font.CONFIG_BYTES
            or max(key.size, key.unpacked_size) > support.KEY_BYTES
            or any(info["path"] == config and info["length"] > font.CONFIG_BYTES
                   for info in request.get("probeFiles", []))):
        return None, "QLIE 系统字体表或伴随配置超出读取范围"
    try:
        raw = archive.read_file(root / config, font.CONFIG_BYTES)
    except ValueError as error:
        return None, str(error)
    return (config, raw, path, entry), ""


def locate(root, request, files, stack):
    packs = [path for path in files if path.lower().startswith("gamedata/")
             and path.count("/") == 1 and path.lower().endswith(".pack")]
    executables = [path for path in files if "/" not in path and path.lower().endswith(".exe")]
    keys = [path for path in files if path.lower() == "dll/key.fkey"]
    if len(keys) != 1 or not 1 <= len(executables) <= 8 or not 1 <= len(packs) <= 16:
        raise ValueError("需要同一启动目录内的 GameData 封包、DLL/key.fkey 和游戏 EXE")
    key_file = keys[0]
    views = probe_files(root, request, [*packs, *executables, key_file])
    missing, executable_keys, indices = [], [], {}
    for path in executables:
        try:
            image = views[path] if views else archive.read_file(root / path, MAX_EXE)
            if len(image) <= MAX_EXE:
                executable_keys.append((path, qlie_keys.game_key_from_pe(image)))
        except NeedRanges as error:
            missing.extend(error.ranges)
        except ValueError:
            continue
    for path in packs:
        stream = views[path] if views else stack.enter_context(archive.checked_path(root / path).open("rb"))
        try:
            indices[path] = stream, read_index(stream)
        except NeedRanges as error:
            missing.extend(error.ranges)
    if missing:
        return pending_ranges(request, missing)
    if len(executable_keys) != 1:
        raise ValueError("需要唯一带 IconKeyImage 的游戏 EXE")
    exe, game_key = executable_keys[0]
    profile = {path: list(support.profile_members(index)) for path, (_, index) in indices.items()}
    scripts = {path: script_members(index) for path, (_, index) in indices.items()}
    selected = {path: entry for path, entry in indices.items() if scripts[path] or profile[path]}
    font_source, font_diagnostic = system_font(root, request, files, indices)
    if font_source and font_source[2] not in selected:
        path = font_source[2]
        try:
            qlie_writer.hash13_tail(*indices[path], max_bytes=MAX_TAIL)
        except ValueError as error:
            font_source, font_diagnostic = None, str(error)
    if not any(scripts.values()):
        raise ValueError("QLIE 封包没有 ImoScripter 脚本成员")
    if sum(map(len, profile.values())) > 8:
        raise ValueError("ImoScripter 配置副本超出读取范围")
    fingerprints = {}
    for path, (stream, index) in indices.items():
        if path in selected:
            qlie_writer.hash13_tail(stream, index, max_bytes=MAX_TAIL)
        fingerprints[path] = archive.hash_range(stream, index.index_offset, index.file_size - index.index_offset)
    probe_targets = selected.copy()
    if font_source:
        path = font_source[2]
        probe_targets[path] = indices[path]
    for path, (stream, index) in probe_targets.items():
        if not isinstance(stream, ProbeFile):
            continue
        reads = [(index.entries[0], support.KEY_BYTES), *((entry, 64 << 10) for entry, _ in profile[path])]
        if font_source and font_source[2] == path:
            reads.append((font_source[3], font.CONFIG_BYTES))
        for entry, limit in reads:
            if entry.size > limit or entry.unpacked_size > limit:
                raise ValueError("QLIE 密钥或脚本配置超出读取范围")
            try:
                stream.at(entry.offset, entry.size)
            except NeedRanges as error:
                missing.extend(error.ranges)
    if views and not 256 <= len(views[key_file]) <= support.KEY_BYTES:
        raise ValueError("QLIE 外部密钥长度不符合封包格式")
    try:
        external_key = (views[key_file].at(0, len(views[key_file])) if views
                        else archive.read_file(root / key_file, support.KEY_BYTES))
    except NeedRanges as error:
        missing.extend(error.ranges)
    if missing:
        return pending_ranges(request, missing)
    if not 256 <= len(external_key) <= support.KEY_BYTES:
        raise ValueError("QLIE 外部密钥长度不符合封包格式")
    local_keys = {path: support.internal_key(volume, external_key, game_key)
                  for path, volume in selected.items()}
    if any(len(key) < 256 for key in local_keys.values()):
        raise ValueError("QLIE 封包内部密钥长度不足")
    companions = [path for path in files if path.lower() == exe.lower() + ".le.config"]
    glyphs = font.old_mapping(root, files)
    layout = None
    if font_source:
        config, raw, path, entry = font_source
        try:
            volume = indices[path]
            key = local_keys.get(path) or support.internal_key(volume, external_key, game_key)
            if len(key) < 256:
                raise ValueError("QLIE 字体表所在卷的内部密钥长度不足")
            table = support.read_member(volume, entry, key, game_key, font.CONFIG_BYTES)
            layout = font.Layout.read(config, raw, path, entry.name, table, glyphs)
            local_keys[path] = key
            selected[path] = volume
            companions.append(config)
        except ValueError as error:
            font_diagnostic = str(error)
    if glyphs:
        if not layout or not set(layout.runtime_files()) <= set(files):
            raise ValueError("QLIE 中文补丁的字体配置、字形映射或伴随文件不完整")
        companions.extend(layout.runtime_files())
    required = sorted([*selected, key_file, exe, *companions], key=str.casefold)
    index_bytes = sum(index.file_size - index.index_offset for _, index in indices.values())
    install = Installation(required, exe, selected, game_key, external_key, local_keys, fingerprints,
                           index_bytes, layout, font_diagnostic, glyphs)
    support.script_profile(selected, install.read)
    return install


def text_units(relative, member, row_number, record, row):
    raw_name = record.speaker.raw if record.speaker else ""
    speaker = raw_name.split("＠", 1)[0] or None
    kinds = {"choice": "choice", "save-title": "ui"}
    for field, text in row.items():
        if not text.strip() or field == "name" and raw_name.startswith("$"):
            continue
        locator = json.dumps([member, row_number, field], ensure_ascii=False, separators=(",", ":"))
        kind = "name" if field == "name" else kinds.get(record.parts[0].kind, "dialogue")
        yield field, dict(id=archive.digest(f"{relative}\0{locator}".encode("utf-8")),
                          resourcePath=relative, locator=locator, context=member, sourceText=text,
                          speaker=None if kind == "name" else speaker, kind=kind,
                          protectedTokens=["\n"] if field == "message" and len(record.parts) > 1 else None)


def read_scripts(install):
    decoded = 0
    for path, (_, index) in install.archives.items():
        for entry in script_members(index):
            try:
                raw = install.read(path, entry, min(MAX_SCRIPT, MAX_TOTAL - decoded))
                decoded += len(raw)
                script = raw.decode("cp932")
                if script.encode("cp932") != raw:
                    raise ValueError("脚本包含无法保持原字节的 CP932 字符")
            except ValueError as error:
                raise ValueError(f"{path}/{entry.name}：{error}") from error
            yield path, entry.name, script


def scripts(install, sources, translations=None, mapping=None):
    units, changes, used = [], {}, set()
    changed = 0
    mapping = install.glyphs if mapping is None else mapping
    reverse = {code: slot for slot, code in mapping.items()}
    faces = ({face: font.family(style, mapping) + " Regular" for face, style in install.font_layout.faces.items()}
             if mapping != install.glyphs else {})
    for path, member, script in sources:
        try:
            rows, modified = [], False
            for number, record in enumerate(qlie_imos.scan(script)):
                row = record.row()
                decoded = {field: value.translate(install.glyphs) for field, value in row.items()} if install.glyphs else row
                for field, unit in text_units(path, member, number, record, decoded):
                    if translations is None:
                        units.append(unit)
                    elif unit["id"] in translations:
                        used.add(unit["id"])
                        replacement = translations[unit["id"]]
                        if replacement != decoded[field]:
                            encoded = replacement.translate(reverse)
                            if encoded.translate(mapping) != replacement:
                                raise ValueError("译文与 QLIE 字形码位冲突")
                            row[field] = encoded
                            modified = True
                rows.append(row)
            patched = qlie_imos.patch(script, rows) if modified else script
            if faces:
                patched = install.font_layout.script_faces(patched, faces)
            if patched != script:
                data = patched.encode("cp932")
                if data.decode("cp932") != patched:
                    raise ValueError("译文在 CP932 回读时发生变化")
                changed += len(data)
                if len(data) > MAX_SCRIPT or changed > MAX_TOTAL:
                    raise ValueError("QLIE 回填脚本超出大小范围")
                changes.setdefault(path, {})[member] = data
        except UnicodeEncodeError as error:
            raise ValueError(f"{path}/{member} 的译文需要已确认的中文编码与字体方案") from error
        except ValueError as error:
            raise ValueError(f"{path}/{member}：{error}") from error
    if translations is not None and translations.keys() - used:
        raise ValueError("译文 ID 不属于本次选择的 QLIE 脚本")
    return units, changes


def check_volume(install, path, output, changes):
    source, before = install.archives[path]
    after = read_index(output)
    if (qlie_writer.hash13_tail(source, before, max_bytes=MAX_TAIL)
            != qlie_writer.hash13_tail(output, after, max_bytes=MAX_TAIL)
            or [entry.raw_name for entry in before.entries] != [entry.raw_name for entry in after.entries]):
        raise ValueError("QLIE 封包的成员顺序或查找表发生变化")
    for old, new in zip(before.entries, after.entries, strict=True):
        if old.encryption != new.encryption:
            raise ValueError("QLIE 封包成员的加密方式发生变化")
        if old.name in changes:
            actual = support.read_member((output, after), new, install.internal_keys[path],
                                         install.game_key, MAX_SCRIPT)
            if new.packed or actual != changes[old.name]:
                raise ValueError(f"QLIE 封包成员回读与目标译文不一致：{old.name}")
        elif (old.size, old.unpacked_size, old.packed, old.checksum) != (
                new.size, new.unpacked_size, new.packed, new.checksum
        ) or archive.hash_range(source, old.offset, old.size) != archive.hash_range(output, new.offset, new.size):
            raise ValueError(f"QLIE 封包未翻译成员发生变化：{old.name}")


def execute(request):
    root = Path(request["sourceDirectory"])
    operation = request["operation"]
    files = request.get("resourceFiles") or []
    if (not isinstance(files, list) or len(files) > 40
            or any(archive.member_name(path) != path for path in files)
            or len({path.casefold() for path in files}) != len(files)):
        raise ValueError("QLIE 需要无重复的当前游戏相对资源路径")
    with ExitStack() as stack:
        try:
            install = locate(root, request, files, stack)
        except ValueError as error:
            if operation != "probe":
                raise
            return dict(probe=None, diagnostic=str(error)[:400])
        if isinstance(install, dict):
            return install
        warnings = [FONT_WARNING if install.font_layout else WARNING]
        if operation == "probe":
            entries = [entry for _, index in install.archives.values() for entry in script_members(index)]
            return dict(indexFingerprints=install.fingerprints, indexBytes=install.index_bytes,
                        mediaOnlyArchives=len(install.fingerprints) - len(install.archives),
                        scriptCount=len(entries), scriptBytes=sum(entry.unpacked_size for entry in entries),
                        systemFont=dict(matched=install.font_layout is not None, diagnostic=install.font_diagnostic),
                        probe=dict(engineId="qlie", adapterId=f"template.{TEMPLATE_ID}", adapterVersion="1",
                                   description="QLIE 封包内的 ImoScripter FormatType=1 脚本",
                                   requiredFiles=install.files, originalExecutable=install.executable,
                                   warnings=warnings))
        if set(files) != set(install.files):
            raise ValueError("请选择 QLIE 探测返回的必需资源")
        if operation == "extract":
            units, _ = scripts(install, read_scripts(install))
            return dict(extraction=dict(units=units, warnings=warnings))
        if operation not in ("build", "validate"):
            raise ValueError("未知 QLIE 适配器操作")
        translations = request.get("translations")
        if request.get("translationsFile"):
            with Path(request["translationsFile"]).open(encoding="utf-8-sig") as stream:
                translations = json.load(stream)
        if translations is None:
            translations = {}
        if not isinstance(translations, dict) or any(not isinstance(value, str) for value in translations.values()):
            raise ValueError("QLIE 译文必须为文本 ID 到字符串的映射")
        return rebuild(root, request, install, translations, warnings)


def rebuild(root, request, install, translations, warnings):
    output = Path(request["outputDirectory"])
    archive.separate(archive.checked_path(root), archive.checked_path(output))
    sources = read_scripts(install)
    mapping = install.glyphs
    layout = install.font_layout
    characters = {char for text in translations.values() for char in text}
    needed = font.glyphs_needed(characters, mapping)
    assets = {}
    if needed:
        if not layout:
            raise ValueError(f"QLIE 译文需要已确认的中文编码与字体方案（{install.font_diagnostic}）")
        assets = font.resolve_assets(request, layout)
        sources = list(sources)
        occupied = {char for _, _, script in sources for char in font.PRIVATE_CHARS.findall(script)}
        mapping = font.plan(layout, mapping, needed, occupied, characters)
    _, changes = scripts(install, sources, translations, mapping)
    runtime = {}
    if mapping != install.glyphs:
        if font.LAUNCHER.casefold() == install.executable.casefold():
            raise ValueError("QLIE 补丁启动器与原游戏 EXE 同名")
        config, table = layout.resource_edits(mapping)
        changes.setdefault(layout.table_volume, {})[layout.table_member] = table
        runtime = font.make_runtime(layout, install.executable, mapping, assets)
        runtime[layout.config_path] = config
    files = sorted(set(install.files) | set(runtime), key=str.casefold)
    operation = request["operation"]
    if operation == "validate":
        output_files = request.get("outputFiles", install.files)
        if (not isinstance(output_files, list) or any(not isinstance(path, str) for path in output_files)
                or sorted(output_files) != sorted(files)):
            raise ValueError("QLIE 验收的输出清单与所需资源不一致")
    for path in files:
        target = archive.checked_path(output / path)
        if path in runtime:
            if operation == "build":
                target.parent.mkdir(parents=True, exist_ok=True)
                with target.open("xb") as stream:
                    stream.write(runtime[path])
            elif archive.read_file(target, len(runtime[path])) != runtime[path]:
                raise ValueError(f"QLIE 字体或启动资源回读不一致（{path}）")
        else:
            source = archive.checked_path(root / path)
            edits = changes.get(path)
            if operation == "build":
                target.parent.mkdir(parents=True, exist_ok=True)
                with source.open("rb") as original, target.open("x+b") as result:
                    if edits:
                        qlie_writer.rebuild(original, result, edits, mode="keyed",
                                            key_file=install.external_key, game_key=install.game_key)
                    else:
                        shutil.copyfileobj(original, result, 1 << 20)
            else:
                with target.open("rb") as result:
                    if edits:
                        check_volume(install, path, result, edits)
                    else:
                        with source.open("rb") as original:
                            length = original.seek(0, 2)
                            if result.seek(0, 2) != length or (
                                archive.hash_range(original, 0, length) != archive.hash_range(result, 0, length)
                            ):
                                raise ValueError(f"QLIE 未翻译资源发生变化：{path}")
    if operation == "build":
        build = dict(files=files, mode="replace", warnings=warnings)
        if mapping:
            build.update(launchExecutable=font.LAUNCHER, warnings=[*warnings, LAUNCH_WARNING])
        return dict(build=build)
    return dict(validation=dict(passed=True, issues=[], runtimeStatus="not-tested"))


def main():
    request = json.load(sys.stdin)
    try:
        response = dict(protocolVersion=1, success=True, **execute(request))
    except font.NeedFontAssets as error:
        response = dict(protocolVersion=1, success=False, errorKind="dependency", error=str(error),
                        requiredAssets=dict(catalog=font.ASSET_CATALOG, files=error.files))
    except (ValueError, OSError) as error:
        kind = "io" if isinstance(error, OSError) else "incompatible"
        response = dict(protocolVersion=1, success=False, errorKind=kind, error=str(error)[:1000])
    write_response(request, response)
    return 0 if response["success"] else 1
