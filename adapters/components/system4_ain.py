"""System4 AI2 scripts with length-prefixed MSG1 records."""
import json
from pathlib import Path
from struct import Struct
import sys
import zlib

from python.archives.ain import MAX_RAW, repack, unpack
from python.engines.alicesoft import cipher


WORD = Struct("<I")
HEADER_SIZE = 16
MAX_CONTAINER = 64 << 20
MAX_SLOT = 1 << 20
MAX_SLOTS = 1_000_000
ENCODINGS = ("cp932", "gbk")


def code_header(raw):
    if len(raw) < 16 or raw[:4] != b"VERS":
        raise ValueError("AIN 缺少 VERS 段。")
    version, = WORD.unpack_from(raw, 4)
    code = 16 if raw[8:12] == b"KEYC" else 8
    if version == 0 or raw[code:code + 4] != b"CODE" or len(raw) < code + 8:
        raise ValueError("AIN 的 VERS 或 CODE 段不受支持。")
    length, = WORD.unpack_from(raw, code + 4)
    return version, code + 8 + length


def prefix_version(data, length):
    """Check a bounded AI2 prefix, including the inflated VERS and CODE tags."""
    if len(data) < HEADER_SIZE + 2 or data[:4] != b"AI2\0":
        raise ValueError("AIN 缺少 AI2 容器头。")
    size, = WORD.unpack_from(data, 8)
    stored, = WORD.unpack_from(data, 12)
    if not 0 < size <= MAX_RAW or length > MAX_CONTAINER or stored not in (0, length - HEADER_SIZE):
        raise ValueError("AI2 声明的长度超出范围或与文件长度不一致。")
    raw = zlib.decompressobj().decompress(data[HEADER_SIZE:], 24)
    version, code_end = code_header(raw)
    if code_end + 4 > size:
        raise ValueError("AIN 的 CODE 段越过解压数据边界。")
    return version


def read_file(path):
    with path.open("rb") as stream:
        data = stream.read(MAX_CONTAINER + 1)
    if len(data) > MAX_CONTAINER:
        raise ValueError("AIN 文件超过 64 MiB 读取上限。")
    return data


def message_slots(raw, start):
    count, = WORD.unpack_from(raw, start + 4)
    pos = start + 12
    if not 0 < count <= MAX_SLOTS or pos + count * 4 > len(raw):
        return None
    slots = []
    for _ in range(count):
        if pos + 4 > len(raw):
            return None
        length, = WORD.unpack_from(raw, pos)
        end = pos + 4 + length
        if length > MAX_SLOT or end > len(raw):
            return None
        slots.append((pos, end))
        pos = end
    if raw[pos:pos + 4] == b"MAIN":
        return slots
    return None


