"""Exercise protocol glue and rejection paths with the real pinned reader/writer."""
import json
from pathlib import Path
import pickle
import sys
from unittest.mock import patch
import zlib

from renpy_rpyc_fixture import Raw, encode, sample

package = Path(sys.argv[1])
sys.path.append(str(package))
from tools import renpy_rpyc_adapter as adapter
from tools import renpy_mixed_adapter as mixed

config_path = package / "tools/adapter-template.json"
config = json.loads(config_path.read_text())
config.update(templateId="loose-renpy-rpyc", archiveComponent=None, scriptExtensions=[".rpyc"])
config_path.write_text(json.dumps(config))


def reject(action, message):
    try:
        action()
    except ValueError as error:
        assert message in str(error), str(error)
    else:
        raise AssertionError("expected rejection: " + message)


def request(operation, **extra):
    return dict(protocolVersion=1, adapterDirectory=str(package), operation=operation,
                sourceDirectory=str(source), resourceFiles=["script.rpyc"], **extra)


def archive(path, members):
    body, index, key = bytearray(b"RPA-3.0 " + b"0" * 16 + b" 12345678\n"), {}, 0x12345678
    for name, data in members.items():
        index[name] = [(len(body) ^ key, len(data) ^ key, b"")]
        body.extend(data)
    body[8:24] = f"{len(body):016x}".encode()
    body.extend(zlib.compress(pickle.dumps(index, protocol=3)))
    path.write_bytes(body)


source = Path("source").absolute()
source.mkdir()
script = source / "script.rpyc"
original = sample()
script.write_bytes(original)
assert adapter.execute(request("probe"))["probe"]
before = adapter.execute(request("extract"))["extraction"]["units"]
assert len(before) == 4
assert before[0]["speaker"] == "e"
assert before[0]["protectedTokens"] == ["[player]", "{b}", "{/b}"]

translated = Path("translated").absolute()
adapter.execute(request("build", outputDirectory=str(translated),
                        translations={before[0]["id"]: "你好 [player] {b}更长的中文内容{/b}"}))
assert adapter.execute(request("validate", outputDirectory=str(translated)))["validation"]["passed"]
assert (translated / "script.rpyc").read_bytes()[-16:] == original[-16:]
assert script.read_bytes() == original
reject(lambda: adapter.execute(request("build", outputDirectory=str(Path("bad-controls").absolute()),
                                       translations={before[0]["id"]: "丢失控制符"})), "changed controls")

# Matching extracted dialogue is insufficient: unrelated code and the digest must survive.
(translated / "script.rpyc").write_bytes(sample(code="modified code"))
reject(lambda: adapter.execute(request("validate", outputDirectory=str(translated))), "文本之外")
(translated / "script.rpyc").write_bytes(original[:-16] + b"X" * 16)
reject(lambda: adapter.execute(request("validate", outputDirectory=str(translated))), "文本之外")
script.write_bytes(sample(second_message="different slot"))
reject(lambda: adapter.execute(request("extract")), "differ")
script.write_bytes(b"old-format-rpyc")
assert adapter.execute(request("probe"))["probe"] is None
reject(lambda: adapter.execute(request("extract")), "expected bounded RPYC2")

# A constructor opcode is preserved as data, including while rewriting both slots.
inert = Raw(b"cos\nsystem\n" + encode(("MUST NOT EXECUTE",)) + b"R")
script.write_bytes(sample(code=inert))
with patch("os.system", side_effect=AssertionError("game code executed")):
    adapter.execute(request("build", outputDirectory=str(Path("inert").absolute()),
                            translations={before[0]["id"]: "你好 [player] {b}世界{/b}"}))

config = json.loads((package / "tools/adapter-template.json").read_text())
config.update(templateId="rpa3-renpy-rpyc", archiveComponent="archive.rpa3")
(package / "tools/adapter-template.json").write_text(json.dumps(config))
packed = dict(protocolVersion=1, adapterDirectory=str(package), operation="extract",
              sourceDirectory=str(source), resourceFiles=["one.rpa", "two.rpa"])
