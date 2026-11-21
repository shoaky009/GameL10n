"""Adapter protocol for loose SWF movies with AVM1 constant-pool text."""
import hashlib
import json
from pathlib import Path, PurePosixPath
import struct
import sys
import zlib

from .flash_avm1 import MAX_MOVIE, Movie
from .gamel10n_host import write_response


TEMPLATE = "flash-avm1-pool"
WARNING = "覆盖顶层 AVM1 常量池候选文本。嵌套影片、独立界面及游戏内中文显示需要单独核对。"
FORMAT_ERRORS = (ValueError, struct.error, zlib.error)


def probe(root, files, request):
    lengths = {v["path"]: v["length"] for v in request.get("probeFiles", [])}
    ranges = {(v["path"], v["offset"]): Path(v["dataPath"]) for v in request.get("probeRanges", [])}
    reads, selected, skipped = [], [], []
    fingerprints = {}
    count = size = 0
    for name in files:
        path = root / name
        length = lengths.get(name, path.stat().st_size)
        try:
            if length > MAX_MOVIE + 8:
                raise ValueError("movie exceeds local inspection limit")
            with path.open("rb") as stream:
                header = stream.read(8)
                if len(header) < 8 or header[:3] not in (b"FWS", b"CWS") or header[3] < 8:
                    raise ValueError("unsupported SWF signature or version")
                if int.from_bytes(header[4:], "little") > MAX_MOVIE + 8:
                    raise ValueError("declared movie exceeds local inspection limit")
                data = header + stream.read()
            if len(data) != length:
                offset = len(data)
                if length - offset > 8 << 20:
                    raise ValueError("movie exceeds bounded probe read budget")
                chunks = [data]
                while offset < length:
                    n = min(length - offset, 1 << 20)
                    part = ranges.get((name, offset))
                    if part is None:
                        reads.append(dict(path=name, offset=offset, length=n))
                    else:
                        chunks.append(part.read_bytes())
                    offset += n
                if sum(map(len, chunks)) != length:
                    continue
                data = b"".join(chunks)
            movie = Movie(data)
            fields = sum(1 for _ in movie.fields())
            if fields:
                selected.append(name)
                count += fields
                size += length
                fingerprints[name] = hashlib.sha256(data).hexdigest()
        except FORMAT_ERRORS as error:
            skipped.append(dict(path=name, reason=str(error)))
    if reads:
        return dict(probeReads=reads)
    warnings = [WARNING]
    if skipped:
        warnings.append(f"{len(skipped)} 个 SWF 超出当前检查范围，首项 {skipped[0]}")
    result = dict(inspectedMovies=len(files), selectedMovies=len(selected), textUnits=count,
                  skippedMovieCount=len(skipped), skippedMovies=skipped[:12], selectedBytes=size,
                  resourceFingerprints=fingerprints)
    if not selected:
        return dict(result, probe=None, diagnostic="未发现受支持的 AVM1 常量池文本。" + " ".join(warnings))
    return dict(result, probe=dict(engineId="flash-avm1-swf", adapterId="template." + TEMPLATE,
                adapterVersion="1", requiredFiles=selected, warnings=warnings,
                description="Flash AVM1 常量池文本及 Push 引用"))


def execute(request):
    root = Path(request["sourceDirectory"])
    files = request.get("resourceFiles") or []
    for name in files:
        path = PurePosixPath(name)
        if (not path.parts or path.is_absolute() or ".." in path.parts or "\\" in name
                or ":" in name or path.suffix.lower() != ".swf"):
            raise ValueError(f"invalid SWF resource path {name}")
    op = request["operation"]
    if op == "probe":
        return probe(root, files, request)
    if op not in ("extract", "build", "validate"):
        raise ValueError("unknown SWF adapter operation")
    translations = request.get("translations") or {}
    if request.get("translationsFile"):
        translations = json.loads(Path(request["translationsFile"]).read_text(encoding="utf-8-sig"))
    if not isinstance(translations, dict) or any(not isinstance(v, str) for v in translations.values()):
        raise ValueError("translations must map unit IDs to strings")
    units, used = [], set()
    for name in files:
        try:
            movie = Movie((root / name).read_bytes())
            edits = {}
            for locator, text, kind in movie.fields():
                uid = f"{name}#{locator}"
                if op == "extract":
                    context = {"system": "系统提示", "name": "角色名"}.get(kind, "内心独白" if text.startswith("（") else "剧情文本")
                    units.append(dict(id=uid, resourcePath=name, locator=locator, sourceText=text,
                                      kind=kind, context=context))
                elif uid in translations:
                    edits[locator] = translations[uid]
                    used.add(uid)
            if op == "build":
                target = Path(request["outputDirectory"]) / name
                data = movie.build(edits)
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(data)
            elif op == "validate":
                patched = Movie((Path(request["outputDirectory"]) / name).read_bytes())
                movie.validate(patched, edits)
        except FORMAT_ERRORS as error:
            raise ValueError(f"{name}: {error}") from error
    if op == "extract":
        return dict(extraction=dict(units=units, warnings=[WARNING]))
    if translations.keys() - used:
        raise ValueError("translation IDs outside selected AVM1 resources")
    if op == "build":
        return dict(build=dict(files=files, mode="replace", warnings=[WARNING]))
    return dict(validation=dict(passed=True, issues=[], runtimeStatus="not-tested"))


def main():
    request = json.load(sys.stdin)
    try:
        response = dict(protocolVersion=1, success=True, **execute(request))
    except (OSError, *FORMAT_ERRORS) as error:
        response = dict(protocolVersion=1, success=False,
                        errorKind="io" if isinstance(error, OSError) else "incompatible", error=str(error))
    write_response(request, response)
    return 0 if response["success"] else 1
