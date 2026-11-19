"""Artemis AST 2.0 display fields in PFS v8 volumes."""
from dataclasses import dataclass
import hashlib
import io
import json
from pathlib import Path
import re
import struct
import sys

from python.archives import pfs
from python.engines import artemis_ast as ast

from . import engine_archive as archive
from .gamel10n_host import validation_context, write_response

TEMPLATE_ID = "artemis-pfs8-ast"
MAX_VOLUMES = 16
MAX_PROBE_INDEX = 1 << 20
MAX_PROBE_TOTAL = 8 << 20
MAX_SCRIPT = 4 << 20
MAX_SCRIPTS = 64 << 20
VOLUME = re.compile(r"\.pfs(?:\.[0-9]{3,})?$", re.IGNORECASE)
ANCHOR = re.compile(r"\{AST:[0-9]+\}")
LUA_TOKEN = re.compile(r'"(?:\\[\s\S]|[^"\\])*"'
                       r"|'(?:\\[\s\S]|[^'\\])*'"
                       r"|--(?:\[(?P<comment>=*)\[)?|\[(?P<string>=*)\[")


class IndexPrefix(io.BytesIO):
    """Bounded probe bytes with the original volume's length for range checks."""
    def __init__(self, data, length):
        super().__init__(data)
        self.length = length

    def seek(self, offset, whence=0):
        if whence == 2:
            return super().seek(self.length + offset)
        return super().seek(offset, whence)


def volumes(files):
    names = [archive.member_name(file) for file in files]
    names = [name for name in names if VOLUME.search(name)]
    if len(names) > MAX_VOLUMES or len({name.casefold() for name in names}) != len(names):
        raise ValueError("PFS volume list is too large or has colliding paths")
    return names


def script_members(index):
    return [member for member in index.entries if member.name.lower().endswith(".ast")]


def volume_layout(indices):
    selected, members, total = [], set(), 0
    for relative, index in indices.items():
        scripts = script_members(index)
        if scripts:
            selected.append(relative)
        for member in scripts:
            name = archive.member_name(member.name).casefold()
            if name in members:
                raise ValueError(f"AST member appears in multiple PFS volumes: {member.name}")
            members.add(name)
            total += member.size
            if member.size > MAX_SCRIPT or total > MAX_SCRIPTS:
                raise ValueError("PFS scripts exceed the extraction budget")
    return selected


def probe(root, request, files):
    lengths = {info["path"]: info["length"] for info in request.get("probeFiles", [])}
    ranges = {(info["path"], info["offset"]): info for info in request.get("probeRanges", [])}
    indices, needed, total = {}, [], 0
    try:
        for relative in files:
            path = archive.checked_path(root / relative)
            with path.open("rb") as stream:
                if relative not in lengths:
                    index = archive.pfs8_index(stream)
                    total += index.index_end - 7
                    if index.index_end - 7 > MAX_PROBE_INDEX or total > MAX_PROBE_TOTAL:
                        return dict(probe=None, diagnostic="PFS index exceeds the probe budget")
                    indices[relative] = index
                    continue
                header = stream.read(7)
                if len(header) != 7 or not header.startswith(b"pf8"):
                    return dict(probe=None)
                size, = struct.unpack_from("<I", header, 3)
                total += size
                if not 4 <= size <= MAX_PROBE_INDEX or total > MAX_PROBE_TOTAL or size + 7 > lengths[relative]:
                    return dict(probe=None)
                data = stream.read(size)
            if len(data) != size:
                info = ranges.get((relative, 7))
                if info is None:
                    needed.append(dict(path=relative, offset=7, length=size))
                    continue
                with Path(info["dataPath"]).open("rb") as fragment:
                    data = fragment.read(size + 1)
                if len(data) != size:
                    return dict(probe=None)
            indices[relative] = archive.pfs8_index(IndexPrefix(header + data, lengths[relative]))
        selected = volume_layout(indices)
    except ValueError as error:
        return dict(probe=None, diagnostic=str(error)[:400])
    if needed:
        return dict(probeReads=needed)
    if not selected:
        return dict(probe=None, diagnostic="PFS volumes contain no AST scripts")
    scripts = [entry for index in indices.values() for entry in script_members(index)]
    return dict(indexFingerprints={name: index.index_sha1 for name, index in indices.items()},
                mediaOnlyArchives=len(indices) - len(selected),
                indexBytes=sum(index.index_end + 4 for index in indices.values()),
                scriptCount=len(scripts), scriptBytes=sum(entry.size for entry in scripts),
                probe=dict(engineId="artemis", adapterId=f"template.{TEMPLATE_ID}", adapterVersion="1",
                           description="PFS v8 中的 Artemis AST 日文显示字段",
                           requiredFiles=selected,
                           warnings=["脚本范围限日文 AST 2.0，其他脚本和游戏内显示需另行核对"]))


