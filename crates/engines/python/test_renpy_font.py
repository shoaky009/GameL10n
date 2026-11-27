"""Tests for Ren'Py Chinese font coverage detection and FontGroup fallback generation."""
import ast
import importlib
import json
from pathlib import Path
import pickle
import struct
import sys
import tempfile
import textwrap
import unittest
from unittest.mock import patch
import zlib

ROOT = Path(__file__).resolve().parents[3]


def _make_sfnt_with_format4_cmap(ranges):
    """Build a minimal valid SFNT container with a Unicode format-4 cmap subtable."""
    segments = [*ranges, (0xFFFF, 0xFFFF)]
    seg_count = len(segments)
    seg_count_x2 = seg_count * 2
    end_codes = b"".join(struct.pack(">H", end) for _, end in segments)
    start_codes = b"".join(struct.pack(">H", start) for start, _ in segments)
    deltas = b"".join(struct.pack(">H", (1 - start) & 0xFFFF) if start < 0xFFFF else struct.pack(">H", 1)
                      for start, _ in segments)
    range_offsets = b"\x00\x00" * seg_count
    subtable_body = struct.pack(">HHHHHH", 4, 14 + seg_count * 8 + 2, 0, seg_count_x2, 2, 0) + struct.pack(">H", 0)
    subtable_body += end_codes + b"\x00\x00" + start_codes + deltas + range_offsets
    # Fix exact subtable length in bytes 2..4.
    subtable = subtable_body[:2] + struct.pack(">H", len(subtable_body)) + subtable_body[4:]
    cmap = struct.pack(">HH", 0, 1) + struct.pack(">HHI", 3, 1, 12) + subtable
    # SFNT header (12 bytes) + 1 table record (16 bytes) + cmap table at offset 28.
    header = struct.pack(">IHHHH", 0x00010000, 1, 16, 0, 0)
    record = b"cmap" + struct.pack(">III", 0, 28, len(cmap))
    return header + record + cmap


def _rpa_file(path, members):
    data, entries = bytearray(b" " * 34), {}
    for name, content in members.items():
        entries[name] = [(len(data), len(content))]
        data.extend(content)
    offset = len(data)
    data.extend(zlib.compress(pickle.dumps(entries, protocol=3)))
    data[:34] = f"RPA-3.0 {offset:016x} 00000000\n".encode("ascii")
    path.write_bytes(data)


