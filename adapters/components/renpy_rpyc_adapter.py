"""Fixed RPYC2 protocol adapter; semantic parsing stays in the pinned library.

No Ren'Py objects or game Python are executed. RPA media stays in its archive;
only compiled scripts are extracted, and every untouched member is validated.
"""
import hashlib
import json
from pathlib import Path, PurePosixPath
import shutil
import sys
import tempfile

from python.engines import renpy_rpyc as rpyc
from . import engine_archive as archive
from . import renpy_font
from .gamel10n_host import validation_context

WARNING = ("仅处理选中的 RPYC2 对白、菜单及 TranslateString.new；角色名只作上下文。"
           "封包默认以松散覆盖输出；覆盖和整包重建均按需附带中文字体回退脚本。"
           "Python／screen 内文字未覆盖；游戏启动、字体及中文显示尚未实机验证。")


def canonical(value):
    return json.dumps(value, ensure_ascii=True, sort_keys=True, separators=(",", ":"))


def resolve(root, relative):
    return archive.checked_path(Path(root) / archive.member_name(relative))


def overlay_path(resource, member):
    parent = PurePosixPath(archive.member_name(resource)).parent
    name = archive.member_name(member)
    return name if str(parent) in ("", ".") else (parent / name).as_posix()


def is_other_translation_script(member):
    parts = PurePosixPath(archive.member_name(member)).parts
    for index in range(len(parts) - 2):
        if parts[index].casefold() == "tl":
            return parts[index + 1].casefold() != "none"
    return False


def overlay_files_map(request, valid_targets, optional_extras=()):
    output = request.get("outputDirectory")
    output_files = request.get("outputFiles")
    if not output or not isinstance(output_files, list):
        raise ValueError("overlay 模式缺少 outputDirectory 或 outputFiles。")
    extras = {archive.member_name(item).casefold() for item in optional_extras}
    mapping = {}
    for item in output_files:
        relative = archive.member_name(item)
        key = relative.casefold()
        if key in mapping:
            raise ValueError("overlay 输出列表存在重复路径：" + relative)
        if key not in valid_targets and key not in extras:
            raise ValueError("overlay 输出包含未登记的脚本路径：" + relative)
        mapping[key] = resolve(output, relative)
    return mapping


def disk_files(root):
    base = archive.checked_path(root)
    if not base.exists():
        return set()
    return {
        archive.member_name(path.relative_to(base).as_posix()).casefold()
        for path in base.rglob("*")
        if path.is_file()
    }


def check_validation_context(request):
    context = validation_context(request)
    if context is None:
        return
    original = context["originalUnits"]
    after = {unit["id"]: unit for unit in context["outputUnits"]}
    translations = context["translations"]
    if len(after) != len(original):
        raise ValueError("回填改变了文本数量。")
    changed_cjk = False
    for unit in original:
        candidate = after.get(unit["id"])
        if not candidate or candidate["sourceText"] != translations.get(unit["id"], unit["sourceText"]):
            raise ValueError("回填文本与预期不一致：" + unit["id"])
        if not changed_cjk and candidate["sourceText"] != unit["sourceText"]:
            changed_cjk = bool(renpy_font.CJK_CHAR_RE.search(candidate["sourceText"]))
    return changed_cjk


def load_translations(request):
    translations = request.get("translations") or {}
    if request.get("translationsFile"):
        translations = json.loads(archive.read_file(request["translationsFile"], 128 << 20))
    if not isinstance(translations, dict) or any(not isinstance(text, str) for text in translations.values()):
        raise ValueError("译文必须为 ID 到文本的映射。")
    return translations


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


