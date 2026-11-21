"""Read ImoScripter trial fields and the system fonts selected by the rebuilt game."""
from contextlib import ExitStack
import json
from pathlib import Path
from xml.etree import ElementTree

from . import engine_archive as archive
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
        records = qlie_script.scan(script, unicode_scripts=install.layout.unicode_scripts)
        faces = set(font.FONT_COMMAND.findall(script))
        for row, field, expected in fields:
            if type(row) is not int or not 0 <= row < len(records) or field not in records[row].row():
                raise ValueError(f"QLIE 试译字段缺失：{volume}/{member}，行 {row}，字段 {field}")
            yield records[row].row()[field], expected, faces, encoding


def locate_samples(source, samples):
    # Other adapters can use their own IDs and locators. Bind them to original ImoScripter fields.
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


def verify(request):
    output = Path(request["output"])
    original = Path(request["source"])
    files = request["files"]
    overlay = set(files) if request["mode"] == "overlay" else set()
    root = EffectiveResources(original, output, files) if overlay else output
    with Path(request["samplesFile"]).open(encoding="utf-8") as stream:
        samples = json.load(stream)
    with ExitStack() as stack:
        if not request["template"]:
            try:
                source = pack.locate(original, {}, request["resources"], stack)
            except ValueError:
                return dict(fonts=[], aliases={})  # FilePack also carries other script formats.
            samples = locate_samples(source, samples)
            if not samples:
                return dict(fonts=[], aliases={})
        effective_files = [*request["resources"], *files] if overlay else files
        install = pack.locate(root, {}, [p for p in effective_files if p != font.MAPPING], stack)
        base = pack.unicode_dialogue(root, install, patched=True) if samples and request["template"] else set()
        if base and base | install.font_resources.selected_faces() != {font.UNICODE_FACE}:
            raise ValueError("QLIE 启动字体表与对白基础字体没有统一使用微软雅黑")
        aliases, script_faces = {}, set()
        for raw, expected, faces, encoding in trial_fields(install, samples, overlay):
            if base and (not encoding.startswith("utf-16") or raw != expected
                         or faces - font.INHERITED - {font.UNICODE_FACE}):
                raise ValueError("QLIE 译文字段需要直接写入 UTF-16 剧本并使用统一的对白字体")
            if raw == expected:
                continue
            if len(raw) != len(expected):
                raise ValueError("QLIE 脚本字段长度与宿主试译不一致，需核对游戏实际解码方式")
            for slot, char in zip(raw, expected):
                if slot == char:
                    continue
                if ord(slot) not in font.SLOTS or aliases.setdefault(slot, char) != char:
                    raise ValueError(f"QLIE 试译字符「{char}」在游戏脚本中读为 U+{ord(slot):04X}，缺少一致的解码或字形映射")
            script_faces.update(faces)
        if not aliases:
            return dict(fonts=[], aliases={})
        slot, char = next(iter(aliases.items()))
        evidence = f"QLIE 试译字符「{char}」在游戏脚本中写为 U+{ord(slot):04X}"
        if not install.font_resources:
            raise ValueError(f"{evidence}，缺少可核验的系统字体选择：{install.font_diagnostic}")
        if (not request["launcher"] or "/" in request["launcher"] or request["launcher"] not in files
                or font.LAUNCH not in effective_files or not install.executable):
            raise ValueError(f"{evidence}，补丁启动入口缺少加载配套字体的配置")
        launch = ElementTree.fromstring(archive.read_file(root / font.LAUNCH, font.CONFIG_BYTES))
        if launch.tag != "launch" or launch.get("executable") != install.executable:
            raise ValueError(f"{evidence}，字体启动配置未指向当前游戏 EXE")
        slots = install.font_resources.slots().values()
        fixed = {face for selector, face in slots if selector == "system_fix"}
        selected = {face for _, face in slots} | install.font_resources.selected_faces() | (script_faces - font.INHERITED)
        selected.update(inherited_faces(install))
        registered = {}
        for node in launch.findall("font"):
            face = node.get("face")
            path = archive.member_name(node.get("file"))
            if face in registered or path not in files:
                raise ValueError(f"{evidence}，启动配置的字体文件缺失或名称重复：{path}")
            registered[face] = path
        missing = selected - registered.keys()
        if missing or not selected:
            raise ValueError(f"{evidence}，启动器没有加载游戏选用的字体：{', '.join(sorted(missing))}")
        return dict(fonts=[dict(face=face, path=registered[face], fixed=face in fixed)
                           for face in sorted(selected)], aliases=aliases)
