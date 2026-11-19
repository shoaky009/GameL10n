"""Versioned archive entry point over the pinned, shared engine library.

Only standard-library dependencies are used. Safe index/format decoding stays
in python.archives; this file supplies bounded file I/O, rebuilds and IPC.
"""
import hashlib
import importlib
import json
import os
from pathlib import Path
import pickle
import stat
import sys
import unicodedata
import zlib

VERSION = 1
MAX_ARCHIVE = 512 << 20
MAX_FILE = 64 << 20
MAX_TOTAL = 256 << 20
MAX_ENTRIES = 10_000
RPA_MAX_ARCHIVE = 2 << 30
MAX_INDEX = 8 << 20
INDEXED_COMPONENTS = ("archive.bgi", "archive.kif", "archive.ypf482", "archive.pfs8")


class ArchiveInput:
    """Count actual source reads, including the shared index parser's reads."""
    def __init__(self, stream):
        self.stream, self.bytes_read = stream, 0

    def read(self, size=-1):
        data = self.stream.read(size)
        self.bytes_read += len(data)
        return data

    def seek(self, offset, whence=0):
        return self.stream.seek(offset, whence)

    def tell(self):
        return self.stream.tell()


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
    if component == "archive.xp3":
        if filter_name != "none":
            raise ValueError("XP3 requires an explicitly established filter=none; zero flags are not proof")
        module = importlib.import_module("python.archives.xp3")
        entries = module.extract(data, filter_name="none", **limits)
    else:
        raise ValueError("unknown archive component")
    validate_names(entries)
    return module, entries


def select_entries(request, entries):
    members = request.get("members")
    extensions = request.get("extensions")
    if members is not None and (not isinstance(members, list) or not members or len(members) > MAX_ENTRIES):
        raise ValueError("members must be a nonempty bounded list")
    if extensions is not None and (not isinstance(extensions, list) or not extensions
            or len(extensions) > 16 or any(not isinstance(x, str) or len(x) > 32 or "/" in x or "\\" in x for x in extensions)):
        raise ValueError("invalid member extension selection")
    if members is not None and set(members) - {entry.name for entry in entries}:
        raise ValueError("selected archive member does not exist")
    return [entry for entry in entries if (members is None or entry.name in members)
            and (extensions is None or Path(member_name(entry.name)).suffix.lower() in extensions)]


def chunks(stream, offset, length):
    stream.seek(offset)
    while length:
        data = stream.read(min(length, 1 << 20))
        if not data:
            raise ValueError("truncated archive payload")
        length -= len(data)
        yield data


def hash_range(stream, offset, length, prefix=b""):
    result = hashlib.sha256(prefix)
    for data in chunks(stream, offset, length):
        result.update(data)
    return result.hexdigest()


def copy_range(stream, output, offset, length):
    for data in chunks(stream, offset, length):
        output.write(data)


def open_index(component, stream):
    """Return the format revision, index entries, and a bounded member reader."""
    limits = dict(max_index_size=MAX_INDEX, max_entries=MAX_ENTRIES)
    if component == "archive.bgi":
        from python.archives import bgi
        index = bgi.read_index(stream, **limits)

        def read(entry, maximum):
            member = bgi.read_member(stream, index, entry, max_stored_size=maximum)
            return bgi.decode_member(member, max_output_size=maximum, max_symbols=maximum)

        version, entries = index.version, index.entries
    elif component == "archive.kif":
        from python.archives import catsystem2_int as kif
        if kif.probe_int(stream, **limits).encrypted:
            raise ValueError("encrypted KIF requires a confirmed filename scheme; use the pinned INT API with explicit parameters")
        index = kif.read_int(stream, **limits)

        def read(entry, maximum):
            return kif.read_member(stream, entry, max_member=maximum)

        version, entries = "plain", index.entries
    elif component == "archive.ypf482":
        from python.archives import yuris_482 as ypf
        _, entries, _ = ypf.read_index(stream, **limits)

        def read(entry, maximum):
            if entry.size > maximum:
                raise ValueError("YPF stored member exceeds the read budget")
            return ypf.decode_member(b"".join(chunks(stream, entry.offset, entry.size)),
                                     entry, max_output=maximum)

        version = 482
    elif component == "archive.pfs8":
        from python.archives import pfs
        index = pfs.read_index(stream, **limits)
        if index.version != 8:
            raise ValueError(f"unsupported PFS version: {index.version}")

        def read(entry, maximum):
            return pfs.read_member(stream, index, entry, max_stored_size=maximum).data

        version, entries = index.version, index.entries
    else:
        raise ValueError("unknown indexed archive component")
    validate_names(entries)
    return version, entries, read


