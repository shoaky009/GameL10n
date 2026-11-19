"""GameL10n adapter for ADVSCRIPT2 scripts in NEKOPACK4A volumes."""

import hashlib
import json
from pathlib import Path
import shutil
import sys

from python.archives import nekopack

from . import engine_archive as archive
from .gamel10n_host import write_response
from .nekosdk_script import MAX_SCRIPT, ScriptText


COMPONENT = "archive.nekopack4a"
TEMPLATE = "nekopack4a-advscript2"


def script_members(entries):
    return [entry for entry in entries if Path(entry.name).suffix.lower() == ".txt"]


def probe(root, request, files):
    lengths = {item["path"]: item["length"] for item in request.get("probeFiles", [])}
    ranges = {item["path"]: item for item in request.get("probeRanges", [])
              if item["offset"] == nekopack.HEADER_SIZE}
    fingerprints, reads, selected, names = {}, [], [], set()
    index_bytes = script_count = script_bytes = 0
    try:
        for relative in files:
            path = archive.checked_path(root / relative)
            length = lengths.get(relative)
            if length is None:
                length = path.stat().st_size
            with path.open("rb") as stream:
                header = stream.read(nekopack.HEADER_SIZE)
                size = archive.nekopack4a_index_size(header, length)
                index_bytes += size + len(header)
                if index_bytes > archive.MAX_INDEX:
                    raise ValueError("NEKOPACK indices exceed the probe budget")
                index_data = stream.read(size)
            if len(index_data) != size:
                if relative not in lengths:
                    raise ValueError("truncated NEKOPACK index")
                fragment = ranges.get(relative)
                if fragment is None:
                    reads.append(dict(path=relative, offset=len(header), length=size))
                    continue
                with Path(fragment["dataPath"]).open("rb") as stream:
                    index_data = stream.read(size + 1)
            entries = archive.nekopack4a_entries(index_data, length, size)
            archive.validate_names(entries)
            fingerprints[relative] = archive.digest(header + index_data)
            members = script_members(entries)
            if not members:
                continue
            script_names = {archive.member_name(entry.name).casefold() for entry in members}
            if script_names & names:
                raise ValueError("NEKOPACK script names occur in more than one volume")
            names.update(script_names)
            selected.append(relative)
            script_count += len(members)
            script_bytes += sum(entry.size for entry in members)
            if script_count > archive.MAX_ENTRIES or script_bytes > archive.MAX_TOTAL:
                raise ValueError("NEKOPACK scripts exceed the adapter budget")
    except (ValueError, OSError) as error:
        return dict(probe=None, diagnostic=str(error)[:400])
    if reads:
        return dict(probeReads=reads)
    if not selected:
        return dict(probe=None, diagnostic="No .txt members in NEKOPACK4A volumes")
    return dict(indexFingerprints=fingerprints, indexBytes=index_bytes, scriptCount=script_count,
                scriptBytes=script_bytes, mediaOnlyArchives=len(files) - len(selected),
                probe=dict(engineId="nekosdk", adapterId=f"template.{TEMPLATE}", adapterVersion="1",
                           description="NEKOPACK4A 中的 ADVSCRIPT2 对白与人名",
                           requiredFiles=selected,
                           warnings=["按 CP932 读写已识别字段，选项、界面和中文显示需另外确认"]))


def unit(relative, member, field):
    locator = json.dumps([member, field.path], ensure_ascii=False, separators=(",", ":"))
    return dict(id=hashlib.sha256(f"{relative}\0{locator}".encode("utf-8")).hexdigest(),
                resourcePath=relative, locator=locator, context=member,
                sourceText=field.text, kind=field.kind)


def scan(root, files, translations=None):
    units, replacements, used = [], {}, set()
    decoded = changed_bytes = 0
    for relative in files:
        with archive.checked_path(root / relative).open("rb") as stream:
            _, entries, read = archive.open_index(COMPONENT, stream)
            members = script_members(entries)
            if not members:
                raise ValueError(f"{relative}: no ADVSCRIPT2 members")
            changes = {}
            for entry in members:
                try:
                    data = read(entry, min(MAX_SCRIPT, archive.MAX_TOTAL - decoded))
                    decoded += len(data)
                    script = ScriptText(data)
                    edits = {}
                    for field in script.fields:
                        row = unit(relative, entry.name, field)
                        if translations is None:
                            units.append(row)
                        elif row["id"] in translations:
                            used.add(row["id"])
                            edits[field.path] = translations[row["id"]]
                    rebuilt = script.build(edits) if edits else data
                    if rebuilt is not data:
                        changes[entry.name] = rebuilt
                        changed_bytes += len(rebuilt)
                        if changed_bytes > archive.MAX_TOTAL:
                            raise ValueError("rebuilt scripts exceed the adapter budget")
                except ValueError as error:
                    raise ValueError(f"{relative}/{entry.name}: {error}") from error
            replacements[relative] = changes
    if translations is not None and translations.keys() - used:
        raise ValueError("translation ID does not belong to the selected ADVSCRIPT2 fields")
    return units, replacements


