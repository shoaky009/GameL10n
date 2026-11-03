"""Versioned archive entry point over the pinned, shared engine library.

Only standard-library dependencies are used. Format algorithms and their
licenses remain in python.archives; this file supplies paths, budgets and IPC.
"""
import hashlib
import importlib
import json
import os
from pathlib import Path
import stat
import sys
import unicodedata

VERSION = 1
MAX_ARCHIVE = 512 << 20
MAX_FILE = 64 << 20
MAX_TOTAL = 256 << 20
MAX_ENTRIES = 10_000


def checked_path(value):
    path = Path(value).absolute()
    for part in (path, *path.parents):
        if part.is_symlink() or (part.exists() and getattr(part.lstat(), "st_file_attributes", 0) & 0x400):
            raise ValueError("links/reparse points are not component inputs or outputs")
    return path


def member_name(value):
    if not isinstance(value, str) or not value or len(value) > 4096:
        raise ValueError("invalid archive member name")
    name = value.replace("\\", "/")
    for segment in name.split("/"):
        device = segment.split(".")[0].upper()
        if (not segment or segment in (".", "..") or segment[-1] in " ."
                or any(unicodedata.category(c) == "Cc" or c in ':<>"|?*' for c in segment)
                or device in {"CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$", "CLOCK$"}
                or len(device) == 4 and device[:3] in ("COM", "LPT") and device[3] in "123456789"):
            raise ValueError("unsafe archive member name: " + value[:200])
    return name


def validate_names(entries):
    names, directories, total = set(), set(), 0
    for entry in entries:
        name = member_name(entry.name).casefold()
        total += len(entry.name.encode("utf-8"))
        if total > 2 << 20:
            raise ValueError("archive member-name budget exceeded")
        if name in names or name in directories:
            raise ValueError("colliding archive member paths")
        parent = name
        while "/" in parent:
            parent = parent.rsplit("/", 1)[0]
            if parent in names:
                raise ValueError("archive file/directory collision")
            directories.add(parent)
        names.add(name)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def separate(source, destination):
    if source == destination or source in destination.parents or destination in source.parents:
        raise ValueError("component inputs and outputs must be separate")


def read_file(path, maximum):
    path = checked_path(path)
    with path.open("rb") as stream:
        if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
            raise ValueError("component input must be a regular file")
        data = stream.read(maximum + 1)
    if len(data) > maximum:
        raise ValueError("component input size limit exceeded")
    return data


def load_archive(component, data, filter_name):
    limits = dict(max_entries=MAX_ENTRIES, max_file_size=MAX_FILE,
                  max_total_size=MAX_TOTAL, max_archive_size=MAX_ARCHIVE)
    if component == "archive.rpa3":
        module = importlib.import_module("python.archives.rpa")
        entries = module.extract(data, **limits)
    elif component == "archive.xp3":
        if filter_name != "none":
            raise ValueError("XP3 requires an explicitly established filter=none; zero flags are not proof")
        module = importlib.import_module("python.archives.xp3")
        entries = module.extract(data, filter_name="none", **limits)
    else:
        raise ValueError("unknown archive component")
    validate_names(entries)
    return module, entries


def execute(request):
    if request.get("protocolVersion") != VERSION:
        raise ValueError("unsupported archive component protocol")
    component, operation = request["component"], request["operation"]
    if operation not in ("inspect", "extract", "rebuild"):
        raise ValueError("unsupported archive component operation")
    source = checked_path(request["archive"])
    data = read_file(source, MAX_ARCHIVE)
    module, entries = load_archive(component, data, request.get("filter"))
    report = dict(protocolVersion=VERSION, success=True, component=component,
                  operation=operation, inputSha256=digest(data), entryCount=len(entries))
    members = request.get("members")
    extensions = request.get("extensions")
    if members is not None and (not isinstance(members, list) or not members or len(members) > MAX_ENTRIES):
        raise ValueError("members must be a nonempty bounded list")
    if extensions is not None and (not isinstance(extensions, list) or not extensions
            or len(extensions) > 16 or any(not isinstance(x, str) or len(x) > 32 or "/" in x or "\\" in x for x in extensions)):
        raise ValueError("invalid member extension selection")
    if members is not None and set(members) - {entry.name for entry in entries}:
        raise ValueError("selected archive member does not exist")
    selected = [entry for entry in entries if (members is None or entry.name in members)
                and (extensions is None or Path(member_name(entry.name)).suffix.lower() in extensions)]
    report["entries"] = [dict(name=e.name, length=len(e.data), sha256=digest(e.data)) for e in entries]
    report["selectedCount"] = len(selected)
    if operation == "extract":
        output = checked_path(request["outputDirectory"])
        separate(source, output)
        if output.exists() and (not output.is_dir() or any(output.iterdir())):
            raise ValueError("extraction output must be a new or empty directory")
        # Validate every name and destination before the first write, including unselected entries.
        destinations = [(e, checked_path(output / member_name(e.name))) for e in selected]
        for entry, path in destinations:
            path.parent.mkdir(parents=True, exist_ok=True)
            with path.open("xb") as stream:
                stream.write(entry.data)
        report["extractedFiles"] = [member_name(e.name) for e in selected]
    elif operation == "rebuild":
        output = checked_path(request["outputFile"])
        separate(source, output)
        if output.exists():
            raise ValueError("rebuild output already exists")
        replacements = request.get("replacements")
        if replacements is None:
            replacements = {}
        if not isinstance(replacements, dict) or len(replacements) > MAX_ENTRIES:
            raise ValueError("invalid replacement mapping")
        if replacements.keys() - {entry.name for entry in entries}:
            raise ValueError("unknown replacement member")
        values = {}
        total = 0
        for name, path in replacements.items():
            replacement = checked_path(path)
            separate(replacement, output)
            values[name] = read_file(replacement, MAX_FILE)
            total += len(values[name])
            if total > MAX_TOTAL:
                raise ValueError("replacement budget exceeded")
        originals = {entry.name: entry.data for entry in entries}
        changed = {name: value for name, value in values.items() if value != originals[name]}
        # XP3's writer normalizes layout; keep the original archive for an unchanged build.
        if not changed:
            rebuilt = data
        elif component == "archive.rpa3":
            rebuilt = module.rebuild(data, changed)
        else:
            rebuilt = module.build([(e.name, changed.get(e.name, e.data)) for e in entries], filter_name="none",
                                   max_entries=MAX_ENTRIES, max_total_size=MAX_TOTAL, max_archive_size=MAX_ARCHIVE)
        _, verified = load_archive(component, rebuilt, request.get("filter"))
        expected = {e.name: changed.get(e.name, e.data) for e in entries}
        if {e.name: e.data for e in verified} != expected:
            raise ValueError("rebuilt archive changed untouched members or lost replacements")
        output.parent.mkdir(parents=True, exist_ok=True)
        with output.open("xb") as stream:
            stream.write(rebuilt)
        report.update(outputSha256=digest(rebuilt), changedCount=len(changed), byteIdentical=rebuilt == data)
    return report


def main():
    request = json.load(sys.stdin)
    response = checked_path(request["responseFile"])
    separate(checked_path(request["archive"]), response)
    try:
        report = execute(request)
    except Exception as error:
        report = dict(protocolVersion=VERSION, success=False, errorType=type(error).__name__, error=str(error)[:2000])
    response.parent.mkdir(parents=True, exist_ok=True)
    with response.open("x", encoding="utf-8", newline="\n") as stream:
        json.dump(report, stream, ensure_ascii=False, separators=(",", ":"))
    return 0 if report["success"] else 1


if __name__ == "__main__":
    sys.exit(main())
