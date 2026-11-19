"""Versioned archive entry point over the pinned, shared engine library.

Only standard-library dependencies are used. Safe index/format decoding stays
in python.archives; this file supplies bounded file I/O, rebuilds and IPC.
"""
import hashlib
import importlib
from dataclasses import replace
import json
import os
from pathlib import Path
import pickle
import shutil
import stat
import struct
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
INDEXED_COMPONENTS = ("archive.bgi", "archive.kif", "archive.ypf482", "archive.pfs8", "archive.nekopack4a")


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
        index = pfs8_index(stream)

        def read(entry, maximum):
            return pfs.read_member(stream, index, entry, max_stored_size=maximum).data

        version, entries = index.version, index.entries
    elif component == "archive.nekopack4a":
        from python.archives import nekopack
        length = stream.seek(0, os.SEEK_END)
        stream.seek(0)
        header = stream.read(nekopack.HEADER_SIZE)
        index_size = nekopack4a_index_size(header, length)
        entries = nekopack4a_entries(stream.read(index_size), length, index_size)

        def read(entry, maximum):
            if entry.size > maximum:
                raise ValueError("NEKOPACK stored member exceeds the read budget")
            payload = b"".join(chunks(stream, entry.offset, entry.size))
            unpacked, = struct.unpack_from("<I", payload, len(payload) - 4)
            if unpacked > maximum:
                raise ValueError("NEKOPACK decoded member exceeds the read budget")
            return nekopack.read_member(payload, nekopack.PakEntry(entry.name, 0, entry.size))

        version = "4A"
    else:
        raise ValueError("unknown indexed archive component")
    if component != "archive.pfs8":
        validate_names(entries)
    return version, entries, read


def nekopack4a_index_size(header, length):
    from python.archives import nekopack
    if len(header) != nekopack.HEADER_SIZE or not header.startswith(b"NEKOPACK4A"):
        raise ValueError("expected a NEKOPACK4A header")
    size, = struct.unpack_from("<I", header, 10)
    if not 0 < size <= min(MAX_INDEX, length - len(header)):
        raise ValueError("NEKOPACK index exceeds the input or index budget")
    return size


def nekopack4a_entries(data, length, index_size):
    from python.archives import nekopack
    if len(data) != index_size:
        raise ValueError("truncated NEKOPACK index")
    entries, position = [], 0
    data_offset = nekopack.HEADER_SIZE + index_size
    while position < index_size:
        if position + 4 > index_size or len(entries) >= MAX_ENTRIES:
            raise ValueError("truncated NEKOPACK name length or too many members")
        size, = struct.unpack_from("<I", data, position)
        position += 4
        if size == 0 and not any(data[position:]):
            break
        end = position + size
        if not 1 < size <= nekopack.MAX_NAME or end + 8 > index_size:
            raise ValueError("invalid NEKOPACK name length")
        name = data[position:end]
        if name[-1] != 0:
            raise ValueError("NEKOPACK member name has no terminator")
        key = nekopack.name_checksum(name) & 0xFFFFFFFF
        offset, size = (value ^ key for value in struct.unpack_from("<II", data, end))
        if size < 8 or offset < data_offset or offset + size > length:
            raise ValueError("NEKOPACK member range exceeds the archive")
        entries.append(nekopack.PakEntry(name[:-1].decode("cp932"), offset, size))
        position = end + 8
    if not entries:
        raise ValueError("NEKOPACK archive has no entries")
    previous_end = data_offset
    for entry in sorted(entries, key=lambda entry: entry.offset):
        if entry.offset < previous_end:
            raise ValueError("overlapping NEKOPACK member ranges")
        previous_end = entry.offset + entry.size
    return entries


def pfs8_index(stream):
    from python.archives import pfs
    index = pfs.read_index(stream, max_entries=MAX_ENTRIES, max_index_size=MAX_INDEX)
    if index.version != 8:
        raise ValueError(f"unsupported PFS version: {index.version}")
    validate_names(index.entries)
    return index