def execute_indexed(request, source):
    operation, component = request["operation"], request["component"]
    if operation == "rebuild":
        raise ValueError("this archive component supports inspect and extract")
    verify = request.get("verifyContents", False)
    if not isinstance(verify, bool):
        raise ValueError("verifyContents must be a boolean")
    with source.open("rb") as raw:
        before = os.fstat(raw.fileno())
        if not stat.S_ISREG(before.st_mode):
            raise ValueError("component input must be a regular file")
        stream = ArchiveInput(raw)
        version, entries, read = open_index(component, stream)
        index_bytes = stream.bytes_read
        selected = select_entries(request, entries)
        hashes, extracted, decoded = {}, [], 0
        output = None
        if operation == "extract":
            output = checked_path(request["outputDirectory"])
            separate(source, output)
            if output.exists() and (not output.is_dir() or any(output.iterdir())):
                raise ValueError("extraction output must be a new or empty directory")
        selected_names = {entry.name for entry in selected}
        to_read = entries if verify else selected if output is not None else ()
        if sum(entry.size for entry in to_read) > MAX_TOTAL:
            raise ValueError("selected stored bytes exceed the read budget")
        for entry in to_read:
            data = read(entry, min(MAX_FILE, MAX_TOTAL - decoded))
            decoded += len(data)
            hashes[entry.name] = digest(data)
            if output is not None and entry.name in selected_names:
                name = member_name(entry.name)
                destination = checked_path(output / name)
                destination.parent.mkdir(parents=True, exist_ok=True)
                with destination.open("xb") as target:
                    target.write(data)
                extracted.append(name)
        after = os.fstat(raw.fileno())
        if (before.st_size, before.st_mtime_ns) != (after.st_size, after.st_mtime_ns):
            raise ValueError("archive changed during component operation")
    report = dict(protocolVersion=VERSION, success=True, component=component,
                  operation=operation, formatVersion=version, inputBytes=before.st_size,
                  indexBytes=index_bytes, readBytes=stream.bytes_read, decodedBytes=decoded,
                  entryCount=len(entries), selectedCount=len(selected),
                  selectedBytes=sum(entry.size for entry in selected),
                  entries=[dict(name=entry.name, length=entry.size, sha256=hashes.get(entry.name))
                           for entry in entries], verifiedEntries=len(hashes),
                  allContentsVerified=len(hashes) == len(entries))
    if output is not None:
        report["extractedFiles"] = extracted
    return report


def script_samples(format_name, source, output):
    component, extensions = {
        "Ethornell": ("archive.bgi", {".bgi", ""}),
        "CatSystem": ("archive.kif", {".cst"}),
        "Yuris": ("archive.ypf482", {".ybn"}),
    }[format_name]
    maximum = 2 << 20
    samples, errors = [], []
    with checked_path(source).open("rb") as raw:
        stream = ArchiveInput(raw)
        version, entries, read = open_index(component, stream)
        index_bytes = stream.bytes_read
        by_name = {member_name(entry.name).casefold(): entry for entry in entries}
        eligible = [entry for entry in entries if 0 < entry.size <= maximum
                    and Path(member_name(entry.name)).suffix.lower() in extensions
                    and Path(member_name(entry.name)).name.lower() != "ysc.ybn"]
        eligible.sort(key=lambda entry: (abs(entry.size - (64 << 10)), entry.name))
        for entry in eligible[:6 if format_name == "Ethornell" else 3]:
            name = member_name(entry.name)
            try:
                data = read(entry, maximum)
                if (format_name == "Ethornell" and Path(name).suffix.lower() != ".bgi"
                        and not data.startswith(b"BurikoCompiledScriptVer1.00\0")):
                    continue
                if format_name == "CatSystem" and not data.startswith(b"CatScene"):
                    continue
                members = {name: data}
                if format_name == "Yuris":
                    if not data.startswith(b"YSTB"):
                        continue
                    command_table = Path(name).with_name("ysc.ybn").as_posix()
                    companion = by_name.get(command_table.casefold())
                    if companion is None:
                        raise ValueError("YU-RIS command table ysc.ybn is missing next to the script")
                    members[command_table] = read(companion, maximum)
            except (ValueError, OSError) as error:
                errors.append(str(error)[:160])
                continue
            directory = output / str(len(samples))
            for relative, payload in members.items():
                destination = directory / relative
                destination.parent.mkdir(parents=True, exist_ok=True)
                with destination.open("xb") as target:
                    target.write(payload)
            samples.append(dict(member=entry.name, path=str(len(samples)), files=list(members)))
            if len(samples) == 3:
                break
    status = "sampled" if samples else "sample-unavailable" if errors else "no-script-member"
    return dict(status=status, version=version, indexBytes=index_bytes, samples=samples,
                memberBytesRead=stream.bytes_read - index_bytes, diagnostics=errors[:3])