for name in packed["resourceFiles"]:
    archive(source / name, {"same.rpyc": original})
reject(lambda: adapter.execute(packed), "相同 RPYC 路径")
# Paired archived .rpy alongside .rpyc is ignored by Ren'Py (dir is None),
# while non-default tl/<lang>/ translation packs are skipped by default.
archive(source / "one.rpa", {
    "same.rpyc": original,
    "same.rpy": b"ignored archived source",
    "tl/russian/other.rpyc": original,
    "tl/russian/other.rpy": b"ignored russian source",
})
paired_units = adapter.execute(dict(packed, resourceFiles=["one.rpa"]))["extraction"]["units"]
assert len(paired_units) == 4
archive(source / "one.rpa", {"same.rpyc": original, "orphan.rpy": b"no paired rpyc"})
reject(lambda: adapter.execute(dict(packed, resourceFiles=["one.rpa"])), "加载顺序")
# rpa3-renpy-rpyc defaults to fast overlay output anchored at the archive directory.
(source / "game").mkdir()
archive(source / "game/archive.rpa", {
    "story.rpyc": original,
    "story.rpy": b"ignored archived source",
    "tl/espanol/story.rpyc": original,
    "media.bin": b"media-bytes",
})
rpa_req = dict(packed, resourceFiles=["game/archive.rpa"])
rpa_units = adapter.execute(rpa_req)["extraction"]["units"]
assert len(rpa_units) == 4
rpa_empty = Path("rpa-empty-overlay").absolute()
rpa_empty_build = adapter.execute(dict(rpa_req, operation="build", outputDirectory=str(rpa_empty), translations={}))["build"]
assert rpa_empty_build["mode"] == "overlay" and rpa_empty_build["files"] == []
rpa_overlay = Path("rpa-overlay").absolute()
rpa_text = "你好 [player] {b}覆盖补丁{/b}"
rpa_built = adapter.execute(dict(rpa_req, operation="build", outputDirectory=str(rpa_overlay),
                                 translations={rpa_units[0]["id"]: rpa_text}))["build"]
assert rpa_built["mode"] == "overlay" and rpa_built["files"] == ["game/story.rpyc"]
assert not (rpa_overlay / "game/archive.rpa").exists()
rpa_reextracted = adapter.execute(dict(rpa_req, operation="extract", outputDirectory=str(rpa_overlay),
                                       buildMode="overlay", outputFiles=rpa_built["files"]))["extraction"]["units"]
assert rpa_reextracted[0]["sourceText"] == rpa_text
assert rpa_reextracted[0]["resourcePath"] == "game/archive.rpa"
assert rpa_reextracted[0]["context"] == rpa_units[0]["context"]
assert adapter.execute(dict(rpa_req, operation="validate", outputDirectory=str(rpa_overlay),
                            buildMode="overlay", outputFiles=rpa_built["files"]))["validation"]["passed"]
(rpa_overlay / "game/extra.rpyc").write_bytes(original)
reject(lambda: adapter.execute(dict(rpa_req, operation="validate", outputDirectory=str(rpa_overlay),
                                    buildMode="overlay", outputFiles=rpa_built["files"])), "不一致")
(rpa_overlay / "game/extra.rpyc").unlink()
(rpa_overlay / "game/story.rpyc").write_bytes(sample(code="tampered"))
reject(lambda: adapter.execute(dict(rpa_req, operation="validate", outputDirectory=str(rpa_overlay),
                                    buildMode="overlay", outputFiles=rpa_built["files"])), "文本之外")
config.update(buildMode="replace")
config_path.write_text(json.dumps(config))
rpa_replace = Path("rpa-replace").absolute()
rpa_replaced = adapter.execute(dict(rpa_req, operation="build", outputDirectory=str(rpa_replace),
                                    translations={rpa_units[0]["id"]: rpa_text}))["build"]
