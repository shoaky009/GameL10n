"""Compose a supported FilePack layout with the ImoScripter text profile."""
from contextlib import ExitStack
from dataclasses import dataclass
import json
from pathlib import Path
import shutil
import struct
import sys

from python.archives import qlie_keys

from . import engine_archive as archive
from . import qlie_archive as pack
from . import qlie_samples as support
from . import qlie_script as script_format
from . import qlie_systemfont as font
from .gamel10n_host import write_response


WARNING = "支持已识别的 ImoScripter 脚本字段与严格 CP932 回填，中文编码、系统文本和游戏显示需另行验收"
UNICODE_WARNING = "按脚本原编码回填已识别的 ImoScripter 字段，保留注音标签，系统文本和游戏内显示需另行验收"
FONT_WARNING = "已识别 QLIE 系统字体布局，脚本以外的文字及游戏内中文显示需要单独确认"
LAUNCH_WARNING = "通过 QlieLauncher.exe 启动以加载补丁字体，保留原游戏的日语转区配置"
MAX_SCRIPT = 8 << 20
MAX_TOTAL = 64 << 20
MAX_EXE = 64 << 20
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


def read_index(stream, layout):
    if isinstance(stream, ProbeFile):
        footer = stream.at(len(stream) - 28, 28)
        start, = struct.unpack_from("<Q", footer, 20)
        size = len(stream) - start
        if not 0x41C < size <= pack.MAX_INDEX_REGION:
            raise ValueError("QLIE 封包索引超出读取范围")
        stream.at(start, size)
    return pack.read_index(stream, layout)


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
    executable: str | None
    layout: pack.Pack30 | pack.Pack31
    archives: dict
    game_key: bytes | None
    external_key: bytes | None
    internal_keys: dict[str, bytes]
    fingerprints: dict[str, str]
    index_bytes: int
    font_layout: font.Layout | None
    font_diagnostic: str
    glyphs: dict[int, int]
    font_resources: font.Resources | None

    def read(self, path, entry, limit):
        return self.layout.read(*self.archives[path], entry, self.internal_keys[path], self.game_key, limit)


def system_font(root, request, files, indices):
    config = next((path for path in files if path.casefold() == font.CONFIG), None)
    if config is None:
        return None, "缺少外置的 QLIE 系统字体配置"
    members = {name: [] for name in (font.TABLE, font.TEXT_TABLE, font.FONT_LIST_FILE, font.SETTINGS_FILE)}
    for path, (_, index) in indices.items():
        for entry in index.entries:
            name = entry.name.replace("\\", "/").casefold()
            if name in members:
                members[name].append((path, entry))
    tables = members[font.TABLE] + members[font.TEXT_TABLE]
    if len(tables) != 1:
        return None, "需要唯一的 QLIE 系统字体表"
    documents = [*tables, *members[font.FONT_LIST_FILE], *members[font.SETTINGS_FILE]]
    if (len(documents) > 16 or any(max(e.size, e.unpacked_size) > font.CONFIG_BYTES for _, e in documents)
            or any(info["path"] == config and info["length"] > font.CONFIG_BYTES
                   for info in request.get("probeFiles", []))):
        return None, "QLIE 字体表或伴随配置超出读取范围"
    try:
        raw = archive.read_file(root / config, font.CONFIG_BYTES)
        for path in {path for path, _ in documents}:
            _, index = indices[path]
            key = index.entries[0]
            if max(key.size, key.unpacked_size) > pack.KEY_BYTES:
                raise ValueError("QLIE 字体表所在卷的密钥超出读取范围")
        return (font.Document.read(config, None, raw), documents), ""
    except ValueError as error:
        return None, str(error)


