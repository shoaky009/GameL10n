"""PE import preservation and independent Windows loader/rendering checks."""
import hashlib
import importlib.util
import os
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[3]
SPEC = importlib.util.spec_from_file_location(
    "gdiplus_font_bridge", ROOT / "adapters/components/gdiplus_font_bridge.py")
BRIDGE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BRIDGE)


def fixture():
    """Two import DLLs, interleaved selected/nonselected entries and an overlay."""
    data = bytearray(0xC00)
    data[:2] = b"MZ"
    struct.pack_into("<I", data, 60, 0x80)
    data[0x80:0x84] = b"PE\0\0"
    struct.pack_into("<HHIIIHH", data, 0x84, 0x14C, 2, 0, 0, 0, 224, 0x102)
    opt = 0x98
    struct.pack_into("<H", data, opt, 0x10B)
    struct.pack_into("<I", data, opt + 8, 0x600)
    struct.pack_into("<II", data, opt + 32, 0x1000, 0x200)
    struct.pack_into("<II", data, opt + 56, 0x3000, 0x400)
    struct.pack_into("<I", data, opt + 92, 16)
    struct.pack_into("<II", data, opt + 104, 0x2000, 60)
    # Relocation metadata must survive; this is structural input, not an EXE to run.
    struct.pack_into("<II", data, opt + 96 + 5 * 8, 0x2500, 12)
    struct.pack_into("<H", data, opt + 70, 0x140)
    table = opt + 224
    struct.pack_into("<8s6I2HI", data, table, b".text", 0x200, 0x1000,
                     0x200, 0x400, 0, 0, 0, 0, 0x60000020)
    struct.pack_into("<8s6I2HI", data, table + 40, b".rdata", 0x600, 0x2000,
                     0x600, 0x600, 0, 0, 0, 0, 0x40000040)
    data[0x400:0x600] = bytes(range(256)) * 2
    struct.pack_into("<5I", data, 0x600, 0x2240, 0, 0, 0x2100, 0x2280)
    struct.pack_into("<5I", data, 0x614, 0x2260, 0, 0, 0x2120, 0x22C0)
    data[0x700:0x70C] = b"gdiplus.dll\0"
    data[0x720:0x72D] = b"kernel32.dll\0"
    names = [BRIDGE.FUNCTIONS[0], "GdipMeasureString", BRIDGE.FUNCTIONS[1],
             "GdipCreateBitmapFromScan0", BRIDGE.FUNCTIONS[2], BRIDGE.FAMILY_QUERY]
    for index, name in enumerate(names):
        rva = 0x2300 + index * 0x40
        raw = rva - 0x2000 + 0x600
        encoded = name.encode("ascii") + b"\0"
        data[raw + 2:raw + 2 + len(encoded)] = encoded
        struct.pack_into("<I", data, 0x840 + index * 4, rva)
        struct.pack_into("<I", data, 0x880 + index * 4, rva)
    struct.pack_into("<II", data, 0x860, 0x8000002A, 0)
    struct.pack_into("<II", data, 0x8C0, 0x8000002A, 0)
    return bytes(data) + b"original overlay"