def load(source, files, packed, scratch, verify=False, include_tl=False):
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
        entries += report["entryCount"]
        if entries > archive.MAX_ENTRIES:
            raise ValueError("RPYC2 封包合计超过 256 MiB 脚本／10,000 成员预算。")
        compiled_names = {
            archive.member_name(entry["name"]).casefold()
            for entry in report["entries"]
            if entry["name"].lower().endswith(".rpyc")
        }
        lengths = {archive.member_name(entry["name"]).casefold(): entry["length"] for entry in report["entries"]}
        # Ren'Py skips archived .rpy/_ren.py/.rpym (dir is None) and only loads .rpyc.
        # Ignore paired .rpy/_ren.py alongside .rpyc and non-default tl/<lang>/ translations.
        unsupported = []
        for entry in report["entries"]:
            name = archive.member_name(entry["name"])
            if not include_tl and is_other_translation_script(name):
                continue
            lower = name.lower()
            suffix = Path(name).suffix.lower()
            if suffix == ".rpy" and (name + "c").casefold() in compiled_names:
                continue
            if lower.endswith("_ren.py") and (name[:-3] + ".rpyc").casefold() in compiled_names:
                continue
            if suffix in (".rpy", ".rpym", ".rpymc") or lower.endswith("_ren.py"):
                unsupported.append(entry["name"])
        if unsupported:
            raise ValueError("RPYC2 流程不能确定源码或模块的加载顺序：" + resource + " → " + ", ".join(unsupported[:8]))
        for member in sorted(report["extractedFiles"]):
            if not include_tl and is_other_translation_script(member):
                continue
            key = member.casefold()
            selected_bytes += lengths.get(key, 0)
            if selected_bytes > archive.MAX_TOTAL:
                raise ValueError("RPYC2 封包合计超过 256 MiB 脚本／10,000 成员预算。")
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


def extract(request, files, scripts, packed):
    overrides = {}
    if packed and request.get("buildMode") == "overlay":
        valid = {overlay_path(resource, member).casefold() for resource, member, _ in scripts}
        font_path = renpy_font.fallback_path(files)
        overrides = overlay_files_map(request, valid, optional_extras=[font_path] if font_path else [])
    result = []
    for resource, member, path in scripts:
        effective = overrides.get(overlay_path(resource, member).casefold(), path) if packed else path
        _, _, rows, manifest = read_script(effective, member)
        result.extend(units(resource, member, rows, manifest))
    return dict(extraction=dict(units=result, warnings=[WARNING]), validationContextVersion=1)


def build_script(resource, member, path, translations):
    script, names, rows, manifest = read_script(path, member)
    matched, changed_texts = set(), []
    for row, record in zip(rows, manifest["records"]):
        unit_id, _ = identity(resource, member, record["locator"])
        if unit_id in translations:
            matched.add(unit_id)
            if translations[unit_id] != row["message"]:
                row["message"] = translations[unit_id]
                changed_texts.append(row["message"])
    if not changed_texts:
        return script.raw, script.raw, matched, False
    patched = rpyc.patch(script, rows, manifest, filename=member, names=names)
    return patched, script.raw, matched, renpy_font.has_cjk_text(changed_texts)


def validate_script(before, after, member):
    original, names, original_rows, manifest = read_script(before, member)
    modified = rpyc.parse(archive.read_file(after, rpyc.MAX_FILE))
    rows, _ = rpyc.export(modified, filename=member, names=names)
    # Recreate only permitted text changes, preserving the complete original graph.
    if rpyc.patch(original, rows, manifest, filename=member, names=names) != modified.raw:
        raise ValueError("RPYC2 补丁改变了文本之外的内容：" + member)
    return renpy_font.has_cjk_text(row["message"] for old, row in zip(original_rows, rows)
                                   if old["message"] != row["message"])


def build(request, source, files, packed, build_mode, scripts, reports, scratch, translations, fallback):
    remaining = set(translations)
    output = archive.checked_path(request["outputDirectory"])
    archive.separate(archive.checked_path(source), output)
    overlay = build_mode == "overlay"
    written = [] if overlay else list(files)
    changed_cjk = False
    replacements = {resource: {} for resource in files}
    for index, (resource, member, path) in enumerate(scripts):
        patched, original, matched, cjk = build_script(resource, member, path, translations)
        changed_cjk |= cjk
        remaining.difference_update(matched)
        if not packed:
            target = resolve(output, resource)
        elif patched == original:
            continue
        elif overlay:
            relative = overlay_path(resource, member)
            written.append(relative)
            target = resolve(output, relative)
        else:
            target = scratch / ("changed-" + str(index))
            raw_name = next(entry["name"] for entry in reports[resource]["entries"]
                            if archive.member_name(entry["name"]) == member)
            replacements[resource][raw_name] = str(target)
        target.parent.mkdir(parents=True, exist_ok=True)
        with target.open("xb") as stream:
            stream.write(patched)
    if remaining:
        raise ValueError("部分译文无法对应当前 RPYC2 脚本，请重新提取：" + ", ".join(sorted(remaining)[:3]))
    if packed and not overlay:
        for resource in files:
            target = resolve(output, resource)
            if replacements[resource]:
                archive.execute(dict(protocolVersion=1, component="archive.rpa3", operation="rebuild",
                                     archive=str(resolve(source, resource)), outputFile=str(target),
                                     replacements=replacements[resource]))
            else:
                target.parent.mkdir(parents=True, exist_ok=True)
                with resolve(source, resource).open("rb") as original, target.open("xb") as destination:
                    shutil.copyfileobj(original, destination, 8 << 20)
    written.extend(fallback.write(output, changed_cjk))
    return dict(build=dict(mode=build_mode, files=sorted(written), warnings=[WARNING]))