assert rpa_replaced["mode"] == "replace" and rpa_replaced["files"] == ["game/archive.rpa"]
assert adapter.execute(dict(rpa_req, operation="validate", outputDirectory=str(rpa_replace)))["validation"]["passed"]
config.pop("buildMode", None)
config.update(templateId="renpy-mixed", archiveComponent="archive.rpa3", scriptExtensions=[".rpy", ".rpyc"])
config_path.write_text(json.dumps(config))
script.write_bytes(original)
(source / "script.rpy").write_text('label start:\n    "Source"\n')
reject(lambda: mixed.execute(dict(request("extract"), resourceFiles=["script.rpy", "script.rpyc"])), "加载冲突")
archive(source / "one.rpa", {"script.rpyc": original})
reject(lambda: mixed.execute(dict(request("extract"), resourceFiles=["script.rpy", "one.rpa"])), "加载冲突")
archive(source / "one.rpa", {"story.rpy": b"source"})
archive(source / "two.rpa", {"story.rpyc": original})
reject(lambda: mixed.execute(packed), "加载冲突")
(source / "nested").mkdir()
archive(source / "nested/one.rpa", {"other.rpyc": original})
reject(lambda: mixed.execute(dict(packed, resourceFiles=["script.rpyc", "nested/one.rpa"])), "加载根目录")
archive(source / "one.rpa", {"other.rpyc": original, "media.bin": b"keep-media"})
# Packed-only renpy-mixed uses fast overlay output without rewriting one.rpa.
packed_only = dict(request("extract"), resourceFiles=["one.rpa"])
packed_units = mixed.execute(packed_only)["extraction"]["units"]
assert len(packed_units) == 4
empty_overlay = Path("mixed-empty-overlay").absolute()
built_empty = mixed.execute(dict(packed_only, operation="build", outputDirectory=str(empty_overlay), translations={}))["build"]
assert built_empty["mode"] == "overlay" and built_empty["files"] == []
output = Path("mixed-output").absolute()
changed_text = "你好 [player] {b}混合资源{/b}"
built = mixed.execute(dict(packed_only, operation="build", outputDirectory=str(output),
                           translations={packed_units[0]["id"]: changed_text}))["build"]
assert built["mode"] == "overlay" and built["files"] == ["other.rpyc"]
assert not (output / "one.rpa").exists()
reextracted = mixed.execute(dict(packed_only, operation="extract", outputDirectory=str(output),
                                 buildMode="overlay", outputFiles=built["files"]))["extraction"]["units"]
reextracted_unit = next(unit for unit in reextracted if unit["id"] == packed_units[0]["id"])
assert reextracted_unit["sourceText"] == changed_text
assert reextracted_unit["resourcePath"] == "one.rpa"
assert reextracted_unit["context"] == packed_units[0]["context"]
assert mixed.execute(dict(packed_only, operation="validate", outputDirectory=str(output),
                          buildMode="overlay", outputFiles=built["files"]))["validation"]["passed"]
# Loose + packed renpy-mixed defaults to replace so existing loose resources can be updated.
combined = dict(request("extract"), resourceFiles=["script.rpyc", "one.rpa"])
units = mixed.execute(combined)["extraction"]["units"]
assert len(units) == 8
packed_unit = next(unit for unit in units if unit["resourcePath"] == "one.rpa")
replaced = Path("mixed-replace").absolute()
built_replace = mixed.execute(dict(combined, operation="build", outputDirectory=str(replaced),
                                   translations={packed_unit["id"]: changed_text}))["build"]
assert built_replace["mode"] == "replace" and built_replace["files"] == ["one.rpa", "script.rpyc"]
assert mixed.execute(dict(combined, operation="validate", outputDirectory=str(replaced)))["validation"]["passed"]
assert not list(Path.cwd().glob("renpy-*")), "mixed temporary resources leaked"
assert not list(Path.cwd().glob("rpyc-*")), "temporary resources leaked"
print("rpyc-adapter-ok")