def mask_comments(text):
    """Remove Lua comments for parsing while retaining every character offset."""
    if "--" not in text:
        return text
    masked, position = [], 0
    while match := LUA_TOKEN.search(text, position):
        start, end = match.span()
        masked.append(text[position:start])
        comment = match[0].startswith("--")
        delimiter = match["comment"] if comment else match["string"]
        if delimiter is not None:
            closing = f"]{delimiter}]"
            close_at = text.find(closing, end)
            if close_at < 0:
                raise ValueError("unterminated Lua long bracket")
            end = close_at + len(closing)
        elif comment:
            ends = [at for newline in ("\r", "\n") if (at := text.find(newline, end)) >= 0]
            end = min(ends, default=len(text))
        content = text[start:end]
        masked.append(re.sub(r"[^\r\n]", " ", content) if comment else content)
        position = end
    masked.append(text[position:])
    return "".join(masked)


@dataclass
class Field:
    unit: dict
    groups: list
    anchors: list

    def edits(self, value):
        if value == self.unit["sourceText"]:
            return []
        if not value.strip() or "\0" in value or ANCHOR.findall(value) != self.anchors:
            raise ValueError(f"translation changed a required AST field or command anchor: {self.unit['locator']}")
        fragments = ANCHOR.split(value)
        edits = []
        for group, fragment in zip(self.groups, fragments):
            length = sum(len(literal.text) for literal in group)
            consumed, start = 0, 0
            for index, literal in enumerate(group):
                consumed += len(literal.text)
                end = len(fragment) * consumed // length if length else (
                    len(fragment) if index == len(group) - 1 else 0)
                text = fragment[start:end]
                if text != literal.text:
                    encoded = ast.escape_lua(text, allow_lua_escapes=True)
                    edits.append((literal.start, literal.end, f'"{encoded}"'))
                start = end
        return edits


def text_groups(box):
    groups = [[]]
    for item in box.items:
        if item.is_str():
            groups[-1].append(item)
        elif item.kind == "table" and groups[-1]:
            groups.append([])
    return [group for group in groups if group]


def field(relative, member, unit, slot, kind, groups):
    parts = ["".join(literal.text for literal in group) for group in groups]
    if not any(part.strip() for part in parts):
        return None
    if any(ANCHOR.search(part) for part in parts):
        raise ValueError("AST literal collides with a command anchor")
    anchors = [f"{{AST:{index}}}" for index in range(1, len(parts))]
    text = parts[0] + "".join(anchor + part for anchor, part in zip(anchors, parts[1:]))
    locator = json.dumps([member, unit.block, unit.kind, unit.index, slot],
                         ensure_ascii=False, separators=(",", ":"))
    identity = hashlib.sha256(f"{relative}\0{locator}".encode("utf-8")).hexdigest()
    return Field(dict(id=identity, resourcePath=relative, locator=locator, sourceText=text,
                      context=unit.block, kind=kind, protectedTokens=anchors or None), groups, anchors)


def script_fields(data, relative, member):
    original = data.decode("utf-8-sig")
    doc = ast.read_ast(mask_comments(original).encode("utf-8"), path=member, encoding="utf-8",
                       language="ja", lenient_escapes=True, max_bytes=MAX_SCRIPT)
    if doc.astver != "2.0":
        raise ValueError(f"unsupported AST version: {doc.astver}")
    blocks = {entry.key.text: entry.value for entry in doc.ast.pairs()}
    if len(blocks) != len(doc.ast.items):
        raise ValueError("duplicate Artemis AST block")
    fields = []
    for unit in doc.units:
        block = blocks[unit.block]
        if unit.kind == "message":
            box = block.get("text").get("ja").items[unit.index]
            if names := box.get("name"):
                for index, item in enumerate(names.items):
                    literal = item.value if item.kind == "pair" else item
                    fields.append(field(relative, member, unit, f"name{index}", "name", [[literal]]))
            fields.append(field(relative, member, unit, "text", "dialogue", text_groups(box)))
        else:
            literal = (block.get("select").get("ja").items[unit.index] if unit.kind == "choice"
                       else block.find_tag("savetitle").get("text"))
            fields.append(field(relative, member, unit, "text", unit.kind, [[literal]]))
    return original, [entry for entry in fields if entry is not None]


