"""Read QLIE trial fields and the system fonts selected by the rebuilt game."""
from contextlib import ExitStack
import json
from pathlib import Path
from xml.etree import ElementTree

from . import engine_archive as archive
from . import qlie_archive
from . import qlie_bitmap as bitmap
from . import qlie_pack_adapter as pack
from . import qlie_script
from . import qlie_systemfont as font


def trial_fields(install, samples, overlay):
    """Read each referenced member once, without applying a patch's reverse map."""
    members = {}
    for sample in samples:
        member, row, field = json.loads(sample["locator"])
        members.setdefault((sample["resourcePath"], member), []).append((row, field, sample["expected"]))
    entries = {(volume, entry.name): entry for volume, (_, index) in install.archives.items()
               for entry in pack.script_members(index)}
    for (volume, member), fields in members.items():
        if overlay:
            volumes = [path for path in overlay if (path, member) in entries]
            if len(volumes) != 1:
                raise ValueError(f"QLIE 试译成员 {member} 在叠加卷中有 {len(volumes)} 个候选，需确认游戏实际加载的脚本")
            volume = volumes[0]
        entry = entries.get((volume, member))
        if entry is None:
            raise ValueError(f"QLIE 试译脚本成员缺失：{volume}/{member}")
        raw = install.read(volume, entry, pack.MAX_SCRIPT)
        script, encoding, _ = qlie_script.decode(raw, unicode_scripts=install.layout.unicode_scripts)
        records = qlie_script.scan(script, unicode_scripts=install.layout.unicode_scripts, dialect=install.dialect)
        faces = set(font.FONT_COMMAND.findall(script))
        for row, field, expected in fields:
            if type(row) is not int or not 0 <= row < len(records) or field not in records[row].row():
                raise ValueError(f"QLIE 试译字段缺失：{volume}/{member}，行 {row}，字段 {field}")
            yield records[row].row()[field], expected, faces, encoding


def locate_samples(source, samples):
    # Other adapters can use their own IDs and locators. Bind them to original QLIE fields.
    units, _ = pack.scripts(source, pack.read_scripts(source))
    by_text, by_locator = {}, {}
    for unit in units:
        path, text, locator = unit["resourcePath"], unit["sourceText"], unit["locator"]
        by_text.setdefault((path, text), []).append(locator)
        by_locator[path, locator] = text
    matched = []
    for sample in samples:
        path, text = sample["resourcePath"], sample["original"]
        if by_locator.get((path, sample["locator"])) == text:
            matched.append(sample)
        elif candidates := by_text.get((path, text)):
            if len(candidates) != 1:
                raise ValueError(f"QLIE 原文在 {path} 中有多个同文位置，试译需要可核对的卷、成员和字段位置")
            matched.append(dict(sample, locator=candidates[0]))
    return matched


def all_script_faces(install):
    """Collect explicit \\fn2 font names across all scripts, including startup/config helpers."""
    selected = set()
    decoded = 0
    for volume, (_, index) in install.archives.items():
        for entry in pack.script_members(index):
            if decoded >= pack.MAX_TOTAL:
                break
            raw = install.read(volume, entry, min(pack.MAX_SCRIPT, pack.MAX_TOTAL - decoded))
            decoded += len(raw)
            script, _, _ = qlie_script.decode(raw, unicode_scripts=install.layout.unicode_scripts)
            selected.update(font.FONT_COMMAND.findall(script))
    return selected


def inherited_faces(install):
    selected = set()
    for volume, (_, index) in install.archives.items():
        for entry in pack.script_members(index):
            if entry.name.replace("\\", "/").casefold() != font.SETUP_MEMBER:
                continue
            raw = install.read(volume, entry, pack.MAX_SCRIPT)
            script, _, _ = qlie_script.decode(raw, unicode_scripts=install.layout.unicode_scripts)
            selection = font.base_selection(script)
            if selection:
                start, end, _, _ = selection
                faces = font.base_faces(script[start:end])
                if not faces:
                    raise ValueError(f"{volume}/{entry.name} 的对白会继承玩家保存的字体，尚未在继承前选用配套字体")
                selected.update(faces)
    return selected


