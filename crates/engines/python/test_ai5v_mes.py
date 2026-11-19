"""AI5V dictionary, inline text and command operand handling."""

from pathlib import Path
import sys
import unittest


sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

from adapters.components.ai5v_mes import ScriptText


class Ai5vMesTest(unittest.TestCase):
    def test_edit_expands_dictionary_without_changing_control_operands(self):
        first = "あ".encode("cp932")
        inline = "い".encode("cp932")
        name = b"\x04\x1e\x06NEXT.MES\x06"
        control = b"\x11\x08\x83\xc3\x03"
        code = b"\x01" + name + b"\x80" + bytes([inline[0] - 0x20, inline[1]]) + control + b"\x00\x80\x00"
        raw = b"\x04\x00" + first + code
        script = ScriptText(raw)
        self.assertEqual([field.text for field in script.fields], ["あい", "あ"])
        self.assertIs(script.build({"text/0": "あい"}), raw)
        new_pair = b"\xe0\x40"
        replacement = "あ" + new_pair.decode("cp932") * 2 + "い"
        rebuilt = script.build({"text/0": replacement})
        self.assertEqual(rebuilt[:6], b"\x06\x00" + first + new_pair)
        self.assertIn(name + b"\x80\x81\x81", rebuilt)
        self.assertTrue(rebuilt.endswith(control + b"\x00\x80\x00"))
        self.assertEqual([field.text for field in ScriptText(rebuilt).fields], [replacement, "あ"])

    def test_rejects_unknown_bytecode_and_truncated_operands(self):
        dictionary = b"\x04\x00" + "あ".encode("cp932")
        for code in (b"\x81", b"\x04\x2a\x80", b"\x04", b"\x06abc", b"\x09\x83", b"\x01\x80"):
            with self.subTest(code=code):
                with self.assertRaises(ValueError):
                    ScriptText(dictionary + code)

    def test_does_not_reinterpret_parameter_strings_or_single_byte_translations(self):
        raw = b"\x04\x00" + "あ".encode("cp932") + b"\x06\xdf\x06\x80\x00"
        script = ScriptText(raw)
        self.assertEqual([field.text for field in script.fields], ["あ"])
        with self.assertRaises(ValueError):
            script.build({"text/0": "ASCII"})