def locate(root, request, files, stack):
    packs = [path for path in files if path.lower().startswith("gamedata/")
             and path.count("/") == 1 and path.lower().endswith(".pack")]
    executables = [path for path in files if "/" not in path and path.lower().endswith(".exe")]
    keys = [path for path in files if path.lower() == "dll/key.fkey"]
    if not 1 <= len(packs) <= 16:
        raise ValueError("需要同一启动目录下 1 到 16 个 GameData 封包")
    views = probe_files(root, request, [*packs, *executables, *keys])
    streams = {path: views[path] if views else stack.enter_context(
        archive.checked_path(root / path).open("rb")) for path in packs}
    missing, versions, indices = [], set(), {}
    for stream in streams.values():
        try:
            versions.add(pack.version(stream))
        except NeedRanges as error:
            missing.extend(error.ranges)
    if missing:
        return pending_ranges(request, missing)
    layout = pack.LAYOUTS.get(next(iter(versions))) if len(versions) == 1 else None
    if layout is None:
        raise ValueError("当前启动目录的 FilePack 版本没有匹配的组合读写布局")
    expected = request.get("templateId")
    if expected and expected != layout.template:
        raise ValueError(f"所选 QLIE 模板需要不同的封包布局，当前为 FilePack {layout.version}")
    key_file = exe = game_key = external_key = None
    if layout.key_source == "external-and-executable":
        if len(keys) != 1 or not 1 <= len(executables) <= 8:
            raise ValueError("FilePack 3.0 需要同一启动目录下的 DLL/key.fkey 和游戏 EXE")
        key_file = keys[0]
        executable_keys = []
        for path in executables:
            try:
                image = views[path] if views else archive.read_file(root / path, MAX_EXE)
                if len(image) <= MAX_EXE:
                    executable_keys.append((path, qlie_keys.game_key_from_pe(image)))
            except NeedRanges as error:
                missing.extend(error.ranges)
            except ValueError:
                continue
        try:
            if views and not 256 <= len(views[key_file]) <= pack.KEY_BYTES:
                raise ValueError("QLIE 外部密钥长度不符合封包格式")
            external_key = (views[key_file].at(0, len(views[key_file])) if views
                            else archive.read_file(root / key_file, pack.KEY_BYTES))
        except NeedRanges as error:
            missing.extend(error.ranges)
        if not missing:
            if len(executable_keys) != 1 or not 256 <= len(external_key) <= pack.KEY_BYTES:
                raise ValueError("需要唯一带 IconKeyImage 的游戏 EXE 与有效的外部密钥")
            exe, game_key = executable_keys[0]
    for path, stream in streams.items():
        try:
            indices[path] = stream, read_index(stream, layout)
        except NeedRanges as error:
            missing.extend(error.ranges)
        except ValueError as error:
            raise ValueError(f"{path}：{error}") from error
    if missing:
        return pending_ranges(request, missing)
    profile = {path: list(support.profile_members(index, layout)) for path, (_, index) in indices.items()}
    scripts = {path: script_members(index) for path, (_, index) in indices.items()}
    selected = {path: entry for path, entry in indices.items() if scripts[path] or profile[path]}
    font_source, font_diagnostic = system_font(root, request, files, indices)
    if font_source and not exe:
        profiles = {path.casefold() for path in files if path.lower().endswith(".exe.le.config")}
        paired = [path for path in executables if path.casefold() != font.LAUNCHER.casefold()
                  and path.casefold() + ".le.config" in profiles]
        if len(paired) == 1:
            exe = paired[0]
        else:
            font_source, font_diagnostic = None, "需要唯一带 Locale Emulator 配置的原游戏 EXE"
    if not any(scripts.values()):
        raise ValueError("QLIE 封包没有 ImoScripter 脚本成员")
    if sum(map(len, profile.values())) > 8:
        raise ValueError("ImoScripter 配置副本超出读取范围")
    fingerprints = {}
    for path, (stream, index) in indices.items():
        if path in selected:
            layout.tail(stream, index)
        fingerprints[path] = archive.hash_range(stream, index.index_offset, index.file_size - index.index_offset)
    font_entries = {}
    if font_source:
        for path, entry in font_source[1]:
            font_entries.setdefault(path, []).append(entry)
    for path in sorted(selected.keys() | font_entries.keys()):
        stream, index = indices[path]
        if not isinstance(stream, ProbeFile):
            continue
        reads = [(index.entries[0], pack.KEY_BYTES), *((entry, font.CONFIG_BYTES) for entry, _ in profile[path])]
        known = {entry.name for entry, _ in reads}
        reads.extend((entry, font.CONFIG_BYTES) for entry in font_entries.get(path, []) if entry.name not in known)
        for entry, limit in reads:
            if entry.size > limit or entry.unpacked_size > limit:
                raise ValueError("QLIE 密钥或脚本配置超出读取范围")
            try:
                stream.at(entry.offset, entry.size)
            except NeedRanges as error:
                missing.extend(error.ranges)
    if missing:
        return pending_ranges(request, missing)
    local_keys = {path: layout.key(*volume, external_key, game_key)
                  for path, volume in selected.items()}
    if any(len(key) < 256 for key in local_keys.values()):
        raise ValueError("QLIE 封包内部密钥长度不足")
    companions = []
    glyphs = font.old_mapping(root, files)
    font_layout = None
    font_resources = None
    if font_source:
        config, entries = font_source
        try:
            docs = []
            for path, entry in entries:
                volume = indices[path]
                if path not in local_keys:
                    layout.tail(*volume)
                    local_keys[path] = layout.key(*volume, external_key, game_key)
                raw = layout.read(*volume, entry, local_keys[path], game_key, font.CONFIG_BYTES)
                docs.append(font.Document.read(path, entry.name, raw))
            font_resources = font.Resources(config, docs[0], docs[1:])
            font_layout = font.Layout.read(font_resources, glyphs)
            selected.update((path, indices[path]) for path in font_entries)
            companions.append(config.path)
        except ValueError as error:
            font_diagnostic = str(error)
    if glyphs:
        if not font_layout or not set(font_layout.runtime_files()) <= set(files):
            raise ValueError("QLIE 中文补丁的字体配置、字形映射或伴随文件不完整")
        companions.extend(font_layout.runtime_files())
    if exe and (font_layout or key_file):
        companions.append(exe)
        companions.extend(path for path in files if path.casefold() == exe.casefold() + ".le.config")
    if key_file:
        companions.append(key_file)
    required = sorted([*selected, *companions], key=str.casefold)
    index_bytes = sum(index.file_size - index.index_offset for _, index in indices.values())
    install = Installation(required, exe, layout, selected, game_key, external_key, local_keys,
                           fingerprints, index_bytes, font_layout, font_diagnostic, glyphs, font_resources)
    support.script_profile(selected, install.read, layout)
    return install


