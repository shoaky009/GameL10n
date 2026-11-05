"""Compose existing Ren'Py readers for disjoint loose and archived scripts."""
from dataclasses import dataclass
import hashlib
import json
from pathlib import Path, PurePosixPath
import shutil
import sys
import tempfile

from . import engine_archive as archive
from . import renpy_rpyc_adapter as compiled
from .gamel10n_host import parser, write_response

WARNING = ("仅处理选中 Ren’Py 脚本的已支持文本字段；不推断同名覆盖和自定义加载规则。"
           "Python／screen 内文字、独立文字覆盖及游戏启动、字体和中文显示仍需另行验证。")


@dataclass
class Container:
    resource: str | None
    directory: Path
    files: list
    members: list


def load(source, files, scratch, verify=False):
    """Check the complete namespace from indices before reading script payloads."""
    packed = [file for file in files if file.lower().endswith(".rpa")]
    loose = [file for file in files if not file.lower().endswith(".rpa")]
    if len(packed) > 16:
        raise ValueError("Ren’Py 混合流程最多处理 16 个封包。")
    roots = {PurePosixPath(file).parent.parts for file in packed}
    if len(roots) > 1:
        raise ValueError("封包不在同一资源目录，需先确认加载根目录：" + ", ".join(packed[:8]))
    root = next(iter(roots), ())
    seen = {}
    input_bytes = selected_bytes = entries = 0

    def register(virtual, origin, length):
        nonlocal selected_bytes
        maximum = compiled.rpyc.MAX_FILE if virtual.lower().endswith(".rpyc") else archive.MAX_FILE
        if length > maximum:
            raise ValueError("单个脚本超过读取预算：" + origin)
        selected_bytes += length
        if selected_bytes > archive.MAX_TOTAL:
            raise ValueError("Ren’Py 脚本合计超过 256 MiB 预算。")
        key = str(PurePosixPath(virtual).with_suffix("")).casefold()
        if key in seen:
            raise ValueError("Ren’Py 脚本加载冲突，需先确认覆盖顺序：" + seen[key] + " / " + origin)
        seen[key] = origin

    for resource in loose:
        path = compiled.resolve(source, resource)
        length = path.stat().st_size
        input_bytes += length
        parts = PurePosixPath(resource).parts
        if packed and tuple(part.casefold() for part in parts[:len(root)]) != tuple(part.casefold() for part in root):
            raise ValueError("松散脚本与封包不在同一可确定的加载根目录：" + resource)
        register("/".join(parts[len(root):]), resource, length)
    for resource in packed:
        report = archive.execute(dict(protocolVersion=1, component="archive.rpa3", operation="inspect",
                                      archive=str(compiled.resolve(source, resource)), extensions=[".rpy", ".rpyc"]))
        input_bytes += report["inputBytes"]
        entries += report["entryCount"]
        for member in report["entries"]:
            name = archive.member_name(member["name"])
            extension = PurePosixPath(name).suffix.lower()
            if extension in (".rpy", ".rpyc"):
                register(name, resource + " → " + name, member["length"])
            elif extension in (".rpym", ".rpymc", ".rpa", ".xp3", ".ks", ".soc", ".tjs", ".scn") or name.lower().endswith("_ren.py"):
                raise ValueError("发现模板未覆盖的模块或封包：" + resource + " → " + name)
    if input_bytes > archive.RPA_MAX_ARCHIVE or entries > archive.MAX_ENTRIES:
        raise ValueError("Ren’Py 混合资源超过 2 GiB 输入／10,000 封包成员预算。")
    if not seen:
        raise ValueError("选中资源中没有 .rpy 或 RPYC2 脚本。")

    containers = [Container(None, Path(source), loose, [])] if loose else []
    for index, resource in enumerate(packed):
        directory = scratch / str(index)
        report = archive.execute(dict(protocolVersion=1, component="archive.rpa3", operation="extract",
                                      archive=str(compiled.resolve(source, resource)), outputDirectory=str(directory),
                                      extensions=[".rpy", ".rpyc"], verifyContents=verify))
        containers.append(Container(resource, directory, sorted(report["extractedFiles"]), report["entries"]))
    return containers