class EffectiveResources:
    def __init__(self, original, output, files):
        self.original, self.output, self.files = original, output, set(files)

    def __truediv__(self, relative):
        return (self.output if relative in self.files else self.original) / relative


def inspect_tails(install, *, source_archives=None, unregistered_source=None):
    """Validate known lookup tails strictly; allow unregistered tails only when present in source."""
    unregistered = {}
    for path, (stream, index) in install.archives.items():
        sig = qlie_archive.tail_signature(stream, index)
        if sig in install.layout.known_tails:
            install.layout.tail(stream, index)
            continue
        if unregistered_source is not None:
            if path in unregistered_source or (
                source_archives is not None and path not in source_archives and unregistered_source
            ):
                if index.file_size - 28 - index.index_end < 1060:
                    raise ValueError(f"{path}：QLIE 封包尾表长度不足，缺少密钥区")
                unregistered[path] = qlie_archive.tail_label(sig)
                continue
            raise ValueError(f"{path}：原封包使用宿主已登记尾表，但回包后尾表变为未登记格式（{qlie_archive.tail_label(sig)}）")
        unregistered[path] = qlie_archive.tail_label(sig)
    return unregistered


LAUNCHER_ARTIFACTS = (font.MAPPING, font.LAUNCH, font.LAUNCHER)


def has_launcher_artifacts(files):
    return any(path in files for path in LAUNCHER_ARTIFACTS)


def recognized_font_slots(install):
    if not install.font_resources:
        return None, install.font_diagnostic or "缺少可核验的系统字体表或外置配置"
    try:
        return install.font_resources.slots(), ""
    except ValueError as error:
        return None, str(error)


def verify_launcher_bundle(
    root,
    request,
    files,
    effective_files,
    *,
    evidence,
    expected_executable=None,
    required_faces=None,
    fixed_faces=frozenset(),
):
    launcher = request.get("launcher")
    if (not launcher or "/" in launcher or launcher not in files
            or font.LAUNCH not in effective_files):
        raise ValueError(f"{evidence}，补丁启动入口缺少加载配套字体的配置")
    try:
        launch = ElementTree.fromstring(archive.read_file(root / font.LAUNCH, font.CONFIG_BYTES))
    except ElementTree.ParseError as error:
        raise ValueError(f"{evidence}，字体启动配置 XML 解析失败：{error}") from error
    if launch.tag != "launch":
        raise ValueError(f"{evidence}，字体启动配置未指向当前游戏 EXE")
    configured_exe = launch.get("executable")
    if expected_executable is not None:
        if configured_exe != expected_executable:
            raise ValueError(f"{evidence}，字体启动配置未指向当前游戏 EXE")
    else:
        known_files = {path.casefold() for path in (*request["resources"], *files)}
        if (not configured_exe or configured_exe != configured_exe.strip()
                or "/" in configured_exe or "\\" in configured_exe
                or not configured_exe.lower().endswith(".exe")
                or configured_exe.casefold() not in known_files):
            raise ValueError(f"{evidence}，字体启动配置未指向当前游戏根目录的 EXE：{configured_exe or '(空)'}")
    registered = {}
    for node in launch.findall("font"):
        face = node.get("face")
        raw_file = node.get("file")
        if not face or face != face.strip() or not raw_file:
            raise ValueError(f"{evidence}，启动配置的字体项缺少有效 face 或 file")
        path = archive.member_name(raw_file)
        if face in registered or path not in files:
            raise ValueError(f"{evidence}，启动配置的字体文件缺失或名称重复：{path}")
        registered[face] = path
    if not registered:
        raise ValueError(f"{evidence}，字体启动配置未声明任何配套字体")
    if required_faces is not None:
        missing = required_faces - registered.keys()
        if missing or not required_faces:
            raise ValueError(f"{evidence}，启动器没有加载游戏选用的字体：{', '.join(sorted(missing))}")
        selected = sorted(required_faces)
    else:
        selected = sorted(registered)
    return [dict(face=face, path=registered[face], fixed=face in fixed_faces) for face in selected]