def rebuild(source, destination, changes):
    source, destination = archive.checked_path(source), archive.checked_path(destination)
    archive.separate(source, destination)
    destination.parent.mkdir(parents=True, exist_ok=True)
    if not changes:
        with source.open("rb") as original, destination.open("xb") as output:
            shutil.copyfileobj(original, output, 1 << 20)
        return
    with source.open("rb") as stream:
        _, entries, read = archive.open_index(COMPONENT, stream)
        members, budget = {}, archive.MAX_TOTAL
        for entry in entries:
            data = changes[entry.name] if entry.name in changes else read(entry, min(budget, archive.MAX_FILE))
            budget -= len(data)
            if budget < 0:
                raise ValueError("NEKOPACK rebuild exceeds the decoded size budget")
            members[entry.name] = data
    packed = nekopack.build_pak(members)
    with destination.open("xb") as output:
        output.write(packed)


def check_rebuild(source, destination, changes):
    with archive.checked_path(source).open("rb") as original, archive.checked_path(destination).open("rb") as patched:
        if not changes:
            length = original.seek(0, 2)
            if length != patched.seek(0, 2) or archive.hash_range(original, 0, length) != archive.hash_range(patched, 0, length):
                raise ValueError("unchanged NEKOPACK volume differs from the original")
            return
        _, before, read_before = archive.open_index(COMPONENT, original)
        _, after, read_after = archive.open_index(COMPONENT, patched)
        if [entry.name for entry in before] != [entry.name for entry in after]:
            raise ValueError("NEKOPACK member names or order changed")
        budget = archive.MAX_TOTAL
        for old, new in zip(before, after):
            maximum = min(budget, archive.MAX_FILE)
            expected = changes[old.name] if old.name in changes else read_before(old, maximum)
            if read_after(new, maximum) != expected:
                raise ValueError(f"NEKOPACK member differs from expected output: {old.name}")
            budget -= len(expected)


def execute(request):
    operation = request["operation"]
    root = Path(request["sourceDirectory"])
    files = request.get("resourceFiles") or []
    if (not isinstance(files, list) or len(files) > 64
            or any(not isinstance(path, str) or path != archive.member_name(path)
                              or Path(path).suffix.lower() != ".pak" for path in files)
            or len({path.casefold() for path in files}) != len(files)):
        raise ValueError("expected distinct relative NEKOPACK .pak files")
    if operation == "probe":
        return probe(root, request, files)
    if not files:
        raise ValueError("select the NEKOPACK volumes returned by probe")
    if operation == "extract":
        units, _ = scan(root, files)
        return dict(extraction=dict(units=units, warnings=[]))
    if operation not in ("build", "validate"):
        raise ValueError("unsupported ADVSCRIPT2 adapter operation")
    output = Path(request["outputDirectory"])
    translations = request.get("translations") or {}
    if request.get("translationsFile"):
        translations = json.loads(Path(request["translationsFile"]).read_text(encoding="utf-8-sig"))
    if not isinstance(translations, dict) or any(not isinstance(value, str) for value in translations.values()):
        raise ValueError("translations must map ADVSCRIPT2 IDs to text")
    _, changes = scan(root, files, translations) if translations else (None, dict.fromkeys(files, {}))
    if operation == "build":
        for relative, edits in changes.items():
            rebuild(root / relative, output / relative, edits)
        return dict(build=dict(files=files, mode="replace", warnings=[]))
    for relative, edits in changes.items():
        check_rebuild(root / relative, output / relative, edits)
    return dict(validation=dict(passed=True, issues=[], runtimeStatus="not-tested"))


def main():
    request = json.load(sys.stdin)
    try:
        response = dict(protocolVersion=1, success=True, **execute(request))
    except (ValueError, OSError) as error:
        kind = "io" if isinstance(error, OSError) else "incompatible"
        response = dict(protocolVersion=1, success=False, errorKind=kind, error=str(error)[:1000])
    write_response(request, response)
    return 0 if response["success"] else 1