def xor_pfs(data, key, offset=0):
    phase = offset % len(key)
    key = key[phase:] + key[:phase]
    mask = (key * ((len(data) + len(key) - 1) // len(key)))[:len(data)]
    return (int.from_bytes(data, "little") ^ int.from_bytes(mask, "little")).to_bytes(len(data), "little")


def rebuild_pfs8(source, destination, replacements):
    """Replace decoded payloads while preserving the index layout and streaming other members."""
    destination = checked_path(destination)
    source = checked_path(source)
    separate(source, destination)
    destination.parent.mkdir(parents=True, exist_ok=True)
    with source.open("rb") as stream:
        index = pfs8_index(stream)
        if replacements.keys() - {entry.name for entry in index.entries}:
            raise ValueError("unknown PFS replacement member")
        if not replacements:
            stream.seek(0)
            with destination.open("xb") as target:
                shutil.copyfileobj(stream, target, 1 << 20)
            return
        physical = sorted(index.entries, key=lambda entry: (entry.offset, entry.ordinal))
        old_end = index.index_end
        shift, records = 0, {}
        for entry in physical:
            if entry.offset < old_end:
                raise ValueError("overlapping PFS members cannot be rebuilt")
            old_end = entry.offset + entry.size
            size = len(replacements[entry.name]) if entry.name in replacements else entry.size
            records[entry.ordinal] = (entry.offset + shift, size)
            shift += size - entry.size
        if index.archive_size + shift > 0xFFFFFFFF:
            raise ValueError("rebuilt PFS exceeds the 32-bit address space")
        stream.seek(0)
        header = stream.read(7)
        table = bytearray(stream.read(index.index_end - 7))
        if hashlib.sha1(table, usedforsecurity=False).digest() != index.key:
            raise ValueError("PFS index changed during rebuild")
        position = 4
        for entry in index.entries:
            # Record header and opaque padding stay in their original byte positions.
            position += 4 + len(entry.name_bytes) + len(entry.padding)
            struct.pack_into("<II", table, position, *records[entry.ordinal])
            position += 8
        key = hashlib.sha1(table, usedforsecurity=False).digest()
        delta = bytes(left ^ right for left, right in zip(index.key, key))
        with destination.open("xb") as target:
            target.write(header)
            target.write(table)
            cursor = index.index_end
            for entry in physical:
                copy_range(stream, target, cursor, entry.offset - cursor)
                if entry.name in replacements:
                    target.write(xor_pfs(replacements[entry.name], key))
                elif key == index.key:
                    copy_range(stream, target, entry.offset, entry.size)
                else:
                    offset = 0
                    for stored in chunks(stream, entry.offset, entry.size):
                        target.write(xor_pfs(stored, delta, offset))
                        offset += len(stored)
                cursor = entry.offset + entry.size
            copy_range(stream, target, cursor, index.archive_size - cursor)


def check_pfs8_replacement(source, output, changed):
    """Check every unmodified member without materializing media in memory."""
    with source.open("rb") as original, output.open("rb") as modified:
        before, after = pfs8_index(original), pfs8_index(modified)
        if before.trailing != after.trailing or len(before.entries) != len(after.entries):
            raise ValueError("PFS index metadata changed")
        delta = bytes(left ^ right for left, right in zip(before.key, after.key))
        for old, new in zip(before.entries, after.entries):
            if (old.name_bytes, old.padding) != (new.name_bytes, new.padding):
                raise ValueError(f"PFS member identity changed: {old.name}")
            if old.name in changed:
                continue
            if old.size != new.size:
                raise ValueError(f"unmodified PFS member size changed: {old.name}")
            offset = 0
            for left, right in zip(chunks(original, old.offset, old.size), chunks(modified, new.offset, new.size)):
                if (left if before.key == after.key else xor_pfs(left, delta, offset)) != right:
                    raise ValueError(f"unmodified PFS member bytes changed: {old.name}")
                offset += len(left)


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
        "NekoSDK": ("archive.nekopack4a", {".txt"}),
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
                if format_name == "NekoSDK" and not data.startswith(b"NEKOSDK_ADVSCRIPT2\0"):
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


def whale_read_names(path):
    if path is None:
        return []
    names = json.loads(read_file(path, 2 << 20).decode("utf-8-sig"))
    if not isinstance(names, list) or len(names) > MAX_ENTRIES:
        raise ValueError("Whale filename manifest must be a bounded JSON list")
    for name in names:
        member_name(name)
    return names


def whale_rebuild(request, source, stream, whale, index, game_key):
    if index.missing or any(entry.kind != whale.SCRIPT for entry in index.entries):
        raise ValueError("Whale DAT rebuild requires a script-only v2 volume")
    if game_key is None:
        raise ValueError("Whale DAT rebuild requires gameTitle")
    if index.length > MAX_ARCHIVE:
        raise ValueError("Whale script archive exceeds the rebuild budget")
    output = checked_path(request["outputFile"])
    separate(source, output)
    by_name = {entry.name: entry for entry in index.entries}
    replacements = request.get("replacements") or {}
    if not isinstance(replacements, dict) or len(replacements) > 128 or replacements.keys() - by_name.keys():
        raise ValueError("invalid Whale script replacement mapping")
    changes, budget = {}, MAX_TOTAL
    for name, path in replacements.items():
        replacement = checked_path(path)
        separate(replacement, output)
        data = read_file(replacement, min(budget, MAX_FILE))
        budget -= len(data)
        if whale.read_member(stream, by_name[name], MAX_FILE, game_key) != data:
            changes[name] = data

    output.parent.mkdir(parents=True, exist_ok=True)
    created = False
    try:
        with output.open("x+b") as target:
            created = True
            if not changes:
                copy_range(stream, target, 0, index.length)
            else:
                target.write(bytes(index.data_start))
                before, updated = index.data_start, {}
                for entry in sorted(index.entries, key=lambda entry: entry.offset):
                    copy_range(stream, target, before, entry.offset - before)
                    offset = target.tell()
                    if entry.name in changes:
                        data = changes[entry.name]
                        stored = whale.encode_script(data, entry, game_key)
                        target.write(stored)
                        updated[entry.key] = replace(entry, offset=offset, size=len(stored), decoded_size=len(data))
                    else:
                        copy_range(stream, target, entry.offset, entry.size)
                        updated[entry.key] = replace(entry, offset=offset)
                    before = entry.offset + entry.size
                    if target.tell() > MAX_ARCHIVE:
                        raise ValueError("rebuilt Whale DAT exceeds the output budget")
                copy_range(stream, target, before, index.length - before)
                if target.tell() > MAX_ARCHIVE:
                    raise ValueError("rebuilt Whale DAT exceeds the output budget")
                whale.write_script_index(target, [updated[entry.key] for entry in index.entries])
                result = whale.read_index(target, (), MAX_ENTRIES)
                if [entry.key for entry in result.entries] != [entry.key for entry in index.entries]:
                    raise ValueError("Whale DAT member identities changed")
                before_by_hash = {entry.key: entry for entry in index.entries}
                for entry in result.entries:
                    old = before_by_hash[entry.key]
                    if old.name in changes:
                        if whale.read_member(target, entry, MAX_FILE, game_key) != changes[old.name]:
                            raise ValueError("rebuilt Whale script differs from its replacement")
                    elif (entry.decoded_size != old.decoded_size or entry.size != old.size
                          or hash_range(target, entry.offset, entry.size) != hash_range(stream, old.offset, old.size)):
                        raise ValueError("rebuilt Whale DAT changed an untouched member")
            return dict(changedCount=len(changes), byteIdentical=not changes)
    except BaseException:
        if created:
            output.unlink(missing_ok=True)
        raise


def execute_whale(request, source):
    package = f"{__package__}." if __package__ else ""
    whale = importlib.import_module(package + "whale_dat")
    names = whale_read_names(request.get("filenameManifest"))
    key = whale.title_key(request["gameTitle"]) if request.get("gameTitle") is not None else None
    operation = request["operation"]
    verify = request.get("verifyContents", False)
    if not isinstance(verify, bool):
        raise ValueError("verifyContents must be a boolean")
    with source.open("rb") as raw:
        before = os.fstat(raw.fileno())
        if not stat.S_ISREG(before.st_mode):
            raise ValueError("component input must be a regular file")
        stream = ArchiveInput(raw)
        index = whale.read_index(stream, names, MAX_ENTRIES)
        validate_names(index.entries)
        index_bytes = stream.bytes_read
        if verify and index.missing:
            raise ValueError("Whale DAT has unresolved filenames; supply a matching filename manifest")
        selected = select_entries(request, index.entries)
        report = dict(protocolVersion=VERSION, success=True, component="archive.whale-dat",
                      operation=operation, formatVersion=2, inputBytes=before.st_size, indexBytes=index_bytes,
                      entryCount=len(index.entries) + len(index.missing), resolvedCount=len(index.entries),
                      unresolvedCount=len(index.missing), selectedCount=len(selected),
                      selectedBytes=sum(entry.size for entry in selected))
        hashes, extracted = {}, {}
        total = 0
        if operation == "extract" or verify:
            to_read = index.entries if verify else selected
            if sum(entry.size for entry in to_read) > MAX_TOTAL:
                raise ValueError("Whale DAT stored members exceed the total read budget")
            selected_names = {entry.name for entry in selected}
            for entry in to_read:
                try:
                    data = whale.read_member(stream, entry, min(MAX_FILE, MAX_TOTAL - total), key)
                except zlib.error as error:
                    raise ValueError("Whale script zlib stream is invalid; confirm gameTitle and archive version") from error
                total += len(data)
                hashes[entry.name] = digest(data)
                if operation == "extract" and entry.name in selected_names:
                    extracted[entry] = data
        if operation == "rebuild":
            report.update(whale_rebuild(request, source, stream, whale, index, key))
        after = os.fstat(raw.fileno())
        if (before.st_size, before.st_mtime_ns) != (after.st_size, after.st_mtime_ns):
            raise ValueError("Whale DAT changed during component operation")
        report.update(readBytes=stream.bytes_read, decodedBytes=total, verifiedEntries=len(hashes),
                      allContentsVerified=len(hashes) == report["entryCount"],
                      entries=[dict(name=entry.name, length=entry.size, hash=f"{entry.key:016x}",
                                    sha256=hashes.get(entry.name)) for entry in index.entries])
    if operation == "extract":
        whale_script = importlib.import_module(package + "whale_script")
        output = checked_path(request["outputDirectory"])
        separate(source, output)
        if output.exists() and (not output.is_dir() or any(output.iterdir())):
            raise ValueError("extraction output must be a new or empty directory")
        assets, truncated = set(), False
        for entry, data in extracted.items():
            if entry.kind == whale.SCRIPT:
                try:
                    for name in sorted(whale_script.asset_names(data.decode("cp932"))):
                        try:
                            normalized = member_name(name)
                        except ValueError:
                            continue
                        if len(assets) < MAX_ENTRIES or normalized in assets:
                            assets.add(normalized)
                        else:
                            truncated = True
                except UnicodeDecodeError:
                    pass
        manifest = checked_path(output.parent / "filenames.json")
        separate(source, manifest)
        encoded_names = json.dumps(sorted(assets), ensure_ascii=False).encode("utf-8")
        if len(encoded_names) > 2 << 20:
            raise ValueError("Whale filename candidates exceed the manifest budget")
        if assets and manifest.exists():
            raise ValueError("Whale filename manifest already exists")
        report["extractedFiles"] = []
        for entry, data in extracted.items():
            name = member_name(entry.name)
            path = checked_path(output / name)
            path.parent.mkdir(parents=True, exist_ok=True)
            with path.open("xb") as target:
                target.write(data)
            report["extractedFiles"].append(name)
        report.update(filenameCandidates=len(assets), filenameCandidatesTruncated=truncated)
        if assets:
            with manifest.open("xb") as target:
                target.write(encoded_names)
            report["filenameManifest"] = manifest.name
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
    if component == "archive.whale-dat":
        return execute_whale(request, source)
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