def rpa_index(module, stream):
    # Media remains in the archive. Only selected scripts/replacements have the
    # smaller allocation budget; the shared reader still validates ALL ranges.
    index = module.read_index(stream, max_entries=MAX_ENTRIES, max_file_size=RPA_MAX_ARCHIVE,
                              max_total_size=RPA_MAX_ARCHIVE, max_archive_size=RPA_MAX_ARCHIVE)
    validate_names(index.entries)
    return index


def rpa_rebuild(request, source, stream, module, index):
    output = checked_path(request["outputFile"])
    separate(source, output)
    if output.exists():
        raise ValueError("rebuild output already exists")
    replacements = request.get("replacements")
    if replacements is None:
        replacements = {}
    if not isinstance(replacements, dict) or len(replacements) > MAX_ENTRIES:
        raise ValueError("invalid replacement mapping")
    by_name = {entry.name: entry for entry in index.entries}
    if replacements.keys() - by_name.keys():
        raise ValueError("unknown replacement member")
    changed, total = {}, 0
    for name, path in replacements.items():
        replacement = checked_path(path)
        separate(replacement, output)
        value = read_file(replacement, MAX_FILE)
        total += len(value)
        if total > MAX_TOTAL:
            raise ValueError("replacement budget exceeded")
        entry = by_name[name]
        original = hash_range(stream, entry.offset, entry.length - len(entry.prefix), entry.prefix)
        if digest(value) != original:
            changed[name] = value

    output.parent.mkdir(parents=True, exist_ok=True)
    created = False
    try:
        with output.open("xb") as target:
            created = True
            if not changed:
                copy_range(stream, target, 0, index.size)
            else:
                target.write(index.header)
                cursor, mapping = 34, {}
                for entry in sorted(index.entries, key=lambda item: item.offset):
                    copy_range(stream, target, cursor, entry.offset - cursor)
                    offset = target.tell()
                    if entry.name in changed:
                        value = changed[entry.name]
                        prefix = value[:min(len(entry.prefix), len(value))]
                        target.write(value[len(prefix):])
                        length = len(value)
                    else:
                        prefix, length = entry.prefix, entry.length
                        copy_range(stream, target, entry.offset, length - len(prefix))
                    chunk = (offset ^ index.key, length ^ index.key)
                    if len(entry.chunk) == 3:
                        chunk += (prefix.decode("latin-1") if isinstance(entry.chunk[2], str) else prefix,)
                    mapping[entry.raw_name] = [chunk]
                    cursor = entry.offset + entry.length - len(entry.prefix)
                    if target.tell() > RPA_MAX_ARCHIVE:
                        raise ValueError("rebuilt RPA size limit exceeded")
                copy_range(stream, target, cursor, index.offset - cursor)
                offset = target.tell()
                # Serialize only data reconstructed from the shared safe reader.
                ordered = {entry.raw_name: mapping[entry.raw_name] for entry in index.entries}
                packed = zlib.compress(pickle.dumps(ordered, protocol=3))
                if offset + len(packed) > RPA_MAX_ARCHIVE:
                    raise ValueError("rebuilt RPA size limit exceeded")
                target.write(packed)
                target.seek(8)
                target.write(f"{offset:016x}".encode("ascii"))

        hashes = {}
        with output.open("rb") as rebuilt:
            verified = rpa_index(module, rebuilt)
            after = {entry.name: entry for entry in verified.entries}
            if set(after) != set(by_name):
                raise ValueError("rebuilt RPA changed member names")
            for entry in index.entries:
                original = hash_range(stream, entry.offset, entry.length - len(entry.prefix), entry.prefix)
                hashes[entry.name] = original
                expected = digest(changed[entry.name]) if entry.name in changed else original
                actual = after[entry.name]
                if hash_range(rebuilt, actual.offset, actual.length - len(actual.prefix), actual.prefix) != expected:
                    raise ValueError("rebuilt RPA changed untouched members or lost replacements")
            output_hash = hash_range(rebuilt, 0, verified.size)
        input_hash = hash_range(stream, 0, index.size)
        if not changed and input_hash != output_hash:
            raise ValueError("unchanged RPA build changed archive bytes")
        return hashes, dict(inputSha256=input_hash, outputSha256=output_hash,
                            changedCount=len(changed), byteIdentical=input_hash == output_hash)
    except BaseException:
        if created:
            output.unlink(missing_ok=True)
        raise