def text_units(relative, member, row_number, record, row, *, unicode_scripts):
    raw_name = record.speaker.raw if record.speaker else ""
    speaker = raw_name.split("＠", 1)[0] or None
    kinds = {"choice": "choice", "save-title": "ui"}
    for field, text in row.items():
        if not text.strip() or field == "name" and raw_name.startswith("$"):
            continue
        locator = json.dumps([member, row_number, field], ensure_ascii=False, separators=(",", ":"))
        kind = "name" if field == "name" else kinds.get(record.parts[0].kind, "dialogue")
        tokens = ["\n"] if field == "message" and len(record.parts) > 1 else []
        if unicode_scripts:
            tokens.extend(dict.fromkeys(tag[0] for tag in script_format.RUBY.finditer(text)))
        yield field, dict(id=archive.digest(f"{relative}\0{locator}".encode("utf-8")),
                          resourcePath=relative, locator=locator, context=member, sourceText=text,
                          speaker=None if kind == "name" else speaker, kind=kind,
                          protectedTokens=tokens or None)


def read_scripts(install):
    decoded = 0
    for path, (_, index) in install.archives.items():
        for entry in script_members(index):
            try:
                raw = install.read(path, entry, min(MAX_SCRIPT, MAX_TOTAL - decoded))
                decoded += len(raw)
                script, encoding, bom = script_format.decode(raw, unicode_scripts=install.layout.unicode_scripts)
            except ValueError as error:
                raise ValueError(f"{path}/{entry.name}：{error}") from error
            yield path, entry.name, script, encoding, bom


