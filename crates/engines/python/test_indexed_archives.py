"""Selected member reads over the pinned archive library."""
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

from adapters.components import engine_archive as archive
from python.archives import bgi_writer, pfs, yuris_482


def ypf482(members):
    def name_record(name):
        raw = name.encode("cp932")
        length = dict(zip(yuris_482.SWAP_TABLE_00[::2], yuris_482.SWAP_TABLE_00[1::2]))
        length.update({right: left for left, right in list(length.items())})
        return struct.pack("<IB", yuris_482.murmur2(raw), length.get(len(raw), len(raw)) ^ 255) + bytes(
            byte ^ 255 for byte in raw)

    names = [name_record(name) for name, _ in members]
    position = end = 32 + sum(len(record) + 22 for record in names)
    output = bytearray(struct.pack("<4sIII", b"YPF\0", 482, len(names), end).ljust(32, b"\0"))
    for record, (_, data) in zip(names, members):
        output.extend(record)
        output.extend(struct.pack("<BBIIQI", 0, 0, len(data), len(data), position, yuris_482.murmur2(data)))
        position += len(data)
    output.extend(b"".join(data for _, data in members))
    return output


class IndexedArchivesTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.source = self.root / "sample.archive"

    def run_component(self, component, operation, **arguments):
        return archive.execute(dict(protocolVersion=1, component=component, operation=operation,
                                    archive=str(self.source), **arguments))

    def test_bgi_and_pfs_select_scripts_without_reading_media(self):
        for component, build, script in (
            ("archive.bgi", bgi_writer.build_archive, "scene.bgi"),
            ("archive.pfs8", pfs.build, "scripts\\scene.ast"),
        ):
            with self.subTest(component=component):
                self.source.write_bytes(build([(script, b"script"), ("media.bin", b"M" * (1 << 20))]))
                output = self.root / component
                details = self.run_component(component, "inspect")
                self.assertEqual(details["readBytes"], details["indexBytes"])
                result = self.run_component(component, "extract", outputDirectory=str(output), members=[script])
                self.assertEqual(result["readBytes"] - result["indexBytes"], len(b"script"))
                self.assertEqual((output / script.replace("\\", "/")).read_bytes(), b"script")
                self.assertIsNone(result["entries"][1]["sha256"])

    def test_plain_kif_samples_and_encrypted_kif_diagnostic(self):
        payload = b"CatScene" + bytes(16)
        directory = b"scene.cst".ljust(32, b"\0") + struct.pack("<II", 48, len(payload))
        self.source.write_bytes(b"KIF\0" + struct.pack("<I", 1) + directory + payload)
        result = archive.script_samples("CatSystem", self.source, self.root)
        self.assertEqual(result["status"], "sampled")
        self.assertEqual((self.root / "0/scene.cst").read_bytes(), payload)
        self.source.write_bytes(b"KIF\0" + struct.pack("<I", 1) + b"__key__.dat\0".ljust(72, b"\0"))
        with self.assertRaisesRegex(ValueError, "confirmed filename scheme"):
            self.run_component("archive.kif", "inspect")

    def test_ypf_samples_keep_same_directory_command_table(self):
        script = (r"scenario\scene.ybn", b"YSTB" + bytes(28))
        companion = (r"scenario\YSC.YBN", b"command table")
        self.source.write_bytes(ypf482([script, companion]))
        result = archive.script_samples("Yuris", self.source, self.root)
        self.assertEqual(result["samples"][0]["files"], ["scenario/scene.ybn", "scenario/ysc.ybn"])
        self.assertEqual((self.root / "0/scenario/ysc.ybn").read_bytes(), companion[1])
        self.source.write_bytes(ypf482([script, ("ysc.ybn", companion[1])]))
        result = archive.script_samples("Yuris", self.source, self.root)
        self.assertEqual(result["status"], "sample-unavailable")
        self.assertIn("ysc.ybn", result["diagnostics"][0])

    def test_pfs_rejects_unsafe_index_before_any_extraction(self):
        self.source.write_bytes(pfs.build([("scene.ast", b"script"), ("../other", b"bad")]))
        output = self.root / "extracted"
        with self.assertRaisesRegex(ValueError, "unsafe archive"):
            self.run_component("archive.pfs8", "extract", outputDirectory=str(output), members=["scene.ast"])
        self.assertFalse(output.exists())
