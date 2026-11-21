"""Selected member reads over the pinned archive library."""
from io import BytesIO
import json
from pathlib import Path
import struct
import sys
import tempfile
import unittest
import zlib

ROOT = Path(__file__).resolve().parents[3]
TOOLS = ROOT / "third_party/engine-tools"
PIN = json.loads((TOOLS / "current.json").read_text(encoding="utf-8"))
sys.path[:0] = [str(ROOT), str(TOOLS / "references" / PIN["revision"] / "library.zip")]

from adapters.components import engine_archive as archive, willplus_wsc
from python.archives import bgi, bgi_writer, pfs, yuris_482
from python.engines.bgi import MAGIC as BGI_SCRIPT_MAGIC


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

    def test_bgi_rebuild_preserves_member_codec_and_index_metadata(self):
        script = BGI_SCRIPT_MAGIC + b"original"
        edited = BGI_SCRIPT_MAGIC + b"new script"
        media = b"CompressedBG fixture image"
        replacement = self.root / "replacement.bgi"
        replacement.write_bytes(edited)
        for version, compress in [(1, False), (2, True)]:
            with self.subTest(version=version):
                extra = b"Q" * bgi_writer.row_length(version)
                data = bgi_writer.build_archive([
                    ("story", bgi_writer.pack_member(script, compress=compress), extra),
                    ("media", media, extra),
                ], version=version)
                self.source.write_bytes(data)
                unchanged = self.root / f"unchanged-{version}.arc"
                self.run_component("archive.bgi", "rebuild", outputFile=str(unchanged))
                self.assertEqual(unchanged.read_bytes(), data)
                output = self.root / f"updated-{version}.arc"
                self.run_component("archive.bgi", "rebuild", outputFile=str(output),
                                   replacements={"story": str(replacement)})
                stream = BytesIO(output.read_bytes())
                index = bgi.read_index(stream)
                self.assertEqual(index.version, version)
                self.assertEqual([(entry.name, entry.extra) for entry in index.entries],
                                 [("story", extra), ("media", extra)])
                stored = bgi.read_member(stream, index, index.entries[0])
                self.assertEqual(stored.codec, "dsc" if compress else "raw")
                self.assertEqual(bgi.decode_member(stored), edited)
                self.assertEqual(bgi.read_member(stream, index, index.entries[1]).stored_data, media)

        self.source.write_bytes(data + b"opaque trailing bytes")
        with self.assertRaisesRegex(ValueError, "cannot be reproduced byte for byte"):
            self.run_component("archive.bgi", "rebuild", outputFile=str(self.root / "refused.arc"),
                               replacements={"story": str(replacement)})

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
        executable = self.root / "other.exe"
        executable.write_bytes(b"MZ")
        with self.assertRaisesRegex(ValueError, "verified against named CatScene"):
            self.run_component("archive.kif", "inspect", keyExecutable=str(executable))

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

    def test_ypf474_rebuild_preserves_the_other_member_and_checks_stored_adler32(self):
        names = (b"a.ybn", b"b.ybn")
        start = 32 + sum(23 + len(name) for name in names)
        header = bytearray(struct.pack("<4sIII", b"YPF\0", 474, 2, start).ljust(32, b"\0"))
        payload = bytearray()
        for name in names:
            stored = zlib.compress(name)
            header.extend(struct.pack("<IB", zlib.crc32(name), len(name) ^ 255))
            header.extend(b ^ 255 for b in name)
            header.extend(struct.pack("<BBIIII", 0, 1, len(name), len(stored), start + len(payload),
                                      zlib.adler32(stored)))
            payload.extend(stored)
        self.source.write_bytes(header + payload)
        replacement = self.root / "replacement"
        replacement.write_bytes(bytes(range(128)))
        output = self.root / "new.ypf"
        result = self.run_component("archive.ypf474", "rebuild", outputFile=str(output),
                                    replacements={"a.ybn": str(replacement)})
        self.assertEqual(result["changedCount"], 1)
        with output.open("rb") as stream:
            _, entries, read = archive.open_index("archive.ypf474", stream)
            self.assertEqual([read(e, 1024) for e in entries], [replacement.read_bytes(), b"b.ybn"])
        payload[-1] ^= 1
        self.source.write_bytes(header + payload)
        with self.assertRaisesRegex(ValueError, "Adler32"):
            self.run_component("archive.ypf474", "inspect", verifyContents=True)

    def test_wsc_length_change_relocates_forward_and_backward_branches(self):
        dialogue = b"\x41\0\0\0\0A\0"
        end = 11 + len(dialogue) + 6
        conditional = b"\x01" + bytes(5) + struct.pack("<iB", end - 11, 0)
        jump = b"\x06" + struct.pack("<IB", end + 11, 0)
        trailer = b"\xff" + bytes(range(8))
        # A backward relative branch after the edited field moves its base, but keeps its target.
        back = b"\x01" + bytes(5) + struct.pack("<iB", -(end + 11), 0)
        stored = (conditional + dialogue + jump + back + trailer).translate(willplus_wsc.ENCODE)
        script = willplus_wsc.ScriptText(stored)
        rebuilt = script.build({"cmd:1:text": "ABCDE"}).translate(willplus_wsc.DECODE)
        self.assertEqual(struct.unpack_from("<i", rebuilt, 6)[0], end - 11 + 4)
        self.assertEqual(struct.unpack_from("<I", rebuilt, end - 6 + 4 + 1)[0], end + 11 + 4)
        self.assertEqual(struct.unpack_from("<i", rebuilt, end + 4 + 6)[0], -(end + 4 + 11))
        self.assertEqual(rebuilt[-9:], trailer)
        with self.assertRaises(ValueError):
            willplus_wsc.ScriptText(stored[:-8])

    def test_pfs_rejects_unsafe_index_before_any_extraction(self):
        self.source.write_bytes(pfs.build([("scene.ast", b"script"), ("../other", b"bad")]))
        output = self.root / "extracted"
        with self.assertRaisesRegex(ValueError, "unsafe archive"):
            self.run_component("archive.pfs8", "extract", outputDirectory=str(output), members=["scene.ast"])
        self.assertFalse(output.exists())