def source_files(container):
    return [file for file in container.files if file.lower().endswith(".rpy")]


def source_parser(request, operation, container, output=None, translations=None):
    # The parser returns JSON directly; the outer responseFile belongs to this adapter.
    return parser(dict(toolPaths=request["toolPaths"], adapterDirectory=request["adapterDirectory"]), "Renpy",
                  operation=operation, sourceDirectory=str(container.directory), resourceFiles=source_files(container),
                  outputDirectory=str(output) if output is not None else None, translations=translations)


def external_id(container, unit_id):
    return ("component:" + hashlib.sha256((container.resource + "\0" + unit_id).encode()).hexdigest()
            if container.resource is not None else unit_id)


def source_units(request, container):
    return source_parser(request, "extract", container)["extraction"]["units"] if source_files(container) else []


def extract(request, containers):
    result = []
    for container in containers:
        for unit in source_units(request, container):
            if container.resource is not None:
                unit["id"] = external_id(container, unit["id"])
                unit["locator"] = "member:" + json.dumps([unit["resourcePath"], unit["locator"]], ensure_ascii=True, separators=(",", ":"))
                unit["resourcePath"] = container.resource
                unit["context"] = container.resource + " → " + unit["context"]
            result.append(unit)
        scripts = [(container.resource or file, file, compiled.resolve(container.directory, file))
                   for file in container.files if file.lower().endswith(".rpyc")]
        result.extend(compiled.extract(scripts)["extraction"]["units"])
    return dict(extraction=dict(units=result, warnings=[WARNING]))


def build(request, source, files, containers, scratch):
    translations = request.get("translations") or {}
    if request.get("translationsFile"):
        translations = json.loads(archive.read_file(request["translationsFile"], 128 << 20))
    if not isinstance(translations, dict) or any(not isinstance(text, str) for text in translations.values()):
        raise ValueError("译文必须为 ID 到文本的映射。")
    remaining = set(translations)
    output = archive.checked_path(request["outputDirectory"])
    archive.separate(archive.checked_path(source), output)
    for index, container in enumerate(containers):
        patched = scratch / ("patched-" + str(index)) if container.resource is not None else output
        rpy_units = source_units(request, container)
        selected = {}
        for unit in rpy_units:
            unit_id = external_id(container, unit["id"])
            if unit_id in translations:
                selected[unit["id"]] = translations[unit_id]
                remaining.discard(unit_id)
        if source_files(container):
            source_parser(request, "build", container, patched, selected)
        for file in container.files:
            if not file.lower().endswith(".rpyc"):
                continue
            data, _, matched = compiled.build_script(container.resource or file, file,
                compiled.resolve(container.directory, file), translations)
            remaining.difference_update(matched)
            target = compiled.resolve(patched, file)
            target.parent.mkdir(parents=True, exist_ok=True)
            with target.open("xb") as stream:
                stream.write(data)
        if container.resource is None:
            continue
        replacements = {}
        for member in container.members:
            name = archive.member_name(member["name"])
            if name in container.files:
                path = compiled.resolve(patched, name)
                if archive.digest(archive.read_file(path, archive.MAX_FILE)) != member["sha256"]:
                    replacements[member["name"]] = str(path)
        target = compiled.resolve(output, container.resource)
        if replacements:
            archive.execute(dict(protocolVersion=1, component="archive.rpa3", operation="rebuild",
                                 archive=str(compiled.resolve(source, container.resource)), outputFile=str(target),
                                 replacements=replacements))
        else:
            target.parent.mkdir(parents=True, exist_ok=True)
            with compiled.resolve(source, container.resource).open("rb") as original, target.open("xb") as destination:
                shutil.copyfileobj(original, destination, 1 << 20)
    if remaining:
        raise ValueError("部分译文无法对应当前 Ren’Py 脚本，请重新提取：" + ", ".join(sorted(remaining)[:3]))
    return dict(build=dict(files=files, warnings=[WARNING]))


