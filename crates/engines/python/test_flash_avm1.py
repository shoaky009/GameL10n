import struct
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "adapters/components"))
from flash_avm1 import Movie, tag_bytes


def record(op, data):
    return bytes([op]) + struct.pack("<H", len(data)) + data


def pool(*strings):
    return record(0x88, struct.pack("<H", len(strings)) + b"".join(s + b"\0" for s in strings))


def movie(actions):
    body = (b"\x08\x00\x00\x0c\x01\x00" + tag_bytes(69, b"\0" * 4)
            + tag_bytes(12, actions) + tag_bytes(1, b"") + tag_bytes(0, b""))
    return b"FWS\x0f" + struct.pack("<I", len(body) + 8) + body


def jump_target(code, p):
    assert code[p:p+3] in (b"\x99\x02\0", b"\x9d\x02\0")
    return p + 5 + struct.unpack_from("<h", code, p + 3)[0]


class FlashPoolTests(unittest.TestCase):
    def test_oversized_pool_uses_literal_at_original_push_operand(self):
        block = pool("台詞".encode(), b"x" * 63000) + record(0x96, b"\x08\0\x08\x01") + b"\x00"
        source = Movie(movie(block))
        key = "pool:0:0"
        edits = {key: "translation" * 550}
        self.assertEqual(source.build({key: "台詞"}), source.source)
        patched = Movie(source.build(edits))
        wire = next(data for code, _, data, _ in patched.tags if code == 12)
        push = len(pool("台詞".encode(), b"x" * 63000))
        self.assertEqual(wire[push+3:-1], b"\0" + edits[key].encode() + b"\0\x08\x01")
        self.assertEqual(list(patched.fields()), [(key, edits[key], "name")])
        source.validate(patched, edits)
        edited_again = Movie(patched.build({key: "中文"}))
        self.assertEqual(list(edited_again.fields()), [(key, "中文", "name")])

    def test_raw_branch_lands_at_original_byte_across_expanded_pushes(self):
        prefix = pool("本文".encode())
        pushes = record(0x96, b"\x08\0") * 12
        target = 6
        branch = record(0x9d, struct.pack("<h", target - len(prefix + pushes) - 5))
        original = prefix + pushes + branch + b"\0"
        source = Movie(movie(original))
        edits = {"pool:0:0": "translated" * 600}
        patched = Movie(source.build(edits))
        wire = next(data for code, _, data, _ in patched.tags if code == 12)
        self.assertEqual(wire[:len(prefix)], prefix)
        p = len(prefix)
        literals = []
        while wire[p] != 0x9d:
            if wire[p] == 0x99:
                p = jump_target(wire, p)
            else:
                self.assertEqual(wire[p], 0x96)
                length = struct.unpack_from("<H", wire, p + 1)[0]
                literals.append(wire[p+3:p+3+length])
                p += 3 + length
        self.assertEqual(literals, [b"\0" + edits["pool:0:0"].encode() + b"\0"] * 12)
        p = jump_target(wire, p)
        hops = 0
        while p >= len(prefix):
            self.assertLess(hops, 12)
            p = jump_target(wire, p)
            hops += 1
        self.assertGreater(hops, 0)
        self.assertEqual(p, target)
        source.validate(patched, edits)

    def test_branch_to_an_unmapped_push_operand_is_rejected(self):
        data = pool("台詞".encode()) + record(0x96, b"\x08\0") + record(0x99, b"\xfa\xff") + b"\0"
        with self.assertRaisesRegex(ValueError, "unmapped AVM1 branch target"):
            Movie(movie(data))

    def test_oversized_single_literal_identifies_instruction_and_limit(self):
        original = pool("項目".encode()) + record(0x96, b"\x08\0") + b"\0"
        with self.assertRaisesRegex(ValueError, "required=.*limit=65535"):
            Movie(movie(original)).build({"pool:0:0": "a" * 66000})
