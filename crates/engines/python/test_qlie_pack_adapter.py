"""Keyed QLIE extraction, replacements and readback across independent volumes."""
import json
from pathlib import Path
import sys
import tempfile
import unittest


REPO = Path(__file__).resolve().parents[3]
TOOLS = REPO / "third_party/engine-tools"
PIN = json.loads((TOOLS / "current.json").read_text(encoding="utf-8"))
sys.path[:0] = [str(REPO), str(TOOLS / "references" / PIN["revision"] / "library.zip")]

from adapters.components import qlie_pack_adapter, qlie_samples
from crates.engines.python.fixtures.qlie_installation import make_installation, make_installation31


class QliePackAdapterTest(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name) / "game"
        self.root.mkdir()
        make_installation(self.root)
        self.output = Path(directory.name) / "patch"
        self.files = [path.relative_to(self.root).as_posix() for path in self.root.rglob("*") if path.is_file()]
        probe = self.execute("probe")["probe"]
        self.files = probe["requiredFiles"]
        self.units = self.execute("extract")["extraction"]["units"]

    def execute(self, operation, **parameters):
        request = dict(operation=operation, sourceDirectory=str(self.root), resourceFiles=self.files,
                       outputDirectory=str(self.output))
        return qlie_pack_adapter.execute(dict(request, **parameters))

    def test_rebuild_targets_original_volume_when_script_names_match(self):
        first = next(unit for unit in self.units if unit["sourceText"] == "こんにちは")
        update = next(unit for unit in self.units if unit["sourceText"] == "別の会話")
        name = next(unit for unit in self.units if unit["kind"] == "name")
        self.assertEqual(first["locator"], update["locator"])
        self.assertNotEqual(first["id"], update["id"])
        self.assertIn("GameData/system.pack", self.files)
        self.assertNotIn("GameData/media.pack", self.files)

        edits = {update["id"]: "更新済み", name["id"]: "別名"}
        build = self.execute("build", translations=edits)["build"]
        self.assertTrue(self.execute("validate", translations=edits, outputFiles=build["files"])["validation"]["passed"])
        result = self.execute("extract", sourceDirectory=str(self.output))["extraction"]["units"]
        self.assertEqual(result, [dict(unit, sourceText=edits.get(unit["id"], unit["sourceText"])) for unit in self.units])

    def test_original_text_preserves_all_required_bytes(self):
        originals = {unit["id"]: unit["sourceText"] for unit in self.units}
        self.execute("build", translations=originals)
        for path in self.files:
            self.assertEqual((self.output / path).read_bytes(), (self.root / path).read_bytes())

    def test_unencodable_translation_fails_before_output(self):
        with self.assertRaisesRegex(ValueError, "需要已确认的中文编码与字体方案"):
            self.execute("build", translations={self.units[0]["id"]: "汉"})
        self.assertFalse(self.output.exists())


class QliePack31Test(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name) / "game"
        self.root.mkdir()
        make_installation31(self.root)
        self.output = Path(directory.name) / "patch"
        self.volumes = sorted(path.relative_to(self.root).as_posix() for path in (self.root / "GameData").glob("*.pack"))

    def request(self, **fields):
        return dict(sourceDirectory=str(self.root), outputDirectory=str(self.output), **fields)

    def test_unicode_rebuild_preserves_ruby_and_volume_identity(self):
        probe = qlie_pack_adapter.execute(self.request(operation="probe", resourceFiles=self.volumes))
        files = probe["probe"]["requiredFiles"]
        self.assertNotIn("GameData/media.pack", files)
        self.assertEqual(probe["probe"]["adapterId"], "template.qlie-pack31-imos")
        units = qlie_pack_adapter.execute(self.request(operation="extract", resourceFiles=files))["extraction"]["units"]
        ruby = next(unit for unit in units if unit["protectedTokens"])
        self.assertEqual(ruby["protectedTokens"], ["[rb,朝,あさ]"])
        other = next(unit for unit in units if unit["resourcePath"] == "GameData/update.pack")
        self.assertNotEqual(ruby["id"], other["id"])
        edits = {ruby["id"]: "保留[rb,朝,あさ]的中文试验文本"}
        build = qlie_pack_adapter.execute(self.request(operation="build", resourceFiles=files, translations=edits))
        validation = qlie_pack_adapter.execute(self.request(operation="validate", resourceFiles=files,
                                                            translations=edits, outputFiles=build["build"]["files"]))
        self.assertTrue(validation["validation"]["passed"])
        result = qlie_pack_adapter.execute(dict(operation="extract", sourceDirectory=str(self.output),
                                                resourceFiles=files))["extraction"]["units"]
        self.assertEqual(result, [dict(unit, sourceText=edits.get(unit["id"], unit["sourceText"])) for unit in units])
        self.assertEqual((self.output / "GameData/update.pack").read_bytes(),
                         (self.root / "GameData/update.pack").read_bytes())

    def test_probe_rechecks_sample_payload_when_index_is_unchanged(self):
        request = dict(files={path: self.root / path for path in self.volumes}, archives=self.volumes)
        first = qlie_samples.probe(request)
        self.assertEqual(first["trials"][0]["archiveRoundtripStatus"], "passed")
        reused = qlie_samples.probe(dict(request, previous=first))
        self.assertTrue(reused["reused"])
        path = self.root / first["archive"]
        data = bytearray(path.read_bytes())
        data[10] ^= 1
        path.write_bytes(data)
        fresh = qlie_samples.probe(dict(request, previous=first))
        self.assertNotIn("reused", fresh)

    def test_uninstall_archive_needs_an_executable_reference_and_a_resource_only_index(self):
        files = ["UnInstallData.pack", "UnInstall.exe"]
        self.assertEqual(qlie_samples.inspect_uninstall_archive(self.root, files)["status"], "confirmed")
        (self.root / "UnInstallData.pack").write_bytes((self.root / "GameData/story.pack").read_bytes())
        self.assertEqual(qlie_samples.inspect_uninstall_archive(self.root, files)["reason"], "unexpected-member-types")
        (self.root / "UnInstall.exe").write_bytes(b"MZ missing archive reference")
        self.assertEqual(qlie_samples.inspect_uninstall_archive(self.root, files)["reason"], "missing-archive-reference")
