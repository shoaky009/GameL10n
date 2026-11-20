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

from adapters.components import qlie_pack_adapter
from crates.engines.python.fixtures.qlie_installation import make_installation


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
