"""ImoScripter and Library/Avg text with byte-preserving encodings and tags."""
from dataclasses import dataclass, replace
import re

from python.engines import qlie as qlie_legacy, qlie_imos


UTF16_LE = b"\xff\xfe"
UTF16_BE = b"\xfe\xff"
AVG_SCENARIO = "avg-scenario"
RUBY = re.compile(r"\[rb,([^,\[\]\r\n]+),([^,\[\]\r\n]+)\]")
SCENARIO_TAG = re.compile(r"\[(?!n\])[^\[\]]+\]")
RUBY_OPEN, RUBY_CLOSE = "\ufdd0", "\ufdd1"
INDENT = "\ufdd2"
PC_TAIL = re.compile(r"^([ \t　]*(?:\[spd,0\])?\[pc,)([^\[\]\r\n]*)\]([、。！？…]+)(?=\r?$)", re.M)
INDEX_ASSIGNMENT = re.compile(r"[A-Za-z_]\w*\[[0-9]+\]=[A-Za-z_]\w*", re.ASCII)


@dataclass(frozen=True)
class ScenarioRecord:
    field: object

    @property
    def parts(self):
        return (self.field,)

    @property
    def speaker(self):
        return self.field if self.field.kind == "name" else None

    def row(self):
        if self.field.kind == "name":
            return {"name": self.field.raw.split("＠")[-1]}
        return {"message": self.field.text}


def decode(raw, *, unicode_scripts=False):
    bom = b""
    encoding = "cp932"
    if unicode_scripts and raw.startswith((UTF16_LE, UTF16_BE)):
        bom = raw[:2]
        encoding = "utf-16-le" if bom == UTF16_LE else "utf-16-be"
    script = raw[len(bom):].decode(encoding)
    if bom + script.encode(encoding) != raw:
        raise ValueError("QLIE 脚本编码无法原字节往返")
    return script, encoding, bom


def mask_ruby(text):
    if RUBY_OPEN in text or RUBY_CLOSE in text:
        raise ValueError("ImoScripter 脚本文字包含保留码位")
    # Single-character replacements leave the parser's source offsets intact.
    return RUBY.sub(lambda tag: RUBY_OPEN + tag[0][1:-1] + RUBY_CLOSE, text)


def assignment_helper(script):
    lines = [line.strip() for line in script.splitlines() if line.strip()]
    if not lines or not lines[0].startswith("@@!"):
        return False
    assignments = [line for line in lines if not line.startswith(("@", "\\"))]
    return bool(assignments) and all(INDEX_ASSIGNMENT.fullmatch(line) for line in assignments)


def scan(script, *, unicode_scripts=False, dialect="imos"):
    if dialect == AVG_SCENARIO:
        return tuple(ScenarioRecord(field) for field in qlie_legacy.qlie_fields(script))
    if not unicode_scripts:
        return qlie_imos.scan(script)
    if assignment_helper(script):
        return ()
    script = PC_TAIL.sub(r"\1\2\3]", script)
    masked = mask_ruby(script)
    records = qlie_imos.scan(masked)
    if masked == script:
        return records
    return tuple(replace(record, parts=tuple(
        replace(part, raw=script[part.start:part.end]) for part in record.parts))
        for record in records)


def _patch_scenario(script, rows):
    records = scan(script, dialect=AVG_SCENARIO)
    if not isinstance(rows, list) or len(rows) != len(records):
        raise ValueError("QLIE 剧情脚本字段数量发生变化")
    edits = {}
    for index, (record, row) in enumerate(zip(records, rows, strict=True)):
        original = record.row()
        if not isinstance(row, dict) or set(row) != set(original) or not all(isinstance(v, str) for v in row.values()):
            raise ValueError("QLIE 剧情脚本字段结构发生变化")
        key = next(iter(original))
        value = row[key]
        if value == original[key]:
            continue
        if key == "name":
            raw_name = record.field.raw
            if raw_name.startswith("$") or not value or any(c in value for c in "＠,【】\n\r\0[]"):
                raise ValueError("QLIE 剧情脚本人名字段无效")
            # Preserve the original voice/character identity before ＠ for Misc_NameDivide.
            edits[index] = raw_name.split("＠")[0] + "＠" + value
        else:
            edits[index] = value
    if not edits:
        return script
    result = qlie_legacy.patch_qlie(script, edits)
    rebuilt = scan(result, dialect=AVG_SCENARIO)
    if [record.row() for record in rebuilt] != rows:
        raise ValueError("QLIE 剧情脚本回填后字段文字发生变化")
    return result


def patch(script, rows, *, unicode_scripts=False, dialect="imos"):
    if dialect == AVG_SCENARIO:
        return _patch_scenario(script, rows)
    if not unicode_scripts:
        return qlie_imos.patch(script, rows)
    if not rows and assignment_helper(script):
        return script
    records = scan(script, unicode_scripts=True)
    original = [record.row() for record in records]
    if original == rows:
        return script
    if INDENT in script or any(INDENT in text for row in rows for text in row.values()):
        raise ValueError("QLIE 脚本文字包含保留的缩进码位")
    masked_rows = []
    for before, after in zip(original, rows, strict=True):
        if any(sorted(RUBY.findall(before[field])) != sorted(RUBY.findall(after[field])) for field in before):
            raise ValueError("QLIE 译文的注音标签发生变化")
        masked = {field: mask_ruby(text) for field, text in after.items()}
        # One full-width space determines the line role. Extra indentation belongs to the visible text.
        masked["message"] = re.sub(r"^　+", lambda m: INDENT * len(m[0]), masked["message"], flags=re.M)
        masked_rows.append(masked)
    normalized = PC_TAIL.sub(r"\1\2\3]", script)
    result = qlie_imos.patch(mask_ruby(normalized), masked_rows)
    result = result.replace(RUBY_OPEN, "[").replace(RUBY_CLOSE, "]")
    if normalized == script and INDENT not in result:
        return result
    result = result.replace(INDENT, "　")
    if normalized != script:
        lines = result.splitlines(keepends=True)
        for i, line in enumerate(script.splitlines(keepends=True)):
            tail = PC_TAIL.match(line)
            if tail and lines[i] == PC_TAIL.sub(r"\1\2\3]", line):
                lines[i] = line
            elif tail:
                lines[i] = re.sub(r"([、。！？…]+)\](?=\r?$)", r"]\1", lines[i])
        result = "".join(lines)
    rebuilt = scan(result, unicode_scripts=True)
    if [record.row() for record in rebuilt] != rows or (
        [[p.kind for p in r.parts] for r in rebuilt] != [[p.kind for p in r.parts] for r in records]
    ):
        raise ValueError("QLIE 回填后脚本文字或行类型发生变化")
    return result
