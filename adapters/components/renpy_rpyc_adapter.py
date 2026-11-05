"""Fixed RPYC2 protocol adapter; semantic parsing stays in the pinned library.

No Ren'Py objects or game Python are executed. RPA media stays in its archive;
only compiled scripts are extracted, and every untouched member is validated.
"""
import hashlib
import json
from pathlib import Path
import shutil
import sys
import tempfile

from python.engines import renpy_rpyc as rpyc
from . import engine_archive as archive

WARNING = ("仅处理选中的 RPYC2 对白、菜单及 TranslateString.new；角色名只作上下文。"
           "Python／screen 内文字未覆盖；游戏启动、字体及中文显示尚未实机验证。")


def canonical(value):
    return json.dumps(value, ensure_ascii=True, sort_keys=True, separators=(",", ":"))


def resolve(root, relative):
    return archive.checked_path(Path(root) / archive.member_name(relative))


def identity(resource, member, locator):
    # Pickle byte offsets move when text grows. Only semantic identity is public.
    semantic = {key: value for key, value in locator.items() if key != "sites"}
    location = canonical(dict(script=member, **semantic))
    unit_id = "rpyc:" + hashlib.sha256((resource + "\0" + location).encode()).hexdigest()
    return unit_id, location


def read_script(path, member):
    try:
        script = rpyc.parse(archive.read_file(path, rpyc.MAX_FILE))
        names = rpyc.character_names(script.slots[1])
        rows, manifest = rpyc.export(script, filename=member, names=names)
        return script, names, rows, manifest
    except Exception as error:
        raise ValueError(f"RPYC2 {member}: {error}") from error


def units(resource, member, rows, manifest):
    for row, record in zip(rows, manifest["records"]):
        locator = record["locator"]
        unit_id, location = identity(resource, member, locator)
        yield dict(id=unit_id, resourcePath=resource, locator="rpyc:" + location,
                   sourceText=row["message"], speaker=row.get("name") or locator["speaker"],
                   kind="choice" if locator["role"] == "choice" else "dialogue",
                   context=f"{resource} → {member}:{locator['line']} ({locator['role']})",
                   protectedTokens=list(dict.fromkeys(rpyc.controls(row["message"]))))


def load(source, files, packed, scratch, verify=False):
    scripts, reports, seen, selected_bytes, entries = [], {}, set(), 0, 0
    if packed and len(files) > 16:
        raise ValueError("RPYC2 流程最多处理 16 个封包。")
    for index, resource in enumerate(files):
        if not packed:
            path = resolve(source, resource)
            selected_bytes += path.stat().st_size
            if selected_bytes > archive.MAX_TOTAL:
                raise ValueError("RPYC2 脚本超过 256 MiB 总量预算。")
            scripts.append((resource, resource, path))
            continue
        directory = scratch / str(index)
        report = archive.execute(dict(protocolVersion=1, component="archive.rpa3", operation="extract",
                                      archive=str(resolve(source, resource)), outputDirectory=str(directory),
                                      extensions=[".rpyc"], verifyContents=verify))
        reports[resource] = report
        selected_bytes += report["selectedBytes"]
        entries += report["entryCount"]
        if selected_bytes > archive.MAX_TOTAL or entries > archive.MAX_ENTRIES:
            raise ValueError("RPYC2 封包合计超过 256 MiB 脚本／10,000 成员预算。")
        unsupported = [entry["name"] for entry in report["entries"]
                       if Path(entry["name"]).suffix.lower() in (".rpy", ".rpym", ".rpymc")
                       or entry["name"].lower().endswith("_ren.py")]
        if unsupported:
            raise ValueError("RPYC2 流程不能确定源码或模块的加载顺序：" + resource + " → " + ", ".join(unsupported[:8]))
        for member in sorted(report["extractedFiles"]):
            key = member.casefold()
            if key in seen:
                raise ValueError("多个封包提供相同 RPYC 路径，需先确认覆盖顺序：" + member)
            seen.add(key)
            scripts.append((resource, member, resolve(directory, member)))
    return scripts, reports


def probe(source, files, packed, template):
    for resource in files:
        with resolve(source, resource).open("rb") as stream:
            header = stream.read(len(rpyc.MAGIC))
        matched = header[:8] in (b"RPA-3.0 ", b"ARC-3.0 ") if packed else header == rpyc.MAGIC
        if not matched:
            return dict(probe=None)
    return dict(probe=dict(engineId="Renpy", adapterId=template, adapterVersion="1",
                           description="Ren’Py RPYC2 固定适配流程", requiredFiles=files, warnings=[WARNING]))


def extract(scripts):
    result = []
    for resource, member, path in scripts:
        _, _, rows, manifest = read_script(path, member)
        result.extend(units(resource, member, rows, manifest))
    return dict(extraction=dict(units=result, warnings=[WARNING]))