def execute_rpa(request, source):
    module = importlib.import_module("python.archives.rpa")
    operation = request["operation"]
    verify = request.get("verifyContents", False)
    if not isinstance(verify, bool):
        raise ValueError("verifyContents must be a boolean")
    with source.open("rb") as raw:
        before = os.fstat(raw.fileno())
        if not stat.S_ISREG(before.st_mode):
            raise ValueError("component input must be a regular file")
        stream = ArchiveInput(raw)
        index = rpa_index(module, stream)
        selected = select_entries(request, index.entries)
        selected_bytes = sum(entry.length for entry in selected)
        if operation == "extract" and (any(entry.length > MAX_FILE for entry in selected) or selected_bytes > MAX_TOTAL):
            raise ValueError("selected member size limit exceeded (64 MiB/member, 256 MiB total)")
        hashes = {}
        report = dict(protocolVersion=VERSION, success=True, component="archive.rpa3", operation=operation,
                      inputSha256=None, inputBytes=index.size, indexBytes=34 + index.size - index.offset,
                      entryCount=len(index.entries), selectedCount=len(selected), selectedBytes=selected_bytes)
        if operation == "rebuild":
            hashes, rebuilt = rpa_rebuild(request, source, stream, module, index)
            report.update(rebuilt)
        elif operation == "extract":
            output = checked_path(request["outputDirectory"])
            separate(source, output)
            if output.exists() and (not output.is_dir() or any(output.iterdir())):
                raise ValueError("extraction output must be a new or empty directory")
            destinations = [(entry, checked_path(output / member_name(entry.name))) for entry in selected]
            for entry, path in destinations:
                data = module.read_entry(stream, entry)
                hashes[entry.name] = digest(data)
                path.parent.mkdir(parents=True, exist_ok=True)
                with path.open("xb") as target:
                    target.write(data)
            report["extractedFiles"] = [member_name(entry.name) for entry in selected]
        if verify:
            for entry in index.entries:
                if entry.name not in hashes:
                    hashes[entry.name] = hash_range(stream, entry.offset, entry.length - len(entry.prefix), entry.prefix)
        after = os.fstat(raw.fileno())
        if (before.st_size, before.st_mtime_ns) != (after.st_size, after.st_mtime_ns):
            raise ValueError("archive changed during component operation")
        report.update(entries=[dict(name=entry.name, length=entry.length, sha256=hashes.get(entry.name)) for entry in index.entries],
                      readBytes=stream.bytes_read, verifiedEntries=len(hashes), allContentsVerified=len(hashes) == len(index.entries))
        return report


def execute(request):
    if request.get("protocolVersion") != VERSION:
        raise ValueError("unsupported archive component protocol")
    component, operation = request["component"], request["operation"]
    if operation not in ("inspect", "extract", "rebuild"):
        raise ValueError("unsupported archive component operation")
    source = checked_path(request["archive"])
    if component == "archive.rpa3":
        return execute_rpa(request, source)
    if component in INDEXED_COMPONENTS:
        return execute_indexed(request, source)
    data = read_file(source, MAX_ARCHIVE)
    module, entries = load_archive(component, data, request.get("filter"))
    report = dict(protocolVersion=VERSION, success=True, component=component,
                  operation=operation, inputSha256=digest(data), inputBytes=len(data), entryCount=len(entries))
    selected = select_entries(request, entries)
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
