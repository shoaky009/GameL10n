"""ADVSCRIPT2 extraction and NEKOPACK4A replacement."""

import json
from pathlib import Path
import struct
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[3]
TOOLS = ROOT / "third_party/engine-tools"
PIN = json.loads((TOOLS / "current.json").read_text(encoding="utf-8"))
sys.path[:0] = [str(ROOT), str(TOOLS / "references" / PIN["revision"] / "library.zip")]

from adapters.components import engine_archive, nekosdk_adapter, nekosdk_script
from python.archives import nekopack


def record(code, *fields):
    header = struct.pack("<IIII", 13, 0, 45, code).ljust(212, b"\0")
    strings = [text.encode("cp932") + b"\0" for text in fields] + [b"\0"] * (33 - len(fields))
    return header + b"".join(struct.pack("<I", len(text)) + text for text in strings)


class NekoSdkTest(unittest.TestCase):
    def test_parameters_and_nontext_records(self):
        display = record(5, "[テキスト表示] 速度:0", "花子", "こんにちは", "voice.ogg")
        control = record(10, "[背景ロード]", "image.png")
        data = nekosdk_script.MAGIC + struct.pack("<I", 2) + display + control
        script = nekosdk_script.ScriptText(data)
        self.assertEqual([(f.path, f.text) for f in script.fields], [("0:1", "花子"), ("0:2", "こんにちは")])
        self.assertIs(script.build({"0:2": "こんにちは"}), data)
        patched = script.build({"0:2": "翻訳済みの会話"})
        self.assertTrue(patched.endswith(control))
        self.assertEqual(nekosdk_script.ScriptText(patched).fields[1].text, "翻訳済みの会話")
        with self.assertRaisesRegex(ValueError, "exceeds the script"):
            nekosdk_script.ScriptText(data[:-1])

    def test_archive_probe_and_partial_replacement(self):
        data = nekosdk_script.MAGIC + struct.pack("<I", 1) + record(5, "[テキスト表示]", "", "こんにちは")
        media = b"untouched media"
        pack = nekopack.build_pak({"scene.txt": data, "image.bin": media})
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "script.pak").write_bytes(pack)
            (root / "images.pak").write_bytes(nekopack.build_pak({"image.bin": media}))
            probe = nekosdk_adapter.execute(dict(operation="probe", sourceDirectory=str(root),
                                                resourceFiles=["script.pak", "images.pak"]))
            request = dict(sourceDirectory=str(root), resourceFiles=probe["probe"]["requiredFiles"])
            self.assertEqual(request["resourceFiles"], ["script.pak"])
            inspected = engine_archive.execute(dict(protocolVersion=1, operation="inspect",
                                                    component="archive.nekopack4a", archive=str(root / "script.pak")))
            self.assertEqual(inspected["readBytes"], inspected["indexBytes"])
            output = root / "untouched"
            nekosdk_adapter.execute(dict(request, operation="build", outputDirectory=str(output)))
            self.assertEqual((output / "script.pak").read_bytes(), pack)

            units = nekosdk_adapter.execute(dict(request, operation="extract"))["extraction"]["units"]
            output = root / "translated"
            request.update(outputDirectory=str(output), translations={units[0]["id"]: "別の文"})
            nekosdk_adapter.execute(dict(request, operation="build"))
            self.assertTrue(nekosdk_adapter.execute(dict(request, operation="validate"))["validation"]["passed"])
            patched = (output / "script.pak").read_bytes()
            entries = nekopack.open_pak(patched).entries
            self.assertEqual(nekopack.read_member(patched, entries[1]), media)
            text = nekosdk_script.ScriptText(nekopack.read_member(patched, entries[0]))
            self.assertEqual(text.fields[0].text, "別の文")