def build(request, source, files, packed, scripts, reports, scratch):
    translations = request.get("translations") or {}
    if request.get("translationsFile"):
        translations = json.loads(archive.read_file(request["translationsFile"], 128 << 20))
    if not isinstance(translations, dict) or any(not isinstance(text, str) for text in translations.values()):
        raise ValueError("译文必须为 ID 到文本的映射。")
    remaining, replacements = set(translations), {resource: {} for resource in files}
    output = archive.checked_path(request["outputDirectory"])
    archive.separate(archive.checked_path(source), output)
    for index, (resource, member, path) in enumerate(scripts):
        script, names, rows, manifest = read_script(path, member)
        for row, record in zip(rows, manifest["records"]):
            unit_id, _ = identity(resource, member, record["locator"])
            if unit_id in translations:
                row["message"] = translations[unit_id]
                remaining.discard(unit_id)
        patched = rpyc.patch(script, rows, manifest, filename=member, names=names)
        if not packed:
            target = resolve(output, resource)
        elif patched != script.raw:
            target = scratch / ("changed-" + str(index))
            raw_name = next(entry["name"] for entry in reports[resource]["entries"]
                            if archive.member_name(entry["name"]) == member)
            replacements[resource][raw_name] = str(target)
        else:
            continue
        target.parent.mkdir(parents=True, exist_ok=True)
        with target.open("xb") as stream:
            stream.write(patched)
    if remaining:
        raise ValueError("部分译文无法对应当前 RPYC2 脚本，请重新提取：" + ", ".join(sorted(remaining)[:3]))
    if packed:
        for resource in files:
            target = resolve(output, resource)
            if replacements[resource]:
                archive.execute(dict(protocolVersion=1, component="archive.rpa3", operation="rebuild",
                                     archive=str(resolve(source, resource)), outputFile=str(target),
                                     replacements=replacements[resource]))
            else:
                target.parent.mkdir(parents=True, exist_ok=True)
                with resolve(source, resource).open("rb") as original, target.open("xb") as destination:
                    shutil.copyfileobj(original, destination, 1 << 20)
    return dict(build=dict(files=files, warnings=[WARNING]))


def validate(request, files, packed, scripts, reports, scratch):
    after_scripts, after_reports = load(request["outputDirectory"], files, packed, scratch / "patched", verify=True)
    if [(r, m) for r, m, _ in scripts] != [(r, m) for r, m, _ in after_scripts]:
        raise ValueError("补丁改变了 RPYC2 脚本列表。")
    selected = {(r, m) for r, m, _ in scripts}
    for resource, report in reports.items():
        before = {entry["name"]: entry for entry in report["entries"]}
        after = {entry["name"]: entry for entry in after_reports[resource]["entries"]}
        if before.keys() != after.keys():
            raise ValueError("补丁改变了封包成员列表：" + resource)
        for member, entry in before.items():
            if (resource, archive.member_name(member)) not in selected and entry["sha256"] != after[member]["sha256"]:
                raise ValueError("补丁修改了未选择的封包成员：" + resource + " → " + member)
    for (_, member, before), (_, _, after) in zip(scripts, after_scripts):
        original, names, _, manifest = read_script(before, member)
        modified = rpyc.parse(archive.read_file(after, rpyc.MAX_FILE))
        rows, _ = rpyc.export(modified, filename=member, names=names)
        # Recreate exactly the permitted text changes from the original graph.
        # This also checks opaque Python, lookup keys, slots and digest suffix.
        if rpyc.patch(original, rows, manifest, filename=member, names=names) != modified.raw:
            raise ValueError("RPYC2 补丁改变了文本之外的内容：" + member)
    return dict(validation=dict(passed=True, issues=[], runtimeStatus="not-tested"))


def execute(request):
    if request.get("protocolVersion") != 1:
        raise ValueError("不支持的适配协议。")
    config = json.loads(archive.read_file(resolve(request["adapterDirectory"], "tools/adapter-template.json"), 1 << 20))
    packed = config["templateId"] == "rpa3-renpy-rpyc"
    if (config.get("schemaVersion") != 1 or config.get("templateVersion") != 1 or config.get("format") != "Renpy"
            or config.get("templateId") not in ("loose-renpy-rpyc", "rpa3-renpy-rpyc")
            or config.get("scriptExtensions") != [".rpyc"]
            or config.get("archiveComponent") != ("archive.rpa3" if packed else None)):
        raise ValueError("RPYC2 模板配置不匹配。")
    files = sorted(request["resourceFiles"])
    if not files or len(files) > 1024 or len(set(file.casefold() for file in files)) != len(files):
        raise ValueError("资源列表为空、过大或有重复路径。")
    if any(Path(file).suffix.lower() != (".rpa" if packed else ".rpyc") for file in files):
        raise ValueError("资源不属于配置选中的 RPYC2 格式。")
    source, operation = request["sourceDirectory"], request["operation"]
    for resource in files:
        resolve(source, resource)
    if operation == "probe":
        return probe(source, files, packed, config["templateId"])
    if operation not in ("extract", "build", "validate"):
        raise ValueError("不支持的适配操作。")
    with tempfile.TemporaryDirectory(prefix="rpyc-", dir=Path.cwd()) as directory:
        scratch = Path(directory)
        scripts, reports = load(source, files, packed, scratch / "original", verify=operation == "validate")
        if operation == "extract":
            return extract(scripts)
        if operation == "build":
            return build(request, source, files, packed, scripts, reports, scratch)
        return validate(request, files, packed, scripts, reports, scratch)


def main():
    request = json.load(sys.stdin)
    try:
        response = dict(protocolVersion=1, success=True, **execute(request))
    except Exception as error:
        response = dict(protocolVersion=1, success=False, error=f"RPYC2 {type(error).__name__}: {error}")
        print(response["error"], file=sys.stderr)
    text = json.dumps(response, ensure_ascii=False, separators=(",", ":"))
    if request.get("responseFile"):
        with archive.checked_path(request["responseFile"]).open("x", encoding="utf-8") as stream:
            stream.write(text)
    else:
        print(text)
    return 0 if response["success"] else 1
