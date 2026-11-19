"""Resource filename candidates from decoded Whale scripts."""

import json
from pathlib import Path
import re
import sys

from python.engines import whale


MAX_SCRIPT = 32 << 20
FILE_COMMANDS = {
    "BG": ("bg", "tlg"),
    "EV": ("ev", "tlg"),
    "BGM": ("bgm", "ogg"),
    "SE0": ("se", "ogg"),
    "CG": ("", "tlg"),
}
COMMAND = re.compile(r"^([A-Z][A-Z0-9.]*)[ \t]+(.*)$")
VOICE = re.compile(r"^【[^,】]+,([Ss]([0-9]{3})_[A-Za-z0-9_]+)】")
SPRITE = re.compile(r"[Ss]([0-9]{3})_([A-Za-z0-9]{6})_([A-Za-z0-9]{4})_([A-Za-z0-9])")


def asset_names(script):
    names = set()
    for line in script.splitlines():
        voice = VOICE.match(line)
        if voice:
            names.add(f"vo/{voice[2]}/{voice[1]}.ogg")
            continue
        command = COMMAND.match(line)
        if not command:
            continue
        verb, content = command.groups()
        args = [part.strip().strip('"') for part in content.split(",")]
        if verb in ("ST", "MW.FC"):
            sprite = SPRITE.fullmatch(args[0])
            if sprite:
                character, body, pose, size = sprite.groups()
                directory = f"st/{character}{size}"
                names.add(f"{directory}/{pose[:3]}_{pose[3]}.tlg")
                names.add(f"{directory}/S{character}_{body}_000{body[3]}_{size}.tlg")
            continue
        if verb not in FILE_COMMANDS:
            continue
        index = 1 if verb == "CG" else 0
        if index >= len(args) or not args[index]:
            continue
        directory, extension = FILE_COMMANDS[verb]
        name = re.sub(r"@@[0-9]+$", "", args[index].replace("\\", "/"))
        if not name or name.startswith(("/", "$", "%", "@")):
            continue
        if directory and "/" not in name:
            name = f"{directory}/{name}"
        if not name.lower().endswith("." + extension):
            name += "." + extension
        names.add(name)
    return names


def main(request=None):
    request = json.load(sys.stdin) if request is None else request
    report = dict(success=False, parseStatus="failed", roundtripStatus="not-run",
                  translationStatus="not-run", fieldCount=0, runtimeStatus="not-tested")
    try:
        with Path(request["input"]).open("rb") as source:
            data = source.read(MAX_SCRIPT + 1)
        if len(data) > MAX_SCRIPT or b"\0" in data:
            raise ValueError("expected a complete, decoded Whale text script")
        script = data.decode("cp932")
        fields = whale.extract(script)
        report.update(success=True, parseStatus="passed", fieldCount=len(fields),
                      assetNameCount=len(asset_names(script)),
                      profile=dict(format="Whale", encoding="cp932"))
        if request.get("operation") == "verify":
            if request.get("checkRoundtrip", True):
                baseline = whale.patch(script, {})
                if baseline != script:
                    raise ValueError("Whale original-text round trip differs")
                report["roundtripStatus"] = "passed" if baseline.encode("cp932") == data else "inconclusive"
            if request.get("checkTranslation", True) and fields:
                index = next((i for i, f in enumerate(fields) if f.kind == "dialogue" and f.role != "name"), 0)
                replacement = fields[index].text + "確認"
                rebuilt = whale.patch(script, {index: replacement}).encode("cp932")
                after = whale.extract(rebuilt.decode("cp932"))
                if len(after) != len(fields) or after[index].text != replacement:
                    raise ValueError("Whale replacement did not preserve the text fields")
                report["changedTextReadback"] = "passed"
                try:
                    whale.patch(script, {index: replacement + "汉化验证"}).encode("cp932")
                except UnicodeEncodeError as error:
                    report.update(translationStatus="failed", diagnostic=dict(errorType=type(error).__name__),
                                  error="CP932 cannot represent the Chinese sample")
                else:
                    report["translationStatus"] = "passed"
    except (ValueError, OSError) as error:
        report.update(success=False, error=str(error)[:1000], diagnostic=dict(errorType=type(error).__name__))
    return report


if __name__ == "__main__":
    report = main()
    print(json.dumps(report, ensure_ascii=True))
    sys.exit(0 if report["success"] else 1)