def scripts(install, sources, translations=None, mapping=None, faces=None):
    units, changes, used = [], {}, set()
    changed = 0
    mapping = install.glyphs if mapping is None else mapping
    reverse = {code: slot for slot, code in mapping.items()}
    options = dict(unicode_scripts=install.layout.unicode_scripts)
    for path, member, script, encoding, bom in sources:
        try:
            cp932 = encoding == 'cp932'
            rows, modified = [], False
            for number, record in enumerate(script_format.scan(script, **options)):
                row = record.row()
                decoded = {field: value.translate(install.glyphs) for field, value in row.items()} if cp932 and install.glyphs else row
                for field, unit in text_units(path, member, number, record, decoded, **options):
                    if translations is None:
                        units.append(unit)
                    elif unit["id"] in translations:
                        used.add(unit["id"])
                        replacement = translations[unit["id"]]
                        if replacement != decoded[field]:
                            encoded = replacement.translate(reverse) if cp932 else replacement
                            if cp932 and encoded.translate(mapping) != replacement:
                                raise ValueError("译文与 QLIE 字形码位冲突")
                            row[field] = encoded
                            modified = True
                rows.append(row)
            patched = script_format.patch(script, rows, **options) if modified else script
            if faces:
                patched = install.font_layout.script_faces(patched, faces)
            if patched != script:
                encoded = patched.encode(encoding)
                if encoded.decode(encoding) != patched:
                    raise ValueError("译文按脚本原编码回读时发生变化")
                data = bom + encoded
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
    support.check_rebuild(install.layout, source, before, output, install.internal_keys[path],
                          install.game_key, changes, limit=MAX_SCRIPT)


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
        warnings = [FONT_WARNING if install.font_layout else
                    UNICODE_WARNING if install.layout.unicode_scripts else WARNING]
        if operation == "probe":
            entries = [entry for _, index in install.archives.values() for entry in script_members(index)]
            distinct_names = {entry.name.replace("\\", "/").casefold() for entry in entries}
            duplicates = len(entries) - len(distinct_names)
            if duplicates:
                warnings.append(f"各卷中存在 {duplicates} 份同路径脚本，需确认运行时的卷加载顺序")
            return dict(indexFingerprints=install.fingerprints, indexBytes=install.index_bytes,
                        mediaOnlyArchives=len(install.fingerprints) - len(install.archives),
                        scriptCount=len(entries), duplicateScripts=duplicates,
                        scriptBytes=sum(entry.unpacked_size for entry in entries),
                        systemFont=dict(matched=install.font_layout is not None, diagnostic=install.font_diagnostic),
                        probe=dict(engineId="qlie", adapterId=f"template.{install.layout.template}", adapterVersion="1",
                                   description="QLIE 封包内的 ImoScripter FormatType=1 脚本",
                                   requiredFiles=install.files,
                                   originalExecutable=install.executable if install.executable in install.files else None,
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
    if needed:
        sources = list(sources)
        if install.layout.unicode_scripts:
            legacy = [item for item in sources if item[3] == "cp932"]
            if len(legacy) != len(sources):
                units, _ = scripts(install, legacy)
                characters = {char for unit in units for char in translations.get(unit["id"], "")}
                needed = font.glyphs_needed(characters, mapping)
    runtime, faces = {}, {}
    if needed:
        if not layout:
            raise ValueError(f"QLIE 译文需要已确认的中文编码与字体方案（{install.font_diagnostic}）")
        if font.LAUNCHER.casefold() == install.executable.casefold():
            raise ValueError("QLIE 补丁启动器与原游戏 EXE 同名")
        assets = font.resolve_assets(request, layout)
        occupied = {char for _, _, script, _, _ in sources for char in font.PRIVATE_CHARS.findall(script)}
        mapping = font.plan(layout, mapping, needed, occupied, characters)
        faces = {face: font.family(style, mapping) + " Regular" for face, style in layout.faces.items()}
        runtime = font.make_runtime(layout, install.executable, mapping, assets)
    _, changes = scripts(install, sources, translations, mapping, faces)
    if faces:
        for doc, raw in layout.resource_edits(faces):
            if doc.member is None:
                runtime[doc.path] = raw
            else:
                changes.setdefault(doc.path, {})[doc.member] = raw
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
                        _, index = install.archives[path]
                        install.layout.rebuild(original, result, index, install.internal_keys[path],
                                               install.external_key, install.game_key, edits)
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


def main(template_id=None):
    request = json.load(sys.stdin)
    if template_id is not None:
        request["templateId"] = template_id
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
