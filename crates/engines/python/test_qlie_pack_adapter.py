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

from adapters.components import (
    qlie_archive,
    qlie_bitmap,
    qlie_display,
    qlie_pack31,
    qlie_pack_adapter,
    qlie_samples,
    qlie_script,
    qlie_sfnt,
    qlie_systemfont,
)
from crates.engines.python.fixtures.qlie_installation import (
    EXTERNAL_KEY,
    GAME_KEY,
    key_resource_exe,
    make_installation,
    make_installation31,
    make_pack,
    make_pack31,
)


class QlieBitmapTest(unittest.TestCase):
    def test_dialogue_strokes_grow_around_their_center_and_stay_inside_the_cell(self):
        rows = [0] * 64
        rows[0] = 1 << 63
        rows[32] = 1 << 30
        mask = qlie_bitmap.dialogue_mask(b"".join(row.to_bytes(8, "big") for row in rows))
        pixels = {(x, y) for y in range(64) for x in range(64)
                  if mask[y * 8 + x // 8] & (128 >> (x % 8))}
        self.assertEqual(pixels, {(x, y) for x in range(64) for y in range(64)
                                  if x + y <= 2 or abs(x - 33) + abs(y - 32) <= 2})


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

    def test_writer_tail_verifies_packkey_signature_and_mode1_large_size_seed(self):
        path = self.root / "GameData/story.pack"
        with path.open("rb") as stream:
            index = qlie_pack31.read_index(stream)
            tail = qlie_pack31.writer_tail(stream, index)
        self.assertGreaterEqual(len(tail), qlie_pack31.PACKHASH.size + qlie_pack31.PACKKEY_PREFIX.size + qlie_pack31.KEY_AREA_BYTES)

        corrupted = bytearray(path.read_bytes())
        sig_offset = len(corrupted) - qlie_pack31.FOOTER.size - qlie_pack31.KEY_AREA_BYTES - qlie_pack31.PACKKEY_PREFIX.size
        corrupted[sig_offset] ^= 1
        bad = self.root / "GameData/bad_sig.pack"
        bad.write_bytes(corrupted)
        with bad.open("rb") as stream:
            bad_index = qlie_pack31.read_index(stream)
            with self.assertRaisesRegex(ValueError, "HashVer1.4"):
                qlie_pack31.writer_tail(stream, bad_index)

        # Corrupted PACKKEY hash_size must also be rejected.
        corrupted_size = bytearray(path.read_bytes())
        hsize_offset = len(corrupted_size) - qlie_pack31.FOOTER.size - qlie_pack31.KEY_AREA_BYTES - 4
        corrupted_size[hsize_offset] ^= 1
        bad_size = self.root / "GameData/bad_size.pack"
        bad_size.write_bytes(corrupted_size)
        with bad_size.open("rb") as stream:
            bad_size_index = qlie_pack31.read_index(stream)
            with self.assertRaisesRegex(ValueError, "HashVer1.4"):
                qlie_pack31.writer_tail(stream, bad_size_index)

        # chain_crypt must roundtrip arbitrary 8-byte aligned payloads.
        plain = b"8hr48uky,8ugi8ewra4g8d5vbf5hb5s6"
        cipher = qlie_pack31.chain_crypt(0x123456, plain, encrypt=True)
        self.assertNotEqual(cipher, plain)
        self.assertEqual(qlie_pack31.chain_crypt(0x123456, cipher, encrypt=False), plain)

        # Corrupted name_hash inside FilePack 3.1 HashVer1.4 lookup table must be rejected.
        import struct
        raw_pack = bytearray(path.read_bytes())
        with path.open("rb") as stream:
            idx = qlie_pack31.read_index(stream)
            tail = qlie_pack31.writer_tail(stream, idx)
        sig, buckets, count, ordinals, stored, is_comp, safety, ver, res = qlie_pack31.PACKHASH.unpack_from(tail, 0)
        self.assertEqual(is_comp, 0)
        plain_hash = bytearray(qlie_pack31.chain_crypt(qlie_pack31.HASH_CRYPT_KEY, tail[68:68 + stored], encrypt=False))
        # Locate the first non-empty bucket's first entry name_hash and flip 1 bit.
        pos = 0
        for _ in range(buckets):
            n = int.from_bytes(plain_hash[pos:pos + 4], "little")
            pos += 4
            if n:
                ch_len = int.from_bytes(plain_hash[pos:pos + 2], "little")
                pos += 2 + ch_len * 2 + 8
                plain_hash[pos] ^= 1
                break
        bad_payload = qlie_pack31.chain_crypt(qlie_pack31.HASH_CRYPT_KEY, bytes(plain_hash), encrypt=True)
        raw_pack[idx.index_end + 68:idx.index_end + 68 + stored] = bad_payload
        bad_hash = self.root / "GameData/bad_name_hash.pack"
        bad_hash.write_bytes(raw_pack)
        with bad_hash.open("rb") as stream:
            bad_idx = qlie_pack31.read_index(stream)
            with self.assertRaisesRegex(ValueError, "文件名哈希或桶位置"):
                qlie_pack31.writer_tail(stream, bad_idx)


def _avg_scenario_installation(root):
    (root / "GameData/System").mkdir(parents=True)
    (root / "DLL").mkdir()
    (root / "game.exe").write_bytes(key_resource_exe())
    (root / "DLL/key.fkey").write_bytes(EXTERNAL_KEY)
    (root / "GameData/System/config.txt").write_bytes((
        "\r\n".join([
            *[f"Font[{i}].Name=ＭＳ ゴシック" for i in range(5)],
            "Font[5].Name=外字登録",
            "Font[6].Name=ＭＳ ゴシック",
            "Font[7].Name=ＭＳ 明朝",
        ]) + "\r\n"
    ).encode("cp932"))
    fontdata = "\r\n".join(
        f"Font[{i}].List[0]={'外字登録' if i == 5 else 'system_fix'}" for i in range(8)
    ) + "\r\n"
    scenario = (
        "@@@Library\\Avg\\header.s\r\n"
        "@@Main\r\n"
        "【キリエ】\r\n"
        "％v001％\r\n"
        "「きゃっ[f,5]¶[f,0]　こんにちは」\r\n"
        "\r\n"
        "【蓮華＠？？？】\r\n"
        "さようなら\r\n"
        "^select,進む,戻る\r\n"
    )
    (root / "GameData/data0.pack").write_bytes(make_pack([
        ("startup.s", "\\sub,@@!ConfigValueInit,\"Script\\Config\\Value.s\"\r\n".encode("cp932")),
        ("scenario/ks_01/story.s", scenario.encode("cp932")),
        ("system/font/fontdata.dat", fontdata.encode("cp932")),
        ("system/font/外字登録.of", b"\rOriginalFont1\x00\x00"),
    ], bytes([113]) * 256))
    value_s = (
        "@@!ConfigValueInit\r\n"
        "\t\\scp,cfg\r\n"
        "\t\t\\fn2,1,\"ＭＳ ゴシック\"\r\n"
        "\t\t\\fn2,6,\"ＭＳ ゴシック\"\r\n"
        "\t\t\\fn2,7,\"ＭＳ 明朝\"\r\n"
        "\\ret\r\n"
    )
    (root / "GameData/data7.pack").write_bytes(make_pack([
        ("library/avg/setting.txt", b"DefaultFormat=Multi\r\n"),
        ("library/avg/avgsystem.s", "\\gvar,str,Avg_MessageFormat\r\n".encode("cp932")),
        ("script/config/value.s", value_s.encode("cp932")),
        ("script/config/default.txt", "FontName=ＭＳ ゴシック\r\n".encode("cp932")),
    ], bytes([157]) * 256))


class QlieAvgScenarioTest(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name) / "game"
        self.root.mkdir()
        _avg_scenario_installation(self.root)
        self.output = Path(directory.name) / "patch"
        self.files = [path.relative_to(self.root).as_posix() for path in self.root.rglob("*") if path.is_file()]

    def test_avg_scenario_probe_extract_roundtrip_and_font_slot_sync(self):
        probe = qlie_pack_adapter.execute(dict(
            operation="probe", sourceDirectory=str(self.root), resourceFiles=self.files,
            templateId="qlie-pack3-imos",
        ))
        self.assertTrue(probe["systemFont"]["matched"], probe)
        required = probe["probe"]["requiredFiles"]
        self.assertIn("GameData/System/config.txt", required)
        self.assertIn("GameData/data0.pack", required)
        self.assertIn("GameData/data7.pack", required)

        units = qlie_pack_adapter.execute(dict(
            operation="extract", sourceDirectory=str(self.root), resourceFiles=required,
            templateId="qlie-pack3-imos",
        ))["extraction"]["units"]
        self.assertEqual([u["sourceText"] for u in units], [
            "キリエ", "「きゃっ[f,5]¶[f,0]　こんにちは」", "？？？", "さようなら", "進む", "戻る",
        ])
        tagged = next(u for u in units if "[f,5]" in u["sourceText"])
        self.assertEqual(tagged["protectedTokens"], ["[f,5]", "[f,0]"])

        # Original roundtrip stays byte-identical.
        originals = {u["id"]: u["sourceText"] for u in units}
        qlie_pack_adapter.execute(dict(
            operation="build", sourceDirectory=str(self.root), outputDirectory=str(self.output),
            resourceFiles=required, translations=originals, templateId="qlie-pack3-imos",
        ))
        for path in required:
            self.assertEqual((self.output / path).read_bytes(), (self.root / path).read_bytes())

        # Name alias preserves identity before ＠ and tags remain protected.
        name_unit = next(u for u in units if u["sourceText"] == "？？？")
        kiri_unit = next(u for u in units if u["sourceText"] == "キリエ")
        edits = {
            kiri_unit["id"]: "霧枝",
            name_unit["id"]: "謎",
            tagged["id"]: "「[f,5]¶[f,0]　更新済み」",
        }
        out2 = self.output.parent / "patch2"
        build = qlie_pack_adapter.execute(dict(
            operation="build", sourceDirectory=str(self.root), outputDirectory=str(out2),
            resourceFiles=required, translations=edits, templateId="qlie-pack3-imos",
        ))["build"]
        self.assertTrue(qlie_pack_adapter.execute(dict(
            operation="validate", sourceDirectory=str(self.root), outputDirectory=str(out2),
            resourceFiles=required, outputFiles=build["files"], translations=edits, templateId="qlie-pack3-imos",
        ))["validation"]["passed"])
        reextracted = qlie_pack_adapter.execute(dict(
            operation="extract", sourceDirectory=str(out2), resourceFiles=required,
            templateId="qlie-pack3-imos",
        ))["extraction"]["units"]
        self.assertEqual(reextracted, [dict(u, sourceText=edits.get(u["id"], u["sourceText"])) for u in units])

        # Verify raw script preserved identity aliases before ＠.
        from contextlib import ExitStack
        with ExitStack() as stack:
            install = qlie_pack_adapter.locate(out2, {}, required, stack)
            _, idx = install.archives["GameData/data0.pack"]
            entry = next(e for e in idx.entries if e.name == "scenario/ks_01/story.s")
            raw_script = install.read("GameData/data0.pack", entry, 1 << 20).decode("cp932")
            self.assertIn("【キリエ＠霧枝】", raw_script)
            self.assertIn("【蓮華＠謎】", raw_script)

    def test_sample_probe_accepts_avg_scenario_layout(self):
        archives = ["GameData/data0.pack", "GameData/data7.pack"]
        files = {path: self.root / path for path in [*archives, "game.exe", "DLL/key.fkey"]}
        report = qlie_samples.probe(dict(
            files=files, archives=archives, executables=["game.exe"], keyFile="DLL/key.fkey",
        ))
        self.assertEqual(report["status"], "sampled", report)
        self.assertEqual(report["templateId"], "qlie-pack3-imos")
        self.assertEqual(report["profile"]["DefaultFormat"]["value"], "Multi")

    def test_display_verify_catches_unpatched_startup_fn2_and_unknown_dialect_pua(self):
        # Directly simulate a rebuilt patch where a PUA glyph is written into scenario/ks_01/story.s,
        # config.txt & default.txt are patched, but script/config/value.s still selects ＭＳ ゴシック.
        from contextlib import ExitStack
        out = self.output
        out.mkdir()
        required = ["DLL/key.fkey", "GameData/System/config.txt", "GameData/data0.pack", "GameData/data7.pack", "game.exe"]
        for path in required:
            target = out / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes((self.root / path).read_bytes())

        mapping = {0xE000: ord("汉")}
        sans = qlie_systemfont.family("Sans", mapping) + " Regular"
        serif = qlie_systemfont.family("Serif", mapping) + " Regular"
        (out / "GameData/System/config.txt").write_bytes((
            "\r\n".join([
                *[f"Font[{i}].Name={sans}" for i in range(5)],
                "Font[5].Name=外字登録",
                f"Font[6].Name={sans}",
                f"Font[7].Name={serif}",
            ]) + "\r\n"
        ).encode("cp932"))
        (out / "qlie-l10n").mkdir()
        (out / "QlieLauncher.exe").write_bytes(b"stub")
        (out / "qlie-l10n/Sans.ttf").write_bytes(b"stub")
        (out / "qlie-l10n/Serif.ttf").write_bytes(b"stub")
        (out / "qlie-l10n/launch.xml").write_bytes((
            '<?xml version="1.0" encoding="utf-8"?>'
            f'<launch executable="game.exe">'
            f'<font file="qlie-l10n/Sans.ttf" face="{sans}" />'
            f'<font file="qlie-l10n/Serif.ttf" face="{serif}" />'
            '</launch>'
        ).encode("utf-8"))
        # Rewrite data0.pack with PUA char \uE000 in scenario/ks_01/story.s and system fontdata.dat.
        with ExitStack() as stack:
            install = qlie_pack_adapter.locate(self.root, {}, required, stack)
            units, _ = qlie_pack_adapter.scripts(install, qlie_pack_adapter.read_scripts(install))
            target_unit = next(u for u in units if u["sourceText"] == "さようなら")
            _, idx0 = install.archives["GameData/data0.pack"]
            story_e = next(e for e in idx0.entries if e.name == "scenario/ks_01/story.s")
            story_raw = install.read("GameData/data0.pack", story_e, 1 << 20).decode("cp932")
            patched_story = story_raw.replace("さようなら", "さような\ue000").encode("cp932")
            fontdata_e = next(e for e in idx0.entries if e.name == "system/font/fontdata.dat")
            patched_fd = install.read("GameData/data0.pack", fontdata_e, 1 << 20).decode("cp932").replace("system_fix", "system").encode("cp932")
            src0, _ = install.archives["GameData/data0.pack"]
            with (out / "GameData/data0.pack").open("w+b") as dst0:
                install.layout.rebuild(src0, dst0, idx0, install.internal_keys["GameData/data0.pack"],
                                       install.external_key, install.game_key,
                                       {"scenario/ks_01/story.s": patched_story, "system/font/fontdata.dat": patched_fd})
            # Patch default.txt in data7.pack, deliberately leaving script/config/value.s unpatched!
            _, idx7 = install.archives["GameData/data7.pack"]
            src7, _ = install.archives["GameData/data7.pack"]
            with (out / "GameData/data7.pack").open("w+b") as dst7:
                install.layout.rebuild(src7, dst7, idx7, install.internal_keys["GameData/data7.pack"],
                                       install.external_key, install.game_key,
                                       {"script/config/default.txt": f"FontName={sans}\r\n".encode("cp932")})

        samples_file = out / "samples.json"
        samples_file.write_text(json.dumps([dict(
            resourcePath=target_unit["resourcePath"], locator=target_unit["locator"],
            original=target_unit["sourceText"], expected="さような汉",
        )], ensure_ascii=False), encoding="utf-8")
        built_files = [*required, "QlieLauncher.exe", "qlie-l10n/launch.xml", "qlie-l10n/Sans.ttf", "qlie-l10n/Serif.ttf"]
        req = dict(output=str(out), source=str(self.root), resources=required, mode="replace",
                   files=built_files, samplesFile=str(samples_file), launcher="QlieLauncher.exe",
                   template=True, toolPaths={})
        with self.assertRaisesRegex(ValueError, "启动器没有加载游戏选用的字体.*ＭＳ ゴシック"):
            qlie_display.verify(req)

        # Now also patch script/config/value.s -> qlie_display.verify must pass and return both fonts + alias.
        with ExitStack() as stack:
            install = qlie_pack_adapter.locate(self.root, {}, required, stack)
            _, idx7 = install.archives["GameData/data7.pack"]
            src7, _ = install.archives["GameData/data7.pack"]
            val_e = next(e for e in idx7.entries if e.name == "script/config/value.s")
            val_txt = install.read("GameData/data7.pack", val_e, 1 << 20).decode("cp932")
            val_patched = val_txt.replace("ＭＳ ゴシック", sans).replace("ＭＳ 明朝", serif).encode("cp932")
            with (out / "GameData/data7.pack").open("w+b") as dst7:
                install.layout.rebuild(src7, dst7, idx7, install.internal_keys["GameData/data7.pack"],
                                       install.external_key, install.game_key,
                                       {"script/config/default.txt": f"FontName={sans}\r\n".encode("cp932"),
                                        "script/config/value.s": val_patched})
        evidence = qlie_display.verify(req)
        self.assertEqual(evidence["aliases"], {"\ue000": "汉"})
        self.assertEqual({f["face"] for f in evidence["fonts"]}, {sans, serif})

        # Unknown QLIE dialect with PUA launcher artifacts must not silently bypass display verification.
        unknown_root = self.output.parent / "unknown_game"
        unknown_root.mkdir()
        (unknown_root / "GameData").mkdir()
        (unknown_root / "DLL").mkdir()
        (unknown_root / "game.exe").write_bytes(key_resource_exe())
        (unknown_root / "DLL/key.fkey").write_bytes(EXTERNAL_KEY)
        (unknown_root / "GameData/data0.pack").write_bytes(make_pack([("story.s", b"hello\r\n")], bytes([113]) * 256))
        with self.assertRaisesRegex(ValueError, "宿主独立显示回读器"):
            qlie_display.verify(dict(
                output=str(out), source=str(unknown_root),
                resources=["DLL/key.fkey", "GameData/data0.pack", "game.exe"],
                mode="replace", files=[*built_files, qlie_systemfont.MAPPING],
                samplesFile=str(samples_file), launcher="QlieLauncher.exe",
                template=False, toolPaths={},
            ))


def _minimal_of_atlas():
    import struct
    data = bytearray(qlie_bitmap.PIXELS + qlie_bitmap.PAGE)
    data[:5] = b"\x04" + "外字".encode("cp932")
    fullwidth = qlie_bitmap.code_index("あ")
    data[qlie_bitmap.WIDTHS + fullwidth] = 64
    data[qlie_bitmap.ADVANCES + fullwidth] = 64
    data[qlie_bitmap.PALETTE + 4:qlie_bitmap.PALETTE + 8] = bytes.fromhex("ffffffff")
    struct.pack_into("<5H", data, qlie_bitmap.HEADER, 64, 64, 1, 4, 4)
    return bytes(data)


def _hash14_pack30_installation(root):
    (root / "GameData/System").mkdir(parents=True)
    (root / "DLL").mkdir()
    (root / "game.exe").write_bytes(key_resource_exe())
    (root / "DLL/key.fkey").write_bytes(EXTERNAL_KEY)
    (root / "GameData/System/config.txt").write_bytes((
        "\r\n".join([
            *[f"Font[{i}].Name=ＭＳ 明朝" for i in range(6)],
            "Font[6].Name=外字",
            "Font[7].Name=ＭＳ 明朝",
        ]) + "\r\n"
    ).encode("cp932"))
    fontdata = "\r\n".join(
        f"Font[{i}].List[0]={'外字' if i == 6 else 'system_fix'}" for i in range(8)
    ) + "\r\n"
    startup = '\t\\sub,@@!Misc_FontSetup,"Misc\\Misc.s"\r\n'
    misc = (
        "@@!Misc_FontSetup\r\n"
        "\t\\cal,n=6\r\n"
        "\t\\scp,cfg\r\n"
        '\t\t\\fn2,n,"外字"\r\n'
        "\\ret\r\n"
    )
    format_s = (
        "@@!Imo_Format_Text\r\n"
        "\t\\case,Imo_FormatMode\r\n"
        "\t\t\\ans,1\t\\subp,@@!Imo_FormatModeMulti,,ParamStr[0]\r\n"
        "\t\\end\r\n"
        "\\ret\r\n"
        "@@!Imo_FormatModeMulti\r\n"
        "\t\\var,str,s\r\n"
        "\t\\trim,ParamStr[0]\r\n"
        "\t\\scp,Imo_RepTextTable\t\\brep,ResultStr[0]\r\n"
        "\t\\cal,s=ResultStr[0]\r\n"
        '\t\\if,ResultStr[0]=="【"\\then\r\n'
        "\t\t\\sub,@@!Imo_FormatDeleteCode,,s,ResultStr[0]\r\n"
        "\t\t\\cal,Imo_FormatName[idx]=ResultStr[0]\r\n"
        "\t\t\\ret\r\n"
        "\t\\end\r\n"
        "\t\\cal,Imo_FormatText[idx]=Imo_FormatText[idx]+s\r\n"
        "\\ret\r\n"
    )
    select_s = (
        "@@!Select\r\n"
        "\t\\scp,SelectButtonList\t\\a,ParamStr[i]\r\n"
        "\\ret\r\n"
    )
    base_s = '@@!Base\r\n\t\\scp,Imo_RepTextTable\t\\l,"ImoScripter_RepTextList.txt"\t\\sort\r\n\\ret\r\n'
    scenario = (
        "【アリス】【龍之介】\r\n"
        "「爽やかな[rb,風の精,シルフィード]」\r\n"
        "\r\n"
        "^select,進む,戻る\r\n"
    )
    (root / "GameData/data0.pack").write_bytes(make_pack([
        ("startup.s", startup.encode("cp932")),
        ("system/font/fontdata.dat", fontdata.encode("cp932")),
        ("system/font/外字.of", _minimal_of_atlas()),
    ], bytes([113]) * 256, hash_version="1.4"))
    (root / "GameData/data6.pack").write_bytes(make_pack([
        ("scenario/story.s", scenario.encode("cp932")),
    ], bytes([157]) * 256, hash_version="1.4"))
    (root / "GameData/data7.pack").write_bytes(make_pack([
        ("avg/projectsetting.txt", b"FormatType=1\r\n"),
        ("avg/setting.txt", b"ReturnCode=[n]\r\n"),
        ("avg/imoscripter_fontlist.txt", "font[0].name=ＭＳ 明朝\r\nfont[6].name=外字.of\r\n".encode("cp932")),
        ("avg/imoscripter_reptextlist.txt", "〈ハ〉=[f,6]０[f]\r\n".encode("cp932")),
        ("avg/imoscripter_base.s", base_s.encode("cp932")),
        ("avg/imoscripter_format.s", format_s.encode("cp932")),
        ("avg/avgsystem_selectobject.s", select_s.encode("cp932")),
        ("avg/include_route.s", b"@@!AVG_RouteCreate\r\n\tResultInt[0]=RInt0\r\n\\ret\r\n"),
        ("misc/misc.s", misc.encode("cp932")),
    ], bytes([199]) * 256, hash_version="1.4"))


class QliePack30Hash14Test(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name) / "game"
        self.root.mkdir()
        _hash14_pack30_installation(self.root)
        self.output = Path(directory.name) / "patch"
        self.files = [path.relative_to(self.root).as_posix() for path in self.root.rglob("*") if path.is_file()]

    def test_pack30_hash14_probe_ruby_multiname_early_bitmap_route_and_roundtrip(self):
        archives = ["GameData/data0.pack", "GameData/data6.pack", "GameData/data7.pack"]
        sample_report = qlie_samples.probe(dict(
            files={path: self.root / path for path in [*archives, "game.exe", "DLL/key.fkey"]},
            archives=archives, executables=["game.exe"], keyFile="DLL/key.fkey",
        ))
        self.assertEqual(sample_report["status"], "sampled", sample_report)
        self.assertEqual(sample_report["templateId"], "qlie-pack3-imos")
        self.assertEqual(sample_report["writerLayout"], "HashVer1.4")

        probe = qlie_pack_adapter.execute(dict(
            operation="probe", sourceDirectory=str(self.root), resourceFiles=self.files,
            templateId="qlie-pack3-imos",
        ))
        self.assertTrue(probe["systemFont"]["matched"], probe)
        required = probe["probe"]["requiredFiles"]
        self.assertIn("GameData/System/config.txt", required)

        units = qlie_pack_adapter.execute(dict(
            operation="extract", sourceDirectory=str(self.root), resourceFiles=required,
            templateId="qlie-pack3-imos",
        ))["extraction"]["units"]
        self.assertEqual([u["sourceText"] for u in units], [
            "「爽やかな[rb,風の精,シルフィード]」", "アリス】【龍之介", "進む", "戻る",
        ])
        ruby_unit = units[0]
        self.assertEqual(ruby_unit["protectedTokens"], ["[rb,風の精,シルフィード]"])

        # Original roundtrip stays byte-identical on HashVer1.4 packs.
        originals = {u["id"]: u["sourceText"] for u in units}
        qlie_pack_adapter.execute(dict(
            operation="build", sourceDirectory=str(self.root), outputDirectory=str(self.output),
            resourceFiles=required, translations=originals, templateId="qlie-pack3-imos",
        ))
        for path in required:
            self.assertEqual((self.output / path).read_bytes(), (self.root / path).read_bytes())

        # CP932 edits preserve [rb,...], preserve single-＠ multi-name identity, and validate cleanly.
        name_unit = units[1]
        edits = {
            ruby_unit["id"]: "「更新済みの[rb,風の精,シルフィード]」",
            name_unit["id"]: "別名／連名",
        }
        out2 = self.output.parent / "patch2"
        build = qlie_pack_adapter.execute(dict(
            operation="build", sourceDirectory=str(self.root), outputDirectory=str(out2),
            resourceFiles=required, translations=edits, templateId="qlie-pack3-imos",
        ))["build"]
        self.assertTrue(qlie_pack_adapter.execute(dict(
            operation="validate", sourceDirectory=str(self.root), outputDirectory=str(out2),
            resourceFiles=required, outputFiles=build["files"], translations=edits,
            templateId="qlie-pack3-imos",
        ))["validation"]["passed"])
        reextracted = qlie_pack_adapter.execute(dict(
            operation="extract", sourceDirectory=str(out2), resourceFiles=required,
            templateId="qlie-pack3-imos",
        ))["extraction"]["units"]
        self.assertEqual(reextracted, [dict(u, sourceText=edits.get(u["id"], u["sourceText"])) for u in units])

        from contextlib import ExitStack
        with ExitStack() as stack:
            install = qlie_pack_adapter.locate(out2, {}, required, stack)
            self.assertIsNotNone(install.bitmap_font)
            _, idx6 = install.archives["GameData/data6.pack"]
            story = next(e for e in idx6.entries if e.name == "scenario/story.s")
            raw_script = install.read("GameData/data6.pack", story, 1 << 20).decode("cp932")
            self.assertIn("【アリス】【龍之介＠別名／連名】", raw_script)
            # Early Imo_FormatModeMulti route requires patching only AvgSystem_SelectObject.s.
            changes = {}
            install.bitmap_font.routes(changes)
            self.assertEqual(list(changes.get("GameData/data7.pack", {})), ["avg/avgsystem_selectobject.s"])
            self.assertIn(qlie_bitmap.CHOICE_EXPANSION, changes["GameData/data7.pack"]["avg/avgsystem_selectobject.s"].decode("cp932"))

    def test_pack30_hash14_rejects_tampered_packkey_or_hash_size(self):
        path = self.root / "GameData/data6.pack"
        layout = qlie_archive.Pack30()
        corrupted = bytearray(path.read_bytes())
        sig_offset = len(corrupted) - 28 - qlie_pack31.KEY_AREA_BYTES - qlie_pack31.PACKKEY_PREFIX.size
        corrupted[sig_offset] ^= 1
        bad = self.root / "GameData/bad_sig.pack"
        bad.write_bytes(corrupted)
        with bad.open("rb") as stream:
            idx = layout.index(stream)
            with self.assertRaisesRegex(ValueError, "HashVer1.4"):
                layout.tail(stream, idx)

        corrupted_size = bytearray(path.read_bytes())
        hsize_offset = len(corrupted_size) - 28 - qlie_pack31.KEY_AREA_BYTES - 4
        corrupted_size[hsize_offset] ^= 1
        bad_size = self.root / "GameData/bad_size.pack"
        bad_size.write_bytes(corrupted_size)
        with bad_size.open("rb") as stream:
            idx = layout.index(stream)
            with self.assertRaisesRegex(ValueError, "HashVer1.4"):
                layout.tail(stream, idx)

        # Corrupted name_hash or wrong bucket in FilePack 3.0 HashVer1.4 must be rejected.
        import struct

        def corrupt_hash(names):
            table = bytearray(struct.pack("<H", len(names)))
            for ordinal, name in enumerate(names):
                table += struct.pack("<H", len(name)) + name + struct.pack("<QI", ordinal * 2, 0)
            table += bytes(2 * 255) + struct.pack(f"<{len(names)}H", *range(len(names)))
            return bytes(table)

        bad_lookup = self.root / "GameData/bad_lookup.pack"
        bad_lookup.write_bytes(make_pack([("scenario/story.s", b"hello\r\n")], bytes([157]) * 256,
                                         hash_version="1.4", table_override=corrupt_hash))
        with bad_lookup.open("rb") as stream:
            idx = layout.index(stream)
            with self.assertRaisesRegex(ValueError, "文件名哈希或桶位置"):
                layout.tail(stream, idx)

    def test_ruby_inside_pc_does_not_block_sibling_edits_and_still_rejects_raw_commas(self):
        script = "[pc,夕暮れの[rb,海,うみ]]\r\n\r\nこんにちは\r\n"
        records = qlie_script.scan(script)
        rows = [record.row() for record in records]
        self.assertEqual(rows, [{"message": "夕暮れの[rb,海,うみ]"}, {"message": "こんにちは"}])

        # Editing only a sibling dialogue line keeps [pc,...[rb,...]] untouched without delimiter errors.
        sibling_rows = [rows[0], {"message": "更新済み"}]
        patched = qlie_script.patch(script, sibling_rows)
        self.assertIn("[pc,夕暮れの[rb,海,うみ]]", patched)
        self.assertIn("更新済み", patched)
        self.assertEqual([r.row() for r in qlie_script.scan(patched)], sibling_rows)

        # Editing the [pc,...] line itself while preserving [rb,...] also works.
        pc_rows = [{"message": "黄昏の[rb,海,うみ]"}, rows[1]]
        patched_pc = qlie_script.patch(script, pc_rows)
        self.assertIn("[pc,黄昏の[rb,海,うみ]]", patched_pc)
        self.assertEqual([r.row() for r in qlie_script.scan(patched_pc)], pc_rows)

        # Raw commas outside [rb,...] inside [pc,...] must still be rejected.
        with self.assertRaisesRegex(ValueError, "delimiter injection"):
            qlie_script.patch(script, [{"message": "黄昏,の[rb,海,うみ]"}, rows[1]])

    def test_unroutable_bitmap_font_falls_back_to_system_font(self):
        # Remove early replacement from ImoScripter_Format.s and have no ImoScripter_Custom.s.
        # BitmapFont.find must return None so system-font fallback remains available.
        from contextlib import ExitStack
        root = self.output.parent / "unroutable_game"
        root.mkdir()
        _hash14_pack30_installation(root)
        plain_format = (
            "@@!Imo_Format_Text\r\n"
            "\t\\case,Imo_FormatMode\r\n"
            "\t\t\\ans,1\t\\subp,@@!Imo_FormatModeMulti,,ParamStr[0]\r\n"
            "\t\\end\r\n"
            "\\ret\r\n"
            "@@!Imo_FormatModeMulti\r\n"
            "\t\\var,str,s\r\n"
            "\t\\trim,ParamStr[0]\r\n"
            "\t\\cal,s=ResultStr[0]\r\n"
            "\t\\cal,Imo_FormatText[idx]=Imo_FormatText[idx]+s\r\n"
            "\\ret\r\n"
        )
        # Rebuild data7.pack with plain_format (no Imo_RepTextTable route).
        files = [p.relative_to(root).as_posix() for p in root.rglob("*") if p.is_file()]
        req = qlie_pack_adapter.execute(dict(operation="probe", sourceDirectory=str(root), resourceFiles=files,
                                             templateId="qlie-pack3-imos"))["probe"]["requiredFiles"]
        with ExitStack() as stack:
            ins = qlie_pack_adapter.locate(root, {}, req, stack)
            src7, idx7 = ins.archives["GameData/data7.pack"]
            rebuilt = root / "GameData/data7.new.pack"
            with rebuilt.open("w+b") as dst7:
                ins.layout.rebuild(src7, dst7, idx7, ins.internal_keys["GameData/data7.pack"],
                                   ins.external_key, ins.game_key, {"avg/imoscripter_format.s": plain_format.encode("cp932")})
        rebuilt.replace(root / "GameData/data7.pack")

        with ExitStack() as stack:
            ins = qlie_pack_adapter.locate(root, {}, req, stack)
            self.assertIsNotNone(ins.font_layout)
            self.assertIsNone(ins.bitmap_font)

        # Building a non-CP932 translation now enters system-font asset resolution instead of crashing in bitmap routing.
        units = qlie_pack_adapter.execute(dict(operation="extract", sourceDirectory=str(root), resourceFiles=req,
                                               templateId="qlie-pack3-imos"))["extraction"]["units"]
        with self.assertRaisesRegex(ValueError, "启动器|字体素材"):
            qlie_pack_adapter.execute(dict(
                operation="build", sourceDirectory=str(root), outputDirectory=str(self.output.parent / "sfnt_out"),
                resourceFiles=req, translations={units[2]["id"]: "汉"}, templateId="qlie-pack3-imos",
            ))

    def test_display_verify_accepts_valid_unregistered_tail_and_still_rejects_bad_glyph_or_tail_downgrade(self):
        # Build a FilePack 3.0 game using an unregistered HashVer1.5 tail and verify custom adapter display readback.
        src = self.output.parent / "hash15_src"
        out = self.output.parent / "hash15_out"
        for d in (src / "GameData/System", src / "DLL", out / "GameData/System", out / "DLL", out / "qlie-l10n"):
            d.mkdir(parents=True)
        (src / "game.exe").write_bytes(key_resource_exe())
        (src / "DLL/key.fkey").write_bytes(EXTERNAL_KEY)
        cfg_orig = "\r\n".join(f"Font[{i}].Name=ＭＳ 明朝" for i in range(8)) + "\r\n"
        (src / "GameData/System/config.txt").write_bytes(cfg_orig.encode("cp932"))
        fontdata_orig = "\r\n".join(f"Font[{i}].List[0]=system_fix" for i in range(8)) + "\r\n"
        story_orig = "こんにちは\r\n"
        (src / "GameData/data0.pack").write_bytes(make_pack([
            ("system/font/fontdata.dat", fontdata_orig.encode("cp932")),
            ("avg/projectsetting.txt", b"FormatType=1\r\n"),
            ("avg/setting.txt", b"ReturnCode=[n]\r\n"),
            ("scenario/story.s", story_orig.encode("cp932")),
        ], bytes([113]) * 256, hash_version="1.5"))

        mapping = {0xE000: ord("汉")}
        serif = qlie_systemfont.family("Serif", mapping) + " Regular"
        (out / "game.exe").write_bytes(key_resource_exe())
        (out / "DLL/key.fkey").write_bytes(EXTERNAL_KEY)
        (out / "GameData/System/config.txt").write_bytes(
            ("\r\n".join(f"Font[{i}].Name={serif}" for i in range(8)) + "\r\n").encode("cp932")
        )
        (out / "QlieLauncher.exe").write_bytes(b"stub")
        (out / "qlie-l10n/Serif.ttf").write_bytes(b"stub")
        (out / "qlie-l10n/launch.xml").write_bytes((
            '<?xml version="1.0" encoding="utf-8"?>'
            f'<launch executable="game.exe"><font file="qlie-l10n/Serif.ttf" face="{serif}" /></launch>'
        ).encode("utf-8"))
        fontdata_patched = "\r\n".join(f"Font[{i}].List[0]=system" for i in range(8)) + "\r\n"
        (out / "GameData/data0.pack").write_bytes(make_pack([
            ("system/font/fontdata.dat", fontdata_patched.encode("cp932")),
            ("avg/projectsetting.txt", b"FormatType=1\r\n"),
            ("avg/setting.txt", b"ReturnCode=[n]\r\n"),
            ("scenario/story.s", "こんにち\ue000\r\n".encode("cp932")),
        ], bytes([113]) * 256, hash_version="1.5"))

        resources = ["DLL/key.fkey", "GameData/System/config.txt", "GameData/data0.pack", "game.exe"]
        built_files = [*resources, "QlieLauncher.exe", "qlie-l10n/launch.xml", "qlie-l10n/Serif.ttf"]
        samples_file = out / "samples.json"
        samples_file.write_text(json.dumps([dict(
            resourcePath="GameData/data0.pack", locator="custom-loc-0",
            original="こんにちは", expected="こんにち汉",
        )], ensure_ascii=False), encoding="utf-8")
        req = dict(output=str(out), source=str(src), resources=resources, mode="replace",
                   files=built_files, samplesFile=str(samples_file), launcher="QlieLauncher.exe",
                   template=False, toolPaths={})

        # 1. Valid unregistered-tail custom patch passes display readback with a clear tail warning.
        evidence = qlie_display.verify(req)
        self.assertEqual(evidence["aliases"], {"\ue000": "汉"})
        self.assertEqual([f["face"] for f in evidence["fonts"]], [serif])
        self.assertTrue(any("HashVer1.5" in w for w in evidence["warnings"]), evidence)

        # 2. Tampering the script text/glyph mapping on the same unregistered-tail patch is still rejected.
        (out / "GameData/data0.pack").write_bytes(make_pack([
            ("system/font/fontdata.dat", fontdata_patched.encode("cp932")),
            ("avg/projectsetting.txt", b"FormatType=1\r\n"),
            ("avg/setting.txt", b"ReturnCode=[n]\r\n"),
            ("scenario/story.s", "こんにち誤\r\n".encode("cp932")),
        ], bytes([113]) * 256, hash_version="1.5"))
        with self.assertRaisesRegex(ValueError, "缺少一致的解码或字形映射"):
            qlie_display.verify(req)

        # 3. Replacing a known HashVer1.4 original volume with an unregistered tail in output must be rejected.
        (src / "GameData/data0.pack").write_bytes(make_pack([
            ("system/font/fontdata.dat", fontdata_orig.encode("cp932")),
            ("avg/projectsetting.txt", b"FormatType=1\r\n"),
            ("avg/setting.txt", b"ReturnCode=[n]\r\n"),
            ("scenario/story.s", story_orig.encode("cp932")),
        ], bytes([113]) * 256, hash_version="1.4"))
        (out / "GameData/data0.pack").write_bytes(make_pack([
            ("system/font/fontdata.dat", fontdata_patched.encode("cp932")),
            ("avg/projectsetting.txt", b"FormatType=1\r\n"),
            ("avg/setting.txt", b"ReturnCode=[n]\r\n"),
            ("scenario/story.s", "こんにち\ue000\r\n".encode("cp932")),
        ], bytes([113]) * 256, hash_version="1.5"))
        with self.assertRaisesRegex(ValueError, "原封包使用宿主已登记尾表"):
            qlie_display.verify(req)

    def test_qlie_sfnt_preserves_variable_font_subfamily_and_avg_leading_bracket_prose(self):
        import struct

        def make_min_sfnt(*, variable):
            head = bytearray(54)
            struct.pack_into(">IH", head, 0, 0x00010000, 0)
            struct.pack_into(">I", head, 12, 0x5F0F3CF5)
            struct.pack_into(">H", head, 18, 1000)
            hhea = bytearray(36)
            struct.pack_into(">I", hhea, 0, 0x00010000)
            os2 = bytearray(96)
            struct.pack_into(">H", os2, 0, 4)
            cmap = qlie_sfnt.build_cmap({ord("中"): 1})
            entries = [
                (1, "Noto Sans SC Thin"), (2, "Regular"), (3, "id"),
                (4, "Noto Sans SC Thin"), (6, "NotoSansSC-Thin"),
                (16, "Noto Sans SC"), (17, "Thin"), (21, "Noto Sans SC"), (22, "Thin"),
            ]
            recs, storage = bytearray(), bytearray()
            for nid, text in entries:
                val = text.encode("utf-16-be")
                recs += struct.pack(">6H", 3, 1, 0x0409, nid, len(val), len(storage))
                storage += val
            name = struct.pack(">3H", 0, len(entries), 6 + len(recs)) + recs + storage
            tables = {b"head": bytes(head), b"hhea": bytes(hhea), b"OS/2": bytes(os2), b"cmap": cmap, b"name": name}
            if variable:
                tables[b"fvar"] = struct.pack(">8H4siii2H", 1, 0, 16, 2, 1, 20, 0, 8, b"wght",
                                              100 << 16, 100 << 16, 900 << 16, 0, 256)
            ordered = sorted(tables.items())
            n = len(ordered)
            offset = 12 + n * 16
            out = bytearray(offset + sum((len(d) + 3) & ~3 for _, d in ordered))
            struct.pack_into(">4s4H", out, 0, b"\0\1\0\0", n, 16, 0, 0)
            for i, (tag, data) in enumerate(ordered):
                struct.pack_into(">4sIII", out, 12 + i * 16, tag, qlie_sfnt.checksum(data), offset, len(data))
                out[offset:offset + len(data)] = data
                offset += (len(data) + 3) & ~3
            return bytes(out)

        def read_names(font_bytes):
            nt = struct.unpack_from(">H", font_bytes, 4)[0]
            for i in range(nt):
                tag, _, off, ln = struct.unpack_from(">4sIII", font_bytes, 12 + i * 16)
                if tag == b"name":
                    nd = font_bytes[off:off + ln]
                    _, cnt, str_off = struct.unpack_from(">3H", nd, 0)
                    out = {}
                    for j in range(cnt):
                        pid, eid, lid, nid, length, soff = struct.unpack_from(">6H", nd, 6 + j * 12)
                        if pid == 3 and eid == 1 and lid == 0x0409:
                            out[nid] = nd[str_off + soff:str_off + soff + length].decode("utf-16-be")
                    return out
            raise AssertionError("missing name table")

        var_names = read_names(qlie_sfnt.derive(make_min_sfnt(variable=True), "QlieSansTest", {0xE000: ord("中")}))
        self.assertEqual(var_names[1], "QlieSansTest")
        self.assertEqual(var_names[4], "QlieSansTest Regular")
        self.assertEqual(var_names[16], "QlieSansTest")
        self.assertEqual(var_names[17], "Thin")
        self.assertEqual(var_names[22], "Thin")

        static_names = read_names(qlie_sfnt.derive(make_min_sfnt(variable=False), "QlieSansTest", {0xE000: ord("中")}))
        self.assertEqual(static_names[1], "QlieSansTest")
        self.assertEqual(static_names[4], "QlieSansTest Regular")
        self.assertEqual(static_names[17], "Regular")
        self.assertEqual(static_names[22], "Regular")

        # Library/Avg prose starting with 【 (not ending with 】) must roundtrip and reject role changes.
        avg = "@@Main\r\n【キリエ】─\r\nこんにちは\r\n"
        rows = [r.row() for r in qlie_script.scan(avg, dialect=qlie_script.AVG_SCENARIO)]
        self.assertEqual(rows, [{"message": "【キリエ】─"}, {"message": "こんにちは"}])
        patched = qlie_script.patch(avg, [{"message": "【基里埃】─"}, {"message": "你好"}], dialect=qlie_script.AVG_SCENARIO)
        self.assertIn("【基里埃】─", patched)
        with self.assertRaisesRegex(ValueError, "字段类型发生变化"):
            qlie_script.patch(avg, [{"message": "【基里埃】"}, {"message": "你好"}], dialect=qlie_script.AVG_SCENARIO)
