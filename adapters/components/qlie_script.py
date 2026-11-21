"""ImoScripter text with byte-preserving encodings and ruby annotations."""
from dataclasses import replace
import re

from python.engines import qlie_imos


UTF16_LE = b"\xff\xfe"
UTF16_BE = b"\xfe\xff"
RUBY = re.compile(r"\[rb,([^,\[\]\r\n]+),([^,\[\]\r\n]+)\]")
RUBY_OPEN, RUBY_CLOSE = "\ufdd0", "\ufdd1"
INDENT = "\ufdd2"
PC_TAIL = re.compile(r"^([ \t　]*(?:\[spd,0\])?\[pc,)([^\[\]\r\n]*)\]([、。！？…]+)(?=\r?$)", re.M)
INDEX_ASSIGNMENT = re.compile(r"[A-Za-z_]\w*\[[0-9]+\]=[A-Za-z_]\w*", re.ASCII)


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


def scan(script, *, unicode_scripts=False):
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


def patch(script, rows, *, unicode_scripts=False):
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
