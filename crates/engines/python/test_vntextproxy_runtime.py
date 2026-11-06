"""Static runtime preparation, pinned inputs and output containment checks."""

import hashlib
import importlib.util
from pathlib import Path
import struct
import tempfile
import unittest
from unittest.mock import patch
import zipfile


COMPONENT = Path(__file__).resolve().parents[3] / "adapters/components/vntextproxy_runtime.py"
SPEC = importlib.util.spec_from_file_location("vntextproxy_runtime", COMPONENT)
RUNTIME = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNTIME)


class VNTextProxyRuntimeTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        cwd = patch.object(RUNTIME.Path, "cwd", return_value=self.root)
        cwd.start()
        self.addCleanup(cwd.stop)

    def fixture(self, machine=0x14C, magic=0x10B, dll=True):
        data = bytearray(512)
        data[:2] = b"MZ"
        struct.pack_into("<I", data, 0x3C, 0x80)
        data[0x80:0x84] = b"PE\0\0"
        struct.pack_into("<H", data, 0x84, machine)
        struct.pack_into("<HHH", data, 0x94, 224, 0x2000 if dll else 0, magic)
        archive = self.root / "download.zip"
        with zipfile.ZipFile(archive, "w") as package:
            package.writestr("VNTextProxy/d3d9.dll", data)
            package.writestr("VNTextProxy/version.dll", b"unselected")
            package.writestr("../outside.txt", b"must never extract")
            package.writestr("VNTextPatch/VNTextPatch.exe", b"must never extract")
        digest = patch.object(RUNTIME, "ARCHIVE_SHA256", hashlib.sha256(archive.read_bytes()).hexdigest())
        member = patch.dict(RUNTIME.PROXIES, {"d3d9.dll": hashlib.sha256(data).hexdigest()})
        digest.start()
        member.start()
        self.addCleanup(member.stop)
        self.addCleanup(digest.stop)
        return dict(archive=str(archive), proxy="d3d9.dll")

    def test_default_description_does_not_prepare_or_claim_runtime_validation(self):
        result = RUNTIME.execute({})
        self.assertTrue(result["success"])
        self.assertFalse(result["runtimeTested"])
        self.assertEqual(len(result["proxies"]), 8)
        self.assertEqual(list(self.root.iterdir()), [])

    def test_check_is_read_only_and_prepare_selects_one_dll_with_both_licenses(self):
        request = self.fixture()
        before = set(self.root.rglob("*"))
        checked = RUNTIME.execute(dict(operation="check", **request))
        self.assertTrue(checked["success"], checked)
        self.assertFalse(checked["runtimeTested"])
        self.assertEqual(set(self.root.rglob("*")), before)
        prepared = RUNTIME.execute(dict(operation="prepare", output="src/runtime/proxy", **request))
        self.assertTrue(prepared["success"], prepared)
        self.assertFalse(prepared["runtimeTested"])
        self.assertEqual({item["path"] for item in prepared["files"]}, {
            "src/runtime/proxy/d3d9.dll", "src/runtime/proxy/licenses/VNTextProxy-LICENSE.txt"})
        notices = (self.root / prepared["files"][1]["path"]).read_text(encoding="utf-8")
        self.assertIn("Copyright (c) 2021 arcusmaximus", notices)
        self.assertIn("Copyright (c) Microsoft Corporation", notices)
        self.assertFalse((self.root / "outside.txt").exists())
        repeated = RUNTIME.execute(dict(operation="prepare", output="src/runtime/proxy", **request))
        self.assertTrue(all(item["reused"] for item in repeated["files"]))

    def test_unpinned_archive_or_unknown_proxy_does_not_create_output(self):
        archive = self.root / "untrusted.zip"
        archive.write_bytes(b"different release")
        for proxy in ("d3d9.dll", "unknown.dll", "../d3d9.dll", "D3D9.DLL"):
            result = RUNTIME.execute(dict(operation="prepare", archive=str(archive),
                                          proxy=proxy, output="src/runtime"))
            self.assertFalse(result["success"], result)
        self.assertFalse((self.root / "src").exists())

    def test_changed_member_is_rejected_after_archive_hash_passes(self):
        request = self.fixture()
        with patch.dict(RUNTIME.PROXIES, {"d3d9.dll": "0" * 64}):
            result = RUNTIME.execute(dict(operation="prepare", output="src/runtime", **request))
        self.assertIn("member SHA256", result["error"])
        self.assertFalse((self.root / "src").exists())

    def test_x64_pe32plus_and_non_dll_members_are_rejected(self):
        for machine, magic, dll in ((0x8664, 0x20B, True), (0x14C, 0x20B, True), (0x14C, 0x10B, False)):
            request = self.fixture(machine=machine, magic=magic, dll=dll)
            result = RUNTIME.execute(dict(operation="prepare", output="src/runtime", **request))
            self.assertFalse(result["success"], result)
        self.assertFalse((self.root / "src").exists())

    def test_unsafe_output_paths_are_rejected_before_creating_directories(self):
        request = self.fixture()
        for output in ("../escape", "src/../escape", "/absolute", "C:/absolute", "C:relative",
                       "\\\\server\\share", "src/stream:ads", "src/trailing. ", ".", "src//runtime"):
            result = RUNTIME.execute(dict(operation="prepare", output=output, **request))
            self.assertFalse(result["success"], (output, result))
        self.assertFalse((self.root / "src").exists())

    def test_conflicting_license_prevents_the_dll_write(self):
        request = self.fixture()
        notice = self.root / "src/runtime/licenses/VNTextProxy-LICENSE.txt"
        notice.parent.mkdir(parents=True)
        notice.write_text("keep local changes", encoding="utf-8")
        result = RUNTIME.execute(dict(operation="prepare", output="src/runtime", **request))
        self.assertFalse(result["success"], result)
        self.assertEqual(notice.read_text(encoding="utf-8"), "keep local changes")
        self.assertFalse((self.root / "src/runtime/d3d9.dll").exists())

    def test_output_directory_links_cannot_escape_the_workspace(self):
        request = self.fixture()
        with tempfile.TemporaryDirectory() as outside:
            link = self.root / "linked"
            try:
                link.symlink_to(outside, target_is_directory=True)
            except OSError:
                try:
                    import _winapi
                    _winapi.CreateJunction(outside, str(link))
                except (ImportError, OSError):
                    self.skipTest("Directory links require local OS permission")
            result = RUNTIME.execute(dict(operation="prepare", output="linked/proxy", **request))
            self.assertFalse(result["success"], result)
            self.assertEqual(list(Path(outside).iterdir()), [])


if __name__ == "__main__":
    unittest.main()
