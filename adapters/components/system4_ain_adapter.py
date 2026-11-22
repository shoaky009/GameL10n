"""Adapter protocol for loose System4 AI2 scripts with MSG1 text."""
import json
from pathlib import Path, PurePosixPath
import sys
import zlib

from .gamel10n_host import write_response
from .system4_ain import Ain, inspect, prefix_version, read_file


TEMPLATE = "system4-ain-msg1"
WARNING = "覆盖 AIN 的 MSG1 对话表。STR0、EX、字库及游戏内显示需要按当前资源核对。"
RESOURCE_ERRORS = (ValueError, OSError, zlib.error)


def resource_names(request):
    names = request.get("resourceFiles") or [item["path"] for item in request.get("probeFiles", [])]
    for name in dict.fromkeys(names):
        path = PurePosixPath(name)
        if path.suffix.lower() != ".ain":
            continue
        if path.is_absolute() or ".." in path.parts or "\\" in name or ":" in name:
            raise ValueError(f"AIN 资源路径无效：{name}")
        yield name


def probe(root, names, request):
    lengths = {item["path"]: item["length"] for item in request.get("probeFiles", [])}
    selected = []
    profiles = []
    selected_bytes = 0
    for name in names:
        file = root / name
        try:
            length = lengths[name] if name in lengths else file.stat().st_size
            with file.open("rb") as stream:
                version = prefix_version(stream.read(65536), length)
            if name not in lengths:
                report = inspect(Ain(read_file(file)))
                profile = report["profile"]
                profiles.append(dict(path=name, **profile, parseStatus=report["parseStatus"],
                                     roundtripStatus=report["roundtripStatus"],
                                     translationStatus=report["translationStatus"]))
                if not profile["sourceEncoding"] or not report["fieldCount"]:
                    continue
            else:
                profiles.append(dict(path=name, ainVersion=version))
            selected.append(name)
            selected_bytes += length
        except RESOURCE_ERRORS as error:
            profiles.append(dict(path=name, error=str(error)))
    if len(selected) != len(names):
        return dict(probe=None, scripts=profiles,
                    diagnostic="部分 AIN 未通过结构或编码检查，需确认游戏加载的脚本和对应布局。")
    if not selected:
        return dict(probe=None, scripts=profiles, diagnostic="未发现可读取的 AIN MSG1 对话表。")
    return dict(scripts=profiles, selectedBytes=selected_bytes,
                probe=dict(engineId="system4", adapterId="template." + TEMPLATE, adapterVersion="1",
                           requiredFiles=selected, warnings=[WARNING], description="System4 AI2 / MSG1 对话"))


def messages(ain):
    choices = ain.decoded_messages()
    if choices:
        encoding, texts = next(iter(choices.items()))
        # A translated output can lose its encoding evidence when all messages become ASCII.
        if all(other == texts for other in choices.values()):
            return encoding, texts
    raise ValueError(f"MSG1 原文编码需要确认，可严格解码的候选为 {', '.join(choices) or '无'}。")


def execute(request):
    root = Path(request["sourceDirectory"])
    names = list(resource_names(request))
    op = request["operation"]
    if op == "probe":
        return probe(root, names, request)
    if op not in ("extract", "build", "validate") or not names:
        raise ValueError("AIN 操作或资源范围无效。")
    translations = request.get("translations") or {}
    if request.get("translationsFile"):
        translations = json.loads(Path(request["translationsFile"]).read_bytes())
    if not isinstance(translations, dict) or any(not isinstance(text, str) for text in translations.values()):
        raise ValueError("译文必须为 ID 到文本的映射。")
    units = []
    used = set()
    for name in names:
        ain = Ain(read_file(root / name))
        encoding, texts = messages(ain)
        changes = {}
        for index, source in enumerate(texts):
            if not source.strip() or source in ("【", "】"):
                continue
            locator = f"MSG1:slot:{index + 1}"
            unit_id = f"{name}:{locator}"
            if op == "extract":
                units.append(dict(id=unit_id, resourcePath=name, locator=locator,
                                  sourceText=source, context="MSG1 对话", kind="dialogue"))
            elif unit_id in translations:
                used.add(unit_id)
                text = translations[unit_id]
                if text == source:
                    continue
                if not text.strip() or text.endswith(" "):
                    raise ValueError(f"{unit_id} 的译文为空或含回读时会裁剪的末尾空格。")
                try:
                    changes[index] = text.encode(encoding)
                except UnicodeEncodeError as error:
                    raise ValueError(f"{unit_id} 的译文无法按 {encoding} 回填，需要确认游戏的中文显示路径。") from error
        if op == "build":
            target = Path(request["outputDirectory"]) / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(ain.rebuild(changes))
        elif op == "validate":
            actual = Ain(read_file(Path(request["outputDirectory"]) / name))
            expected = ain.revised_raw(changes)
            if actual.raw != expected or (expected is ain.raw and actual.data != ain.data):
                raise ValueError(f"{name} 的回填结果与预期布局或未修改字节不一致。")
    if op == "extract":
        return dict(extraction=dict(units=units, warnings=[WARNING]))
    if translations.keys() - used:
        raise ValueError("译文含当前 AIN 资源未提取的文本 ID。")
    if op == "build":
        return dict(build=dict(files=names, mode="replace", warnings=[WARNING]))
    return dict(validation=dict(passed=True, issues=[], runtimeStatus="not-tested"))


def main():
    request = json.load(sys.stdin)
    try:
        response = dict(protocolVersion=1, success=True, **execute(request))
    except RESOURCE_ERRORS as error:
        response = dict(protocolVersion=1, success=False,
                        errorKind="io" if isinstance(error, OSError) else "incompatible", error=str(error))
    write_response(request, response)
    return 0 if response["success"] else 1
