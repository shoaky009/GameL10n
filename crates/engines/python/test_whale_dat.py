"""Whale DAT script extraction, asset resolution and member replacement."""

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

from adapters.components import engine_archive, whale_dat, whale_script


def dat(entries, title="Fixture"):
    index = bytearray(struct.pack("<I", len(entries) ^ 0x26ACA46E))
    payload = bytearray()
    for name, kind, value in entries:
        raw = name.encode("cp932")
        key = whale_dat.crc64(raw)
        low = key & 0xFFFFFFFF
        offset = 4 + 21 * len(entries) + len(payload)
        if kind == 2:
            packed = whale_dat.transform_script(zlib.compress(value), low ^ whale_dat.title_key(title))
            stored, decoded = len(packed), len(value)
        else:
            packed = value
            offset ^= raw[len(raw) // 2]
            stored, decoded = len(value) ^ raw[len(raw) // 4], len(value) ^ raw[len(raw) // 8]
        index += struct.pack("<QBIII", key, kind ^ (low & 0xFF), offset ^ low, stored ^ low, decoded ^ low)
        payload += packed
    return bytes(index + payload)


class WhaleDatTest(unittest.TestCase):
    def test_script_names_select_media_members(self):
        self.assertEqual(whale_dat.crc64(b"123456789"), 0x62EC59E3F1A4F00A)
        script = "BG school\r\n【花子,S002_A_0020】「こんにちは」\r\n"
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "script.dat"
            source.write_bytes(dat([("scene", 2, script.encode("cp932"))]))
            media = root / "arc0.dat"
            media.write_bytes(dat([("bg\\school.tlg", 0, b"matched"), ("unused.bin", 0, b"other")]))
            base = dict(protocolVersion=1, component="archive.whale-dat")
            result = engine_archive.execute(dict(base, operation="extract", archive=str(source),
                                                 gameTitle="Fixture", outputDirectory=str(root / "script/files")))
            names = root / "script" / result["filenameManifest"]
            self.assertEqual(json.loads(names.read_text(encoding="utf-8")), ["bg/school.tlg", "vo/002/S002_A_0020.ogg"])
            report = engine_archive.execute(dict(base, operation="extract", archive=str(media),
                                                 filenameManifest=str(names), outputDirectory=str(root / "media/files")))
            self.assertEqual(report["unresolvedCount"], 1)
            self.assertEqual((root / "media/files/bg/school.tlg").read_bytes(), b"matched")
            self.assertEqual(report["readBytes"], report["indexBytes"] + len(b"matched"))
            self.assertFalse(report["allContentsVerified"])

    def test_rebuild_keeps_member_ids_and_untouched_script(self):
        script = "【花子】「こんにちは」\r\n".encode("cp932")
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "original.dat"
            original = dat([("begin", 2, script), ("end", 2, b"trace")])
            source.write_bytes(original)
            name = f'{whale_dat.crc64(b"begin"):016x}.script'
            replacement = root / "replacement.script"
            replacement.write_bytes(script)
            request = dict(protocolVersion=1, component="archive.whale-dat", archive=str(source),
                           gameTitle="Fixture", operation="rebuild", outputFile=str(root / "no-op.dat"),
                           replacements={name: str(replacement)})
            self.assertTrue(engine_archive.execute(request)["byteIdentical"])
            self.assertEqual((root / "no-op.dat").read_bytes(), original)
            patched = "【花子】「長い文章を確認する」\r\n".encode("cp932")
            replacement.write_bytes(patched)
            rebuilt = root / "replaced.dat"
            self.assertEqual(engine_archive.execute(dict(request, outputFile=str(rebuilt)))["changedCount"], 1)
            results = engine_archive.execute(dict(request, operation="extract", archive=str(rebuilt),
                                                 outputDirectory=str(root / "out/files")))
            self.assertEqual(results["extractedFiles"], [name, f'{whale_dat.crc64(b"end"):016x}.script'])
            self.assertEqual((root / "out/files" / name).read_bytes(), patched)
            self.assertEqual((root / "out/files" / results["extractedFiles"][1]).read_bytes(), b"trace")

    def test_sprite_and_explicit_resource_candidates(self):
        script = "ST S002_1FA1AA_121A_M, 500\nCG 5, bg/BLACK@@0\nBGM tune"
        self.assertEqual(whale_script.asset_names(script), {
            "st/002M/121_A.tlg", "st/002M/S002_1FA1AA_0001_M.tlg", "bg/BLACK.tlg", "bgm/tune.ogg",
        })