class RenpyFontTest(unittest.TestCase):
    def setUp(self):
        pin = json.loads((ROOT / "third_party/engine-tools/current.json").read_text(encoding="utf-8-sig"))
        self.library = str(ROOT / "third_party/engine-tools/references" / pin["revision"] / "library.zip")

    def _import_renpy_font(self):
        sys.path.insert(0, str(ROOT))
        sys.path.insert(0, self.library)
        return importlib.import_module("adapters.components.renpy_font")

    def test_sfnt_cmap_detects_missing_and_covered_chinese_glyphs(self):
        with patch.object(sys, "path", [str(ROOT), self.library, *sys.path]):
            renpy_font = importlib.import_module("adapters.components.renpy_font")
            latin_only = _make_sfnt_with_format4_cmap([(0x0020, 0x007E)])
            latin_report = renpy_font.inspect_font_bytes(latin_only)
            self.assertEqual(latin_report["status"], "inspected")
            self.assertEqual(latin_report["covered"], 0)
            self.assertGreater(latin_report["missingCount"], 100)

            cjk_font = _make_sfnt_with_format4_cmap([
                (0x0020, 0x007E),
                (0x2014, 0x2026),
                (0x3000, 0x303F),
                (0x4E00, 0x9FFF),
                (0xFF00, 0xFFEF),
            ])
            cjk_report = renpy_font.inspect_font_bytes(cjk_font)
            self.assertEqual(cjk_report["status"], "inspected")
            self.assertEqual(cjk_report["missingCount"], 0)
            self.assertEqual(cjk_report["covered"], cjk_report["tested"])

    def test_detect_missing_fonts_from_gui_and_archive_and_render_valid_script(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(sys, "path", [str(ROOT), self.library, *sys.path]):
            renpy_font = importlib.import_module("adapters.components.renpy_font")
            root = Path(directory)
            game_dir = root / "game"
            game_dir.mkdir()
            latin_only = _make_sfnt_with_format4_cmap([(0x0020, 0x007E)])
            cjk_font = _make_sfnt_with_format4_cmap([
                (0x0020, 0x007E),
                (0x2014, 0x2026),
                (0x3000, 0x303F),
                (0x4E00, 0x9FFF),
                (0xFF00, 0xFFEF),
            ])
            archive_path = game_dir / "archive.rpa"
            _rpa_file(archive_path, {
                "Latin.ttf": latin_only,
                "Chinese.ttf": cjk_font,
            })
            gui_rpy = root / "gui.rpy"
            gui_rpy.write_text(
                'define gui.text_font = "Latin.ttf"\n'
                'define gui.name_text_font = "DejaVuSans.ttf"\n'
                'define gui.interface_text_font = "Chinese.ttf"\n',
                encoding="utf-8",
            )
            missing = renpy_font.detect_missing_chinese_fonts(
                root, [archive_path], "game", [("gui.rpy", gui_rpy)]
            )
            self.assertEqual(missing, ["DejaVuSans.ttf", "Latin.ttf"])

            script = renpy_font.render_font_overlay_script(missing)
            self.assertIn("init 999 python:", script)
            self.assertIn("FontGroup()", script)
            self.assertIn("config.allow_sysfonts = True", script)
            self.assertIn("config.font_name_map", script)
            self.assertIn("'Latin.ttf'", script)
            # Extract and parse the Python body inside `init 999 python:`.
            body = script.split("init 999 python:\n", 1)[1]
            ast.parse(textwrap.dedent(body))

            # If all configured fonts cover Simplified Chinese, no fallback is requested.
            gui_rpy.write_text('define gui.text_font = "Chinese.ttf"\n', encoding="utf-8")
            self.assertEqual(
                renpy_font.detect_missing_chinese_fonts(root, [archive_path], "game", [("gui.rpy", gui_rpy)]),
                [],
            )

    def test_font_companion_follows_changed_dialogue_in_both_patch_modes(self):
        fixtures = ROOT / "crates/engines/tests/fixtures"
        with tempfile.TemporaryDirectory() as directory, patch.object(
            sys, "path", [str(ROOT), str(fixtures / "renpy-rpyc-reference-e9b8018.zip"), str(fixtures), *sys.path]
        ):
            renpy_rpyc_adapter = importlib.import_module("adapters.components.renpy_rpyc_adapter")
            mixed = importlib.import_module("adapters.components.renpy_mixed_adapter")
            renpy_font = importlib.import_module("adapters.components.renpy_font")
            fixture_mod = importlib.import_module("renpy_rpyc_fixture")

            root = Path(directory)
            package = root / "package"
            (package / "tools").mkdir(parents=True)
            config = dict(
                schemaVersion=1,
                templateVersion=1,
                templateId="rpa3-renpy-rpyc",
                format="Renpy",
                archiveComponent="archive.rpa3",
                scriptExtensions=[".rpyc"],
            )
            (package / "tools/adapter-template.json").write_text(json.dumps(config), encoding="utf-8")

            source = root / "source"
            (source / "game").mkdir(parents=True)
            original = fixture_mod.sample()
            latin_only = _make_sfnt_with_format4_cmap([(0x0020, 0x007E)])
            _rpa_file(source / "game/archive.rpa", {
                "story.rpyc": original,
                "gui.rpyc": original,
                "Latin.ttf": latin_only,
            })

            base_req = dict(
                protocolVersion=1,
                adapterDirectory=str(package),
                sourceDirectory=str(source),
                resourceFiles=["game/archive.rpa"],
            )
            units = renpy_rpyc_adapter.execute(dict(base_req, operation="extract"))["extraction"]["units"]
            target_unit = next(u for u in units if "story.rpyc" in u["context"])

            # Patch extract_gui_fonts_from_rpyc to report Latin.ttf for gui.rpyc.
            with patch.object(renpy_font, "extract_gui_fonts_from_rpyc", return_value={"text_font": "Latin.ttf"}):
                empty_out = root / "empty"
                empty_build = renpy_rpyc_adapter.execute(dict(
                    base_req, operation="build", outputDirectory=str(empty_out), translations={}
                ))["build"]
                self.assertEqual(empty_build["files"], [])

                # Identity translations (all values equal to sourceText) must skip rpyc.patch.
                identity_out = root / "identity"
                identity_map = {u["id"]: u["sourceText"] for u in units}
                with patch.object(renpy_rpyc_adapter.rpyc, "patch", side_effect=AssertionError("should skip patch")):
                    identity_build = renpy_rpyc_adapter.execute(dict(
                        base_req, operation="build", outputDirectory=str(identity_out), translations=identity_map
                    ))["build"]
                self.assertEqual(identity_build["mode"], "overlay")
                self.assertEqual(identity_build["files"], [])

                loose = "game/loose.rpyc"
                (source / loose).write_bytes(original)
                mixed_config = dict(config, templateId="renpy-mixed", scriptExtensions=[".rpy", ".rpyc"])
                archive_file = "game/archive.rpa"
                trials = [
                    (renpy_rpyc_adapter, config, [archive_file], "overlay"),
                    (renpy_rpyc_adapter, dict(config, buildMode="replace"), [archive_file], "replace"),
                    (mixed, mixed_config, [archive_file], "overlay"),
                    (mixed, mixed_config, [archive_file, loose], "replace"),
                ]
                font_file = "game/zz_gamel10n_font.rpy"
                text = "中文译文 [player] {b}测试{/b}"
                for adapter, settings, files, mode in trials:
                    with self.subTest(template=settings["templateId"], mode=mode):
                        out = root / (settings["templateId"] + "-" + mode)
                        (package / "tools/adapter-template.json").write_text(json.dumps(settings), encoding="utf-8")
                        req = dict(base_req, resourceFiles=files)
                        built = adapter.execute(dict(req, operation="build", outputDirectory=str(out),
                                                     translations={target_unit["id"]: text}))["build"]
                        self.assertEqual(built["mode"], mode)
                        scripts = ["game/story.rpyc"] if mode == "overlay" else files
                        self.assertEqual(built["files"], sorted([*scripts, font_file]))
                        output = dict(outputDirectory=str(out), buildMode=mode, outputFiles=built["files"])
                        extract = dict(req, **output) if mode == "overlay" else dict(
                            req, sourceDirectory=str(out), resourceFiles=built["files"])
                        units = adapter.execute(dict(extract, operation="extract"))["extraction"]["units"]
                        self.assertEqual(next(u["sourceText"] for u in units if u["id"] == target_unit["id"]), text)
                        validate = dict(req, operation="validate", **output)
                        self.assertTrue(adapter.execute(validate)["validation"]["passed"])
                        (out / font_file).write_text("# incomplete\n", encoding="utf-8")
                        with self.assertRaisesRegex(ValueError, "字体回退脚本"):
                            adapter.execute(validate)
                        (out / font_file).unlink()
                        with self.assertRaisesRegex(ValueError, "字体回退脚本"):
                            adapter.execute(dict(validate, outputFiles=scripts))

    def test_paired_archived_ren_py_is_ignored_while_unpaired_is_rejected(self):
        fixtures = ROOT / "crates/engines/tests/fixtures"
        with tempfile.TemporaryDirectory() as directory, patch.object(
            sys, "path", [str(ROOT), str(fixtures / "renpy-rpyc-reference-e9b8018.zip"), str(fixtures), *sys.path]
        ):
            renpy_rpyc_adapter = importlib.import_module("adapters.components.renpy_rpyc_adapter")
            mixed = importlib.import_module("adapters.components.renpy_mixed_adapter")
            fixture_mod = importlib.import_module("renpy_rpyc_fixture")

            root = Path(directory)
            package = root / "package"
            (package / "tools").mkdir(parents=True)
            source = root / "source"
            (source / "game").mkdir(parents=True)
            original = fixture_mod.sample()
            archive_path = source / "game/archive.rpa"
            base_req = dict(
                protocolVersion=1,
                adapterDirectory=str(package),
                sourceDirectory=str(source),
                resourceFiles=["game/archive.rpa"],
                operation="extract",
            )
            cases = [
                (renpy_rpyc_adapter, dict(schemaVersion=1, templateVersion=1, templateId="rpa3-renpy-rpyc",
                                           format="Renpy", archiveComponent="archive.rpa3", scriptExtensions=[".rpyc"]),
                 "加载顺序"),
                (mixed, dict(schemaVersion=1, templateVersion=1, templateId="renpy-mixed",
                             format="Renpy", archiveComponent="archive.rpa3", scriptExtensions=[".rpy", ".rpyc"]),
                 "未覆盖"),
            ]
            for adapter, cfg, error_pattern in cases:
                with self.subTest(template=cfg["templateId"]):
                    (package / "tools/adapter-template.json").write_text(json.dumps(cfg), encoding="utf-8")
                    _rpa_file(archive_path, {
                        "logic_ren.rpyc": original,
                        "logic_ren.py": b'"""renpy\ninit python:\n    pass\n"""\n',
                    })
                    units = adapter.execute(base_req)["extraction"]["units"]
                    self.assertEqual(len(units), 4)
                    _rpa_file(archive_path, {
                        "logic_ren.rpyc": original,
                        "orphan_ren.py": b'"""renpy\ninit python:\n    pass\n"""\n',
                    })
                    with self.assertRaisesRegex(ValueError, error_pattern):
                        adapter.execute(base_req)


if __name__ == "__main__":
    unittest.main()