def read_declared_aliases(root, effective_files, *, evidence, script_aliases=None):
    if font.MAPPING not in effective_files:
        return dict(script_aliases or {})
    declared = {chr(slot): chr(code) for slot, code in font.old_mapping(root, effective_files).items()}
    if script_aliases is None:
        return declared
    for slot, char in script_aliases.items():
        if slot in declared and declared[slot] != char:
            raise ValueError(
                f"{evidence}，但 {font.MAPPING} 将 U+{ord(slot):04X} 映射为「{declared[slot]}」"
            )
    return {**declared, **script_aliases}


def verify_unregistered_bundle(
    root,
    request,
    files,
    effective_files,
    *,
    evidence,
    warning,
    expected_executable=None,
    aliases=None,
):
    merged_aliases = read_declared_aliases(root, effective_files, evidence=evidence, script_aliases=aliases)
    fonts = verify_launcher_bundle(
        root,
        request,
        files,
        effective_files,
        evidence=evidence,
        expected_executable=expected_executable,
    )
    return dict(fonts=fonts, aliases=merged_aliases, warnings=[warning])


def verify(request):
    output = Path(request["output"])
    original = Path(request["source"])
    files = request["files"]
    overlay = set(files) if request["mode"] == "overlay" else set()
    root = EffectiveResources(original, output, files) if overlay else output
    effective_files = [*request["resources"], *files] if overlay else files
    with Path(request["samplesFile"]).open(encoding="utf-8") as stream:
        samples = json.load(stream)
    warnings = []
    with ExitStack() as stack:
        source = None
        source_archives = None
        unregistered_source = None
        source_font_slots = None
        if not request["template"]:
            try:
                source = pack.locate(original, {}, request["resources"], stack, validate_tail=False)
                source_archives = set(source.archives)
                unregistered_source = inspect_tails(source)
                source_font_slots, _ = recognized_font_slots(source)
            except ValueError as error:
                if has_launcher_artifacts(files) or has_launcher_artifacts(effective_files):
                    detail = (
                        "已独立核验字体启动器、字体文件与字形映射，脚本译文回读与字体选用需进游戏确认。"
                        if font.MAPPING in effective_files else
                        "已独立核验字体启动器与字体文件，未发现字形映射表，脚本译文回读与实际显示需进游戏确认。"
                    )
                    return verify_unregistered_bundle(
                        root, request, files, effective_files,
                        evidence="QLIE 未登记方言的字体启动补丁",
                        warning=f"宿主未登记当前 QLIE 脚本方言或封包布局（{error}）；{detail}",
                    )
                return dict(fonts=[], aliases={}, warnings=[])  # FilePack also carries other script formats.
            samples = locate_samples(source, samples)
            if not samples:
                if has_launcher_artifacts(files):
                    return verify_unregistered_bundle(
                        root, request, files, effective_files,
                        evidence="QLIE 字体启动补丁",
                        warning="试译样本未命中宿主已识别的 QLIE 剧本字段；已独立核验字体启动器与字体资源，实际显示需进游戏确认。",
                        expected_executable=source.executable,
                    )
                return dict(fonts=[], aliases={}, warnings=[])
        try:
            install = pack.locate(root, {}, [p for p in effective_files if p != font.MAPPING], stack,
                                  validate_tail=bool(request["template"]))
            if not request["template"]:
                unregistered_output = inspect_tails(
                    install, source_archives=source_archives, unregistered_source=unregistered_source,
                )
                if unregistered_output:
                    labels = "、".join(sorted(set(unregistered_output.values())))
                    warnings.append(
                        f"QLIE 封包尾表版本（{labels}）未在宿主登记；已独立核验脚本成员、译文与字形，文件名查找表需另行确认。"
                    )
        except ValueError as error:
            raise ValueError(
                f"宿主独立显示回读器检查回填结果失败：{error}。"
                "原版资源可被宿主识别，请核对回填后的封包结构、脚本与字体配置"
            ) from error
        base = pack.unicode_dialogue(root, install, patched=True) if samples and request["template"] else set()
        if base and base | install.font_resources.selected_faces() != {font.UNICODE_FACE}:
            raise ValueError("QLIE 启动字体表与对白基础字体没有统一使用微软雅黑")
        aliases, script_faces, bitmap_chars = {}, set(), set()
        for raw, expected, faces, encoding in trial_fields(install, samples, overlay):
            if base and (not encoding.startswith("utf-16") or raw != expected
                         or {f for f in faces if not install.font_resources.is_ignored_face(f)} - {font.UNICODE_FACE}):
                raise ValueError("QLIE 译文字段需要直接写入 UTF-16 剧本并使用统一的对白字体")
            if raw == expected:
                continue
            if install.bitmap_font and bitmap.TOKEN.search(raw):
                if install.bitmap_font.decode(raw) != expected:
                    raise ValueError("QLIE 试译字段的外字替换表与目标文字不一致")
                bitmap_chars.update(chr(int(m[1], 16)) for m in bitmap.TOKEN.finditer(raw))
                continue
            if len(raw) != len(expected):
                raise ValueError("QLIE 脚本字段长度与宿主试译不一致，需核对游戏实际解码方式")
            for slot, char in zip(raw, expected):
                if slot == char:
                    continue
                if ord(slot) not in font.SLOTS or aliases.setdefault(slot, char) != char:
                    raise ValueError(f"QLIE 试译字符「{char}」在游戏脚本中读为 U+{ord(slot):04X}，缺少一致的解码或字形映射")
            script_faces.update(faces)
        if bitmap_chars:
            native = install.bitmap_font
            native.routes()
            atlas = bitmap.Atlas(native.read(bitmap.ATLAS, bitmap.LIMIT))
            for char, expected in bitmap.rasterize(bitmap_chars, request["toolPaths"]):
                actual = atlas.mask(native.rules[bitmap.token(char)])
                # Published adapters can still produce the original regular-weight OF glyph.
                if actual != expected and actual != bitmap.dialogue_mask(expected):
                    raise ValueError(f"QLIE 外字位图中的「{char}」与目标汉字的字形不一致")
        if not aliases:
            if not request["template"] and has_launcher_artifacts(files):
                declared = read_declared_aliases(root, effective_files, evidence="QLIE 字体启动补丁")
                fonts = verify_launcher_bundle(
                    root, request, files, effective_files,
                    evidence="QLIE 字体启动补丁",
                    expected_executable=install.executable,
                )
                return dict(fonts=fonts, aliases=declared, warnings=warnings)
            return dict(fonts=[], aliases={}, warnings=warnings)
        slot, char = next(iter(aliases.items()))
        evidence = f"QLIE 试译字符「{char}」在游戏脚本中写为 U+{ord(slot):04X}"
        if install.bitmap_font:
            raise ValueError(f"{evidence}，游戏含外字图标槽，缺少该私用码位实际绘制目标汉字的证据")
        install_slots, install_font_reason = recognized_font_slots(install)
        if install_slots is None:
            if not request["template"] and source_font_slots is None:
                res = verify_unregistered_bundle(
                    root, request, files, effective_files,
                    evidence=evidence,
                    warning=(
                        f"宿主未登记当前 QLIE 字体配置布局（{install_font_reason}）；"
                        "已独立核验试译码位、字体启动器与字形映射，游戏字体选用需进游戏确认。"
                    ),
                    expected_executable=install.executable,
                    aliases=aliases,
                )
                res["warnings"] = [*warnings, *res["warnings"]]
                return res
            raise ValueError(f"{evidence}，缺少可核验的系统字体选择：{install_font_reason}")
        if not install.executable:
            raise ValueError(f"{evidence}，补丁启动入口缺少加载配套字体的配置")
        slots = install_slots.values()
        fixed = {face for selector, face in slots if selector == "system_fix"}
        all_faces = script_faces | all_script_faces(install)
        selected = (
            {face for _, face in slots}
            | install.font_resources.selected_faces()
            | {face for face in all_faces if not install.font_resources.is_ignored_face(face)}
        )
        selected.update(inherited_faces(install))
        fonts = verify_launcher_bundle(
            root, request, files, effective_files,
            evidence=evidence,
            expected_executable=install.executable,
            required_faces=selected,
            fixed_faces=fixed,
        )
        return dict(fonts=fonts, aliases=aliases, warnings=warnings)