def validate(request, source, files, packed, scripts, scratch, fallback, include_tl=False):
    changed_cjk = False
    if packed and request.get("buildMode") == "overlay":
        by_overlay = {overlay_path(resource, member).casefold(): (member, path) for resource, member, path in scripts}
        font_rel = fallback.relative
        extras = [font_rel] if font_rel is not None else ()
        overrides = overlay_files_map(request, by_overlay, optional_extras=extras)
        actual_disk = disk_files(request["outputDirectory"])
        if actual_disk != set(overrides):
            raise ValueError("overlay 输出目录文件列表与声明不一致。")
        for key, after in overrides.items():
            if font_rel is not None and key == font_rel.casefold():
                continue
            member, before = by_overlay[key]
            changed_cjk |= validate_script(before, after, member)
        fallback.validate(request, changed_cjk)
        check_validation_context(request)
        return dict(validation=dict(passed=True, issues=[], runtimeStatus="not-tested"))

    after_scripts, _ = load(request["outputDirectory"], files, packed, scratch / "patched", verify=False, include_tl=include_tl)
    if [(r, m) for r, m, _ in scripts] != [(r, m) for r, m, _ in after_scripts]:
        raise ValueError("补丁改变了 RPYC2 脚本列表。")
    if packed:
        selected_by_archive = {resource: set() for resource in files}
        for resource, member, _ in scripts:
            selected_by_archive[resource].add(archive.member_name(member))
        for resource in files:
            archive.check_rpa_replacement(
                resolve(source, resource),
                resolve(request["outputDirectory"], resource),
                selected_by_archive[resource],
            )
    for (_, member, before), (_, _, after) in zip(scripts, after_scripts):
        changed_cjk |= validate_script(before, after, member)
    fallback.validate(request, changed_cjk)
    check_validation_context(request)
    return dict(validation=dict(passed=True, issues=[], runtimeStatus="not-tested"))


def execute(request):
    if request.get("protocolVersion") != 1:
        raise ValueError("不支持的适配协议。")
    config = json.loads(archive.read_file(resolve(request["adapterDirectory"], "tools/adapter-template.json"), 1 << 20))
    packed = config["templateId"] == "rpa3-renpy-rpyc"
    build_mode = config.get("buildMode") or ("overlay" if packed else "replace")
    include_tl = bool(config.get("includeTranslationLanguages", False))
    if (config.get("schemaVersion") != 1 or config.get("templateVersion") != 1 or config.get("format") != "Renpy"
            or config.get("templateId") not in ("loose-renpy-rpyc", "rpa3-renpy-rpyc")
            or config.get("scriptExtensions") != [".rpyc"]
            or config.get("archiveComponent") != ("archive.rpa3" if packed else None)
            or build_mode not in ("overlay", "replace")
            or (build_mode == "overlay" and not packed)):
        raise ValueError("RPYC2 模板配置不匹配。")
    files = sorted(request["resourceFiles"])
    if packed and request["operation"] == "extract":
        files = renpy_font.extraction_inputs(request["sourceDirectory"], files)
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
    if operation == "build":
        translations = load_translations(request)
        if packed and build_mode == "overlay" and not translations:
            archive.separate(archive.checked_path(source), archive.checked_path(request["outputDirectory"]))
            return dict(build=dict(mode="overlay", files=[], warnings=[WARNING]))
    with tempfile.TemporaryDirectory(prefix="rpyc-", dir=Path.cwd()) as directory:
        scratch = Path(directory)
        scripts, reports = load(source, files, packed, scratch / "original", verify=False, include_tl=include_tl)
        if operation == "extract":
            return extract(request, files, scripts, packed)
        paths = ((overlay_path(resource, member) if packed else resource, path) for resource, member, path in scripts)
        fallback = renpy_font.FontFallback(source, files, paths, config)
        if operation == "build":
            return build(request, source, files, packed, build_mode, scripts, reports, scratch, translations, fallback)
        return validate(request, source, files, packed, scripts, scratch, fallback, include_tl=include_tl)


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
