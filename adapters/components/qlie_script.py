"""ImoScripter and Library/Avg text with byte-preserving encodings and tags."""
from dataclasses import dataclass, replace
import re

from python.engines import qlie as qlie_legacy, qlie_imos


UTF16_LE = b"\xff\xfe"
UTF16_BE = b"\xfe\xff"
AVG_SCENARIO = "avg-scenario"
RUBY = re.compile(r"\[rb,([^,\[\]\r\n]+),([^,\[\]\r\n]+)\]")
SCENARIO_TAG = re.compile(r"\[(?!n\])[^\[\]]+\]")
RUBY_OPEN, RUBY_CLOSE, RUBY_COMMA = "\ufdd0", "\ufdd1", "\ufdd5"
INDENT = "\ufdd2"
NAME_JOIN = "\ufdd3\ufdd4"
MULTI_NAME = re.compile(r"^([ \t]*【)([^【】\r\n]+(?:】【[^【】\r\n]+)+)(】)(?=[ \t]*\r?$)", re.M)
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
    if any(code in text for code in (RUBY_OPEN, RUBY_CLOSE, RUBY_COMMA)):
        raise ValueError("ImoScripter 脚本文字包含保留码位")
    # Single-character replacements leave the parser's source offsets intact.
    return RUBY.sub(
        lambda tag: f"{RUBY_OPEN}rb{RUBY_COMMA}{tag[1]}{RUBY_COMMA}{tag[2]}{RUBY_CLOSE}",
        text,
    )


def mask_multi_name(text):
    if any(code in text for code in NAME_JOIN):
        raise ValueError("ImoScripter 脚本文字包含保留码位")
    # Preserve the exact two-character width so source offsets stay intact.
    return MULTI_NAME.sub(lambda m: m[1] + m[2].replace("】【", NAME_JOIN) + m[3], text)


def assignment_helper(script):
    lines = [line.strip() for line in script.splitlines() if line.strip()]
    if not lines or not lines[0].startswith("@@!"):
        return False
    assignments = [line for line in lines if not line.startswith(("@", "\\"))]
    return bool(assignments) and all(INDEX_ASSIGNMENT.fullmatch(line) for line in assignments)


def scan(script, *, unicode_scripts=False, dialect="imos"):
    if dialect == AVG_SCENARIO:
        return tuple(ScenarioRecord(field) for field in qlie_legacy.qlie_fields(script))
    if assignment_helper(script):
        return ()
    script = PC_TAIL.sub(r"\1\2\3]", script)
    masked = mask_multi_name(mask_ruby(script))
    records = qlie_imos.scan(masked)
    if masked == script:
        return records
    return tuple(
        replace(
            record,
            parts=tuple(replace(part, raw=script[part.start:part.end]) for part in record.parts),
            speaker=(replace(record.speaker, raw=script[record.speaker.start:record.speaker.end])
                     if record.speaker else None),
        )
        for record in records
    )


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
    # A leading 【 is legal prose in Library/Avg when the line does not end in 】
    # (e.g. "【キリエ】─"). Mask only existing message prefixes preserving offsets,
    # then verify both field kinds and text rows after patching.
    marker = "\ufdd6"
    if marker in script or any(marker in value for value in edits.values()):
        raise ValueError("QLIE 剧情脚本文字包含保留码位")
    chars = list(script)
    masked_edits = dict(edits)
    for index, value in edits.items():
        field = records[index].field
        if field.kind == "message" and field.raw.startswith("【") and value.startswith("【"):
            chars[field.start] = marker
            masked_edits[index] = marker + value[1:]
    result = qlie_legacy.patch_qlie("".join(chars), masked_edits).replace(marker, "【")
    rebuilt = scan(result, dialect=AVG_SCENARIO)
    if [record.field.kind for record in rebuilt] != [record.field.kind for record in records]:
        raise ValueError("QLIE 剧情脚本字段类型发生变化")
    if [record.row() for record in rebuilt] != rows:
        raise ValueError("QLIE 剧情脚本回填后字段文字发生变化")
    return result


def patch(script, rows, *, unicode_scripts=False, dialect="imos"):
    if dialect == AVG_SCENARIO:
        return _patch_scenario(script, rows)
    if not rows and assignment_helper(script):
        return script
    records = scan(script, unicode_scripts=unicode_scripts)
    original = [record.row() for record in records]
    if original == rows:
        return script
    if INDENT in script or any(INDENT in text or any(code in text for code in NAME_JOIN)
                               for row in rows for text in row.values()):
        raise ValueError("QLIE 脚本文字包含保留的缩进或姓名码位")
    masked_rows = []
    for before, after in zip(original, rows, strict=True):
        if any(sorted(RUBY.findall(before[field])) != sorted(RUBY.findall(after[field])) for field in before):
            raise ValueError("QLIE 译文的注音标签发生变化")
        masked = {field: mask_ruby(text) for field, text in after.items()}
        if "name" in masked and "】【" in before["name"]:
            name = masked["name"]
            if "】【" in name and (name.startswith("】【") or name.endswith("】【") or "】【】【" in name):
                raise ValueError("QLIE 多人名分隔符位置无效")
            masked["name"] = name.replace("】【", NAME_JOIN)
        # One full-width space determines the line role. Extra indentation belongs to the visible text.
        masked["message"] = re.sub(r"^　+", lambda m: INDENT * len(m[0]), masked["message"], flags=re.M)
        masked_rows.append(masked)
    normalized = PC_TAIL.sub(r"\1\2\3]", script)
    result = qlie_imos.patch(mask_multi_name(mask_ruby(normalized)), masked_rows)
    result = (result.replace(RUBY_OPEN, "[").replace(RUBY_CLOSE, "]")
              .replace(RUBY_COMMA, ",").replace(NAME_JOIN, "】【"))
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
    rebuilt = scan(result, unicode_scripts=unicode_scripts)
    if [record.row() for record in rebuilt] != rows or (
        [[p.kind for p in r.parts] for r in rebuilt] != [[p.kind for p in r.parts] for r in records]
    ):
        raise ValueError("QLIE 回填后脚本文字或行类型发生变化")
    return result