class ImportRewriteTest(unittest.TestCase):
    def test_keeps_iat_code_overlay_and_aslr_while_splitting_import_runs(self):
        original = fixture()
        before = BRIDGE.PE(original)
        patched, redirected = BRIDGE.patch_imports(original, list(BRIDGE.FUNCTIONS))
        after = BRIDGE.PE(patched)
        self.assertEqual(patched[before.headers:len(original)], original[before.headers:])
        self.assertEqual(after.directory(5), before.directory(5))
        self.assertEqual(after.u16(after.optional + 70), before.u16(before.optional + 70))
        slots = {iat: (module["library"], symbol) for module in after.imports()
                 for _, symbol, iat in module["entries"]}
        self.assertEqual(slots[0x2280], (BRIDGE.DLL, BRIDGE.FUNCTIONS[0]))
        self.assertEqual(slots[0x2284], ("gdiplus.dll", "GdipMeasureString"))
        self.assertEqual(slots[0x228C], ("gdiplus.dll", "GdipCreateBitmapFromScan0"))
        self.assertEqual(slots[0x22C0], ("kernel32.dll", "#42"))
        self.assertEqual(slots[0x2294], (BRIDGE.DLL, BRIDGE.FAMILY_QUERY))
        self.assertEqual(len(redirected), 4)

    def test_refuses_unsupported_images_instead_of_changing_code_or_sections(self):
        original = fixture()
        patched, _ = BRIDGE.patch_imports(original, [BRIDGE.FUNCTIONS[0]])
        cases = [patched, original[:400]]
        for directory in (4, 14):
            altered = bytearray(original)
            struct.pack_into("<II", altered, 0x98 + 96 + directory * 8, 0x2500, 12)
            cases.append(altered)
        altered = bytearray(original)
        altered[0x98 + 224 + 80] = 1  # occupied next section header
        cases.append(altered)
        altered = bytearray(original)
        altered[0x902:0x902 + len(BRIDGE.FUNCTIONS[0])] = b"X" * len(BRIDGE.FUNCTIONS[0])
        cases.append(altered)
        for data in cases:
            with self.subTest(size=len(data)), self.assertRaises(ValueError):
                BRIDGE.patch_imports(data, [BRIDGE.FUNCTIONS[0]])

    def test_output_paths_reject_windows_devices_and_traversal(self):
        for path in ("../game.exe", "C:/game.exe", "\\game.exe", "dir/CON.exe",
                     "dir/game.exe:stream", "dir./game.exe", "dir/a?b.exe"):
            with self.subTest(path=path), self.assertRaises(ValueError):
                BRIDGE._relative(path)


@unittest.skipUnless(os.name == "nt", "Windows loader and GDI+ required")
class WindowsLoaderTest(unittest.TestCase):
    def test_redirected_executable_loads_and_draws_through_real_iat(self):
        configured = os.environ.get("FONTBRIDGE_TEST_EXE")
        harness = Path(configured) if configured else ROOT / "target/fontbridge/Release/FontBridgeTests.exe"
        if not harness.is_file():
            self.skipTest("build-fontbridge.ps1 -Test prepares the native harness")
        runtime = ROOT / "target/runtime/adapter-components"
        font = Path(os.environ.get("FONTBRIDGE_TEST_FONT", str(Path(os.environ["WINDIR"]) / "Fonts/segoeui.ttf")))
        face = os.environ.get("FONTBRIDGE_TEST_FACE", "Segoe UI")
        with tempfile.TemporaryDirectory(dir=ROOT / "target") as temporary:
            root = Path(temporary)
            source = root / harness.name
            shutil.copyfile(harness, source)
            data = source.read_bytes()
            # Use the public prepare path, including conflict preflight and UTF-16 config.
            old_file = BRIDGE.__file__
            BRIDGE.__file__ = str(runtime / "gdiplus_font_bridge.py")
            try:
                request = dict(input=str(source), expectedSha256=hashlib.sha256(data).hexdigest(),
                               relativePath=source.name, functions=list(BRIDGE.FUNCTIONS),
                               fontFile=str(font), fontFace=face, matchFace="BridgeAlias",
                               mode="replace-matching", outputDirectory=str(root / "output"))
                result = BRIDGE.prepare(request)
                self.assertEqual(result, BRIDGE.prepare(request))
                output = root / "output" / source.name
                self.assertFalse(result["runtimeTested"])
                self.assertEqual(source.read_bytes(), data)
                process = subprocess.run([str(output), "imports", "replace", face],
                                         cwd=root / "output", capture_output=True, timeout=45)
                self.assertEqual(process.returncode, 0, (process.stdout + process.stderr).decode(errors="replace"))
                config = root / "output" / BRIDGE.CONFIG
                config.write_text("conflict", encoding="utf-8")
                with self.assertRaises(ValueError):
                    BRIDGE.prepare(request)
                self.assertEqual(config.read_text(encoding="utf-8"), "conflict")
            finally:
                BRIDGE.__file__ = old_file


if __name__ == "__main__":
    unittest.main()
