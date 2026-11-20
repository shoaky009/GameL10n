"""Check a CP932 glyph mapping and SFNT font on one extracted ImoScripter script."""

import base64
import json
from pathlib import Path
import sys
import tempfile

from python.engines import qlie_imos
from tools.gamel10n_host import component


def sample_file(path, maximum):
    path = Path(path).resolve(strict=True)
    if not path.is_relative_to(Path("work").resolve()) or not path.is_file():
        raise ValueError("小样输入必须是 work/ 下的文件")
    if path.stat().st_size > maximum:
        raise ValueError(f"小样文件超出大小限制：{path.name}")
    return path


def check(request):
    slot_codes = request["allowedSlots"]
    if (not isinstance(slot_codes, list) or not 1 <= len(slot_codes) <= 65535
            or any(type(code) is not int or not 1 <= code <= 65535 for code in slot_codes)):
        raise ValueError("allowedSlots 需要当前引擎已确认可用的 CP932 双字节 Unicode 码位，使用十进制整数")
    chinese = request.get("text", "汉")
    if not isinstance(chinese, str) or not 1 <= len(chinese) <= 32 or any(c in chinese for c in "\r\n"):
        raise ValueError("text 需要一至三十二个字符的单行试译")
    script_file = sample_file(request["script"], 128 << 10)
    font = sample_file(request["font"], 64 << 20)
    original = script_file.read_bytes()
    script = original.decode("cp932")
    rows = qlie_imos.rows(script)
    row_index = next((index for index, row in enumerate(rows) if row.get("message")), None)
    if row_index is None:
        raise ValueError("脚本小样缺少可回填的对白")
    if qlie_imos.patch(script, rows).encode("cp932") != original:
        raise ValueError("脚本小样原文往返不一致")

    translated = rows[row_index]["message"] + chinese
    preserved = [text for row in rows for text in row.values() if isinstance(text, str)]
    mapping = component("glyph_plan", texts=[translated], allowedSlots=slot_codes,
                        preservedTexts=preserved)["map"]
    if not mapping:
        raise ValueError("请选择含有严格 CP932 无法往返的简体字，才能验证字形映射")
    encoded = component("glyph_encode", texts=[translated], map=mapping)["values"][0]
    aliases = base64.b64decode(encoded, validate=True).decode("cp932")
    expected = rows.copy()
    expected[row_index] = dict(rows[row_index], message=aliases)
    patched = qlie_imos.patch(script, expected).encode("cp932")
    actual = qlie_imos.rows(patched.decode("cp932"))
    if actual != expected:
        raise ValueError("回填改变了小样的字段或原文")
    readback = base64.b64encode(actual[row_index]["message"].encode("cp932")).decode("ascii")
    if component("glyph_decode", values=[readback], map=mapping)["texts"] != [translated]:
        raise ValueError("映射后的中文试译回读不一致")

    family = "QLIE Trial"
    with tempfile.TemporaryDirectory(prefix="qlie-runtime-sample-", dir="work") as directory:
        output = Path(directory).resolve() / "sample.ttf"
        component("qlie_font", input=str(font), output=str(output), family=family, map=mapping)
        if not output.is_file() or not output.stat().st_size:
            raise ValueError("字体组件未生成小样字体")
    return dict(status="passed", translationReadback=True, mappedCodepoints=len(mapping),
                fontFace=family + " Regular", runtimeTested=False)


if __name__ == "__main__":
    print(json.dumps(check(json.load(sys.stdin)), ensure_ascii=False))