def patch_script(data, original, edits):
    if not edits:
        return data
    pieces, position = [], 0
    for start, end, replacement in sorted(edits):
        if start < position or end > len(original):
            raise ValueError("overlapping AST edits")
        pieces.extend((original[position:start], replacement))
        position = end
    pieces.append(original[position:])
    encoding = "utf-8-sig" if data.startswith(b"\xef\xbb\xbf") else "utf-8"
    result = "".join(pieces).encode(encoding)
    if len(result) > MAX_SCRIPT:
        raise ValueError("rebuilt AST exceeds the script budget")
    return result


def process(root, files, translations, output=None):
    units, indices, replacements, identities = [], {}, {}, set()
    changed_bytes = 0
    for relative in files:
        with archive.checked_path(root / relative).open("rb") as stream:
            index = archive.pfs8_index(stream)
            indices[relative] = index
            volume_layout(indices)
            changed = {}
            for member in script_members(index):
                if output is not None and not translations:
                    break
                data = pfs.read_member(stream, index, member, max_stored_size=MAX_SCRIPT).data
                try:
                    original, fields = script_fields(data, relative, member.name)
                    edits = []
                    for item in fields:
                        identity = item.unit["id"]
                        if identity in identities:
                            raise ValueError("duplicate AST field identity")
                        identities.add(identity)
                        if output is None:
                            units.append(item.unit)
                        elif identity in translations:
                            edits.extend(item.edits(translations[identity]))
                    if edits:
                        changed[member.name] = patch_script(data, original, edits)
                        changed_bytes += len(changed[member.name])
                        if changed_bytes > MAX_SCRIPTS:
                            raise ValueError("rebuilt AST scripts exceed the total script budget")
                except ValueError as error:
                    raise ValueError(f"{relative}: {member.name}: {error}") from error
            replacements[relative] = changed
    if output is not None:
        if translations.keys() - identities:
            raise ValueError("translation ID does not belong to the selected PFS scripts")
        for relative, changed in replacements.items():
            archive.rebuild_pfs8(root / relative, output / relative, changed)
    return units


def validate(request, root, output, files, translations):
    context = validation_context(request)
    original = context["originalUnits"] if context else process(root, files, {})
    modified = context["outputUnits"] if context else process(output, files, {})
    after = {unit["id"]: unit for unit in modified}
    issues, changed = [], {}
    if len(after) != len(original) or len(modified) != len(after):
        issues.append(dict(code="count", message="回填改变了 AST 文本数量"))
    for unit in original:
        identity = unit["id"]
        text = translations.get(identity, unit["sourceText"])
        expected = dict(unit, sourceText=text)
        actual = after.get(identity)
        if actual is None or any(actual.get(key) != value for key, value in expected.items()):
            if len(issues) < 12:
                issues.append(dict(code="text", unitId=identity, message="回填改变了文本或结构"))
        if text != unit["sourceText"]:
            member, *_ = json.loads(unit["locator"])
            changed.setdefault(unit["resourcePath"], set()).add(member)
    for relative in files:
        archive.check_pfs8_replacement(root / relative, output / relative, changed.get(relative, set()))
    return dict(validation=dict(passed=not issues, issues=issues, runtimeStatus="not-tested"))


def execute(request):
    operation = request["operation"]
    root = Path(request["sourceDirectory"])
    requested = request.get("resourceFiles") or []
    files = volumes(requested)
    if operation == "probe":
        return probe(root, request, files)
    if not files or len(files) != len(requested):
        raise ValueError("select only the PFS volumes required by probe")
    if operation == "extract":
        units = process(root, files, {})
        return dict(extraction=dict(units=units, warnings=[]), validationContextVersion=1)
    output = Path(request["outputDirectory"])
    translations = request.get("translations") or {}
    if request.get("translationsFile"):
        translations = json.loads(Path(request["translationsFile"]).read_text(encoding="utf-8-sig"))
    if not isinstance(translations, dict) or any(not isinstance(value, str) for value in translations.values()):
        raise ValueError("translations must map AST unit IDs to text")
    if operation == "build":
        process(root, files, translations, output)
        return dict(build=dict(files=files, mode="replace", warnings=[]))
    if operation == "validate":
        return validate(request, root, output, files, translations)
    raise ValueError("unsupported Artemis adapter operation")


def main():
    request = json.load(sys.stdin)
    try:
        response = dict(protocolVersion=1, success=True, **execute(request))
    except (ValueError, OSError) as error:
        kind = "io" if isinstance(error, OSError) else "incompatible"
        response = dict(protocolVersion=1, success=False, errorKind=kind,
                        error=f"Artemis PFS: {str(error)[:1000]}")
    write_response(request, response)
    return 0 if response["success"] else 1