class Ain:
    """Read and rebuild MSG1 slots by zero-based index, independent of text encoding."""

    def __init__(self, data):
        self.data = data
        self.raw = raw = unpack(data)
        self.version, code_end = code_header(raw)
        if raw[code_end:code_end + 4] != b"FUNC":
            raise ValueError("AIN 的 CODE 段后缺少 FUNC。")
        matches = 0
        table = None
        start = raw.find(b"MSG1", code_end)
        while 0 <= start <= len(raw) - 12:
            matches += 1
            if matches > 64:
                raise ValueError("AIN 的 MSG1 候选过多，无法确认唯一文字表。")
            slots = message_slots(raw, start)
            if slots is not None:
                if table is not None:
                    raise ValueError("AIN 有多个以 MAIN 为边界的 MSG1 表。")
                table = slots
            start = raw.find(b"MSG1", start + 4)
        if table is None:
            raise ValueError("AIN 未找到布局有效且后接 MAIN 的 MSG1 表。")
        self.slots = table

    def message(self, index):
        start, end = self.slots[index]
        return cipher(self.raw[start + 4:end], decrypt=True)

    def decoded_messages(self, encodings=ENCODINGS):
        """Return complete decodings for codecs that accept every message."""
        choices = {encoding: [] for encoding in encodings}
        for index in range(len(self.slots)):
            message = self.message(index)
            for encoding in tuple(choices):
                try:
                    text = message.decode(encoding).rstrip(" ")
                except UnicodeDecodeError:
                    del choices[encoding]
                else:
                    choices[encoding].append(text)
            if not choices:
                break
        return choices

    def revised_raw(self, changes):
        """Update lengths and ciphertext while preserving all other bytes."""
        parts = []
        cursor = 0
        total_length = len(self.raw)
        for index, message in sorted(changes.items()):
            if not 0 <= index < len(self.slots):
                raise ValueError(f"MSG1 槽位 {index} 超出范围。")
            if not isinstance(message, bytes) or not message or len(message) > MAX_SLOT or b"\0" in message:
                raise ValueError(f"MSG1 槽位 {index} 的译文为空、含 NUL 或超长。")
            if message == self.message(index):
                continue
            start, end = self.slots[index]
            total_length += 4 + len(message) - (end - start)
            if total_length > MAX_RAW:
                raise ValueError("回填后的 AIN 解压数据超过 64 MiB。")
            parts.extend((self.raw[cursor:start], WORD.pack(len(message)), cipher(message, decrypt=False)))
            cursor = end
        if not parts:
            return self.raw
        parts.append(self.raw[cursor:])
        return b"".join(parts)

    def rebuild(self, changes):
        raw = self.revised_raw(changes)
        return self.data if raw is self.raw else repack(self.data, raw)


def inspect(ain, check_original=True, check_translation=True):
    decodings = ain.decoded_messages()
    encoding = next(iter(decodings)) if len(decodings) == 1 else None
    original = "skipped"
    translation = "skipped"
    length_change = "not-run"
    example = next((index for index, (start, end) in enumerate(ain.slots)
                    if 4 < end - start < MAX_SLOT), None)
    if check_original:
        if ain.rebuild({}) != ain.data:
            raise ValueError("AIN 原文回填字节不一致。")
        original = "passed"
    if check_translation:
        translation = "inconclusive"
        if example is not None:
            start, end = ain.slots[example]
            marker = "汉化验证".encode("gbk") if encoding == "gbk" else b"Test"
            replacement = (marker * ((end - start - 4) // len(marker) + 1))[:MAX_SLOT]
            rebuilt = Ain(ain.rebuild({example: replacement}))
            new_start, new_end = rebuilt.slots[example]
            if (rebuilt.version != ain.version or len(ain.slots) != len(rebuilt.slots)
                    or rebuilt.message(example) != replacement or new_start != start
                    or rebuilt.raw[:new_start] != ain.raw[:start] or rebuilt.raw[new_end:] != ain.raw[end:]):
                raise ValueError("AIN 变长回填改变了槽位身份、未修改字节或译文。")
            length_change = "passed"
            if encoding == "gbk":
                translation = "passed"
    return dict(success=True, stage="complete", parseStatus="passed", roundtripStatus=original,
                translationStatus=translation, runtimeStatus="not-tested",
                fieldCount=sum(end > start + 4 for start, end in ain.slots),
                profile=dict(ainVersion=ain.version, textTable="MSG1", slotCount=len(ain.slots),
                             sourceEncoding=encoding, encodingCandidates=list(decodings),
                             lengthChangeReadback=length_change))


def main(request=None):
    request = json.load(sys.stdin) if request is None else request
    try:
        return inspect(Ain(read_file(Path(request["input"]))),
                       request.get("checkRoundtrip", True), request.get("checkTranslation", True))
    except (OSError, ValueError, zlib.error) as error:
        return dict(success=False, stage="parse", parseStatus="failed", roundtripStatus="not-run",
                    translationStatus="not-run", runtimeStatus="not-tested", fieldCount=0,
                    error=str(error), diagnostic=dict(errorType=type(error).__name__))


if __name__ == "__main__":
    result = main()
    print(json.dumps(result, ensure_ascii=True))
    raise SystemExit(0 if result["success"] else 1)
