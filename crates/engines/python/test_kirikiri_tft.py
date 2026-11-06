"""Independent TFT v1 fixtures and optional native Windows glyph checks."""
import ctypes
import importlib.util
import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest


COMPONENT = Path(__file__).resolve().parents[3] / "adapters/components/kirikiri_tft.py"
SPEC = importlib.util.spec_from_file_location("kirikiri_tft", COMPONENT)
TFT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(TFT)


def fixture():
    """Hand-built from upstream's packed struct and Retrieve v1 algorithm.

    0x42 repeats the previous zero twice; 0x41 repeats 64 once. The first
    bitmap crosses a row boundary. Origins, IncX/IncY/Inc, reserved fields,
    non-index bytes and the trailer deliberately carry distinct values.
    """
    header = b"TVP pre-rendered font\x1a\x01\x02" + struct.pack("<III", 2, 48, 52)
    storage = b"\x00\x42\x40\x41\x07\x09\x41extra"
    chars = b"A\0C\0"
    records = struct.pack("<IHHhhhhhH", 36, 3, 2, -2, 9, 7, -1, 8, 0xBEEF)
    records += struct.pack("<IHHhhhhhH", 41, 2, 1, 1, 4, 3, 0, 5, 0x1234)
    return header + storage + chars + records + b"\xde\xadunknown tail"


def single_glyph(encoded, width, height):
    # Index first, bitmap last: EOF really bounds the compressed stream.
    header = b"TVP pre-rendered font\x1a\x01\x02" + struct.pack("<III", 1, 36, 38)
    item = struct.pack("<IHHhhhhhH", 58, width, height, 0, 3, 4, 0, 4, 0)
    return header + b"A\0" + item + encoded