def validate(request, files, before, scratch):
    after = load(request["outputDirectory"], files, scratch / "patched", verify=True)
    if len(before) != len(after):
        raise ValueError("补丁改变了资源列表。")
    for original, patched in zip(before, after):
        if original.resource != patched.resource or original.files != patched.files:
            raise ValueError("补丁改变了脚本资源列表。")
        old = {entry["name"]: entry["sha256"] for entry in original.members}
        new = {entry["name"]: entry["sha256"] for entry in patched.members}
        if old.keys() != new.keys():
            raise ValueError("补丁改变了封包成员列表：" + str(original.resource))
        for member, digest in old.items():
            if archive.member_name(member) not in original.files and digest != new[member]:
                raise ValueError("补丁修改了未选择的封包成员：" + str(original.resource) + " → " + member)
        if source_files(original):
            checked = source_parser(request, "validate", original, patched.directory)["validation"]
            if not checked["passed"] or checked["issues"]:
                raise ValueError("Ren’Py 源码验证失败：" + json.dumps(checked["issues"], ensure_ascii=False))
        for file in original.files:
            if file.lower().endswith(".rpyc"):
                compiled.validate_script(compiled.resolve(original.directory, file), compiled.resolve(patched.directory, file), file)
    return dict(validation=dict(passed=True, issues=[], runtimeStatus="not-tested"))


def execute(request):
    if request.get("protocolVersion") != 1:
        raise ValueError("不支持的适配协议。")
    config = json.loads(archive.read_file(compiled.resolve(request["adapterDirectory"], "tools/adapter-template.json"), 1 << 20))
    if (config.get("schemaVersion") != 1 or config.get("templateVersion") != 1 or config.get("templateId") != "renpy-mixed"
            or config.get("format") != "Renpy" or config.get("archiveComponent") != "archive.rpa3"
            or config.get("scriptExtensions") != [".rpy", ".rpyc"]):
        raise ValueError("Ren’Py 混合模板配置不匹配。")
    files = sorted(archive.member_name(file) for file in request["resourceFiles"])
    if not files or len(files) > 1024 or len(set(file.casefold() for file in files)) != len(files):
        raise ValueError("资源列表为空、过大或有重复路径。")
    if any(PurePosixPath(file).suffix.lower() not in (".rpy", ".rpyc", ".rpa") for file in files):
        raise ValueError("资源不属于配置选中的 Ren’Py 格式。")
    source, operation = request["sourceDirectory"], request["operation"]
    if operation == "probe":
        for file in files:
            if not file.lower().endswith(".rpy") and compiled.probe(source, [file], file.lower().endswith(".rpa"), "renpy-mixed")["probe"] is None:
                return dict(probe=None)
        return dict(probe=dict(engineId="Renpy", adapterId="renpy-mixed", adapterVersion="1",
                               description="Ren’Py 无冲突混合资源", requiredFiles=files, warnings=[WARNING]))
    if operation not in ("extract", "build", "validate"):
        raise ValueError("不支持的适配操作。")
    with tempfile.TemporaryDirectory(prefix="renpy-", dir=Path.cwd()) as directory:
        scratch = Path(directory)
        containers = load(source, files, scratch / "original", verify=operation == "validate")
        if operation == "extract":
            return extract(request, containers)
        if operation == "build":
            return build(request, source, files, containers, scratch)
        return validate(request, files, containers, scratch)


def main():
    request = json.load(sys.stdin)
    try:
        response = dict(protocolVersion=1, success=True, **execute(request))
    except Exception as error:
        response = dict(protocolVersion=1, success=False, error=f"Ren’Py {type(error).__name__}: {error}"[:2000])
        print(response["error"], file=sys.stderr)
    write_response(request, response)
    return 0 if response["success"] else 1
