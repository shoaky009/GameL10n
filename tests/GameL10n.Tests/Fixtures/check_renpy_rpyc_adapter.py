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
archive(source / "one.rpa", {"same.rpyc": original, "same.rpy": b"source overrides compiled"})
reject(lambda: adapter.execute(dict(packed, resourceFiles=["one.rpa"])), "加载顺序")
assert not list(Path.cwd().glob("rpyc-*")), "temporary resources leaked"
print("rpyc-adapter-ok")