class TftTest(unittest.TestCase):
    def test_hand_built_rle_fixture_preserves_distinct_metrics(self):
        raw = fixture()
        entries = TFT.index(raw)
        self.assertEqual(list(entries), [0x41, 0x43])
        self.assertEqual(entries[0x41], (36, 3, 2, -2, 9, 7, -1, 8, 0xBEEF))
        self.assertEqual(TFT.bitmap(raw, entries[0x41]), b"\0\0\0\x40\x40\x07")
        self.assertEqual(TFT.bitmap(raw, entries[0x43]), b"\x09\x09")

    def test_maximum_repeat_is_191_more_pixels_and_64_is_literal(self):
        raw = single_glyph(b"\x40\xff", 192, 1)
        self.assertEqual(TFT.bitmap(raw, TFT.index(raw)[0x41]), b"\x40" * 192)

    def test_supplement_merges_codes_and_preserves_old_records_bitmaps_and_unknown_bytes(self):
        calls = []

        class Renderer:
            def glyph(self, code):
                calls.append(code)
                return TFT.Glyph(999, 2, 1, -3, 10, 6, -2, 9, 0), b"\x04\x40"

        raw = fixture()
        old = TFT.index(raw)
        result = TFT.supplement(raw, [0x44, 0x41, 0x42, 0x44], Renderer())
        new = TFT.index(result)
        self.assertEqual(calls, [0x42, 0x44])
        self.assertEqual(list(new), [0x41, 0x42, 0x43, 0x44])
        self.assertEqual(result[:24], raw[:24])
        self.assertEqual(result[36:len(raw)], raw[36:])
        for code, item in old.items():
            self.assertEqual(new[code], item)
            self.assertEqual(TFT.bitmap(result, new[code]), TFT.bitmap(raw, item))
        self.assertEqual(new[0x42].offset, len(raw))
        self.assertEqual(new[0x44].offset, len(raw) + 2)
        self.assertEqual(new[0x42][1:], (2, 1, -3, 10, 6, -2, 9, 0))
        self.assertEqual(TFT.bitmap(result, new[0x42]), b"\x04\x40")
        self.assertEqual(TFT.supplement(result, new.keys(), None), result)

    def test_no_missing_glyph_is_byte_identical_without_a_renderer(self):
        raw = fixture()
        self.assertEqual(TFT.supplement(raw, [0x41, 0x43], None), raw)

    def test_empty_glyph_keeps_its_advance(self):
        raw = single_glyph(b"", 0, 0)
        item = TFT.index(raw)[0x41]
        self.assertEqual(TFT.bitmap(raw, item), b"")
        self.assertEqual(item.advance, 4)

        class Renderer:
            def glyph(self, code):
                return (0, 0, 0, 0, 0, 6, 0, 6, 0), b""

        result = TFT.supplement(raw, [0x20], Renderer())
        space = TFT.index(result)[0x20]
        self.assertEqual(TFT.bitmap(result, space), b"")
        self.assertEqual(space.advance, 6)

    def test_rejects_unsupported_headers_indexes_and_dimensions(self):
        changes = [
            (22, b"\0"),  # v0 has a different RLE and is deliberately unsupported.
            (23, b"\1"),
            (24, struct.pack("<I", 0)),
            (24, struct.pack("<I", 65537)),
            (28, struct.pack("<I", 0)),
            (32, struct.pack("<I", 49)),  # Overlapping code and record tables.
            (50, b"A\0"),  # Duplicate code.
            (48, b"Z\0"),  # Unsorted code.
            (52, struct.pack("<I", 35)),  # Bitmap in header.
            (52, struct.pack("<I", 48)),  # Bitmap in code table.
            (52, struct.pack("<I", len(fixture()) + 1)),
            (56, b"\xff\xff\xff\xff"),  # Unbounded bitmap dimensions.
        ]
        for offset, value in changes:
            with self.subTest(offset=offset, value=value):
                raw = bytearray(fixture())
                raw[offset:offset + len(value)] = value
                with self.assertRaises(ValueError):
                    TFT.index(raw)
        for raw in (b"", fixture()[:35], fixture()[:91], b"!" + fixture()[1:]):
            with self.subTest(truncated=len(raw)):
                with self.assertRaises(ValueError):
                    TFT.index(raw)

    def test_rejects_first_pixel_repeat_overruns_and_truncation(self):
        for encoded, size in ((b"\x41", 1), (b"\0\xff", 191), (b"\x07", 2)):
            with self.subTest(encoded=encoded, size=size):
                raw = single_glyph(encoded, size, 1)
                with self.assertRaises(ValueError):
                    TFT.index(raw)
        # A stream before the character table cannot consume index bytes.
        raw = bytearray(fixture())
        struct.pack_into("<IHH", raw, 52, 47, 20, 1)
        raw[47] = 0
        with self.assertRaisesRegex(ValueError, "crosses an index"):
            TFT.index(raw)

    def test_rejects_non_bmp_and_surrogates_before_rendering(self):
        for code in (-1, 0xD800, 0xDFFF, 0x10000, "A", True):
            with self.subTest(code=code):
                with self.assertRaisesRegex(ValueError, "BMP scalar"):
                    TFT.supplement(fixture(), [code], None)

    def test_bad_renderer_output_or_failure_returns_no_partial_change(self):
        raw = fixture()
        for pixels in (b"\x41\0", b"\0", "not bytes"):
            class Renderer:
                def glyph(self, code):
                    return (0, 2, 1, 0, 1, 3, 0, 3, 0), pixels
            with self.subTest(pixels=pixels):
                with self.assertRaises(ValueError):
                    TFT.supplement(raw, [0x42], Renderer())

        class Interrupted:
            def glyph(self, code):
                if code == 0x44:
                    raise RuntimeError("Renderer stopped")
                return (0, 1, 1, 0, 1, 3, 0, 3, 0), b"\x20"

        with self.assertRaisesRegex(RuntimeError, "Renderer stopped"):
            TFT.supplement(raw, [0x42, 0x44], Interrupted())
        self.assertEqual(raw, fixture())

    def test_probe_reports_bounded_coverage_and_metrics_without_writing(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "arbitrary-name.bin"
            raw = fixture()
            path.write_bytes(raw)
            result = TFT.probe(path, "ACB中", 2)
            self.assertEqual(result["status"], "matched")
            self.assertFalse(result["runtimeTested"])
            self.assertEqual(result["glyphCount"], 2)
            self.assertEqual(result["coverage"], dict(requestedCount=4, presentCount=2, missingCount=2,
                                                      missingCodepoints=["U+0042", "U+4E2D"]))
            self.assertEqual(len(result["samples"]), 2)
            self.assertEqual(result["samples"][0]["advance"], 8)
            process = subprocess.run([sys.executable, str(COMPONENT)],
                                     input=json.dumps(dict(operation="probe", input=str(path),
                                                           text="ABC", sampleLimit=0)),
                                     capture_output=True, text=True, check=True)
            self.assertEqual(json.loads(process.stdout)["samples"], [])
            self.assertEqual(path.read_bytes(), raw)
            self.assertEqual(list(Path(directory).iterdir()), [path])

    def test_probe_rejects_writes_and_unbounded_samples(self):
        process = subprocess.run([sys.executable, str(COMPONENT)], input='{"operation":"supplement"}',
                                 capture_output=True, text=True)
        self.assertNotEqual(process.returncode, 0)
        self.assertEqual(json.loads(process.stdout)["status"], "rejected")
        self.assertEqual(TFT.probe(None)["status"], "needs-evidence")
        self.assertEqual(TFT.probe(None, "", 3)["status"], "rejected")


@unittest.skipUnless(os.name == "nt", "Requires Windows GDI")
class NativeRendererTest(unittest.TestCase):
    def setUp(self):
        self.font_file = Path(os.environ.get("WINDIR", "C:/Windows")) / "Fonts/msyh.ttc"
        if not self.font_file.is_file():
            self.skipTest("Microsoft YaHei font file is unavailable")

    def test_no_change_does_not_open_gdi(self):
        with TFT.GdiRenderer(self.font_file, "Microsoft YaHei", 24) as renderer:
            self.assertEqual(TFT.supplement(fixture(), [0x41], renderer), fixture())
            self.assertFalse(renderer.registered)
            self.assertIsNone(renderer.dc)

    def test_real_chinese_bitmap_missing_coverage_face_check_and_resource_release(self):
        user = ctypes.WinDLL("user32", use_last_error=True)
        user.GetGuiResources.argtypes = [ctypes.c_void_p, ctypes.c_uint]
        user.GetGuiResources.restype = ctypes.c_uint
        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel.GetCurrentProcess.restype = ctypes.c_void_p
        process = kernel.GetCurrentProcess()

        # GDI reports localized names on some Windows installations. Select an
        # explicit matching name; production code must also reject aliases.
        for face in ("Microsoft YaHei", "微软雅黑"):
            try:
                with TFT.GdiRenderer(self.font_file, face, 24) as renderer:
                    renderer.glyph(0x4E2D)
                break
            except ValueError as error:
                if "substituted face" not in str(error):
                    raise
        else:
            self.fail("Neither known explicit font face matched GDI")
        # Warmed up before counting native handles in this process.
        baseline = user.GetGuiResources(process, 0)
        for _ in range(3):
            with TFT.GdiRenderer(self.font_file, face, 24) as renderer:
                item, pixels = renderer.glyph(0x4E2D)
                self.assertGreater(item.width * item.height, 0)
                self.assertEqual(len(pixels), item.width * item.height)
                self.assertTrue(any(pixels))
                self.assertLessEqual(max(pixels), 64)
                self.assertGreater(item.advance, 0)
                space, blank = renderer.glyph(0x20)
                self.assertEqual(blank, b"")
                self.assertGreater(space.advance, 0)
                with self.assertRaisesRegex(ValueError, "does not cover"):
                    renderer.glyph(0x0378)
            self.assertFalse(renderer.registered)
            self.assertIsNone(renderer.font)
            renderer.close()
            with self.assertRaisesRegex(ValueError, "closed"):
                renderer.glyph(0x4E2D)
        with TFT.GdiRenderer(self.font_file, "Nonexistent TFT Test Face", 24) as renderer:
            with self.assertRaisesRegex(ValueError, "substituted face"):
                renderer.glyph(0x4E2D)
            self.assertFalse(renderer.registered)
            self.assertIsNone(renderer.dc)
        self.assertEqual(user.GetGuiResources(process, 0), baseline)


if __name__ == "__main__":
    unittest.main()
