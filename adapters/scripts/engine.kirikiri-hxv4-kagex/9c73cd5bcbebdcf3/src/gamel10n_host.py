"""Call fixed native tools from Python; never compile C# during adaptation."""
import json
import os
from pathlib import Path
import subprocess


def invoke(kind, request):
    paths = dict(request.get("toolPaths") or {})
    command = paths.get("hostCommand") or json.loads(os.environ.get("GAMEL10N_ADAPTER_HOST", "null"))
    if not command:
        raise RuntimeError("缺少预编译组件宿主，请使用完整发行包。")
    parser = paths.get("vnTextPatch") or os.environ.get("GAMEL10N_VNTEXTPATCH")
    if parser:
        paths["vnTextPatch"] = parser
    parser_command = paths.get("vnTextPatchCommand") or json.loads(os.environ.get("GAMEL10N_VNTEXTPATCH_COMMAND", "null"))
    if parser_command:
        paths["vnTextPatchCommand"] = parser_command
    payload = dict(request, protocolVersion=1, toolPaths=paths)
    process = subprocess.run([*command, kind], input=json.dumps(payload, ensure_ascii=False),
                             encoding="utf-8", stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    try:
        response = json.loads(process.stdout)
    except (ValueError, TypeError) as error:
        raise RuntimeError("组件未返回有效 JSON：" + process.stderr[-4000:]) from error
    if process.returncode != 0 or not response.get("success") or response.get("protocolVersion") != 1:
        raise RuntimeError(response.get("error") or process.stderr[-4000:] or "组件调用失败。")
    return response


def parser(request, format, **changes):
    """Use the pinned script parser for extract/build/validate; implement bounded probe in the adapter."""
    payload = dict(request, protocolVersion=1, format=format, **changes)
    paths = dict(payload.get("toolPaths") or {})
    command = paths.get("vnTextPatchCommand") or json.loads(
        os.environ.get("GAMEL10N_VNTEXTPATCH_COMMAND", "null")
    )
    if not command:
        raise RuntimeError("缺少固定版本 VNTextPatch 原生工具命令。")
    if not isinstance(command, list) or not command or not all(isinstance(item, str) for item in command):
        raise RuntimeError("VNTextPatch 命令必须是可执行文件和参数组成的字符串数组。")
    # An inner tool must not overwrite the outer adapter's response file.
    payload.pop("responseFile", None)
    result = subprocess.run(command, input=json.dumps(payload, ensure_ascii=False),
                            encoding="utf-8", stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            timeout=300, creationflags=0x08000000 if os.name == "nt" else 0)
    try:
        response = json.loads(result.stdout)
    except (ValueError, TypeError) as error:
        raise RuntimeError("解析器未返回有效 JSON：" + result.stderr[-4000:]) from error
    if result.returncode != 0 or response.get("protocolVersion") != 1 or not response.get("success"):
        raise RuntimeError(response.get("error") or result.stderr[-4000:] or "脚本解析失败。")
    return response


def component(operation, request=None, **parameters):
    """Batch glyph work per call. Binary inputs/outputs use base64; glyph maps are slot -> Unicode."""
    payload = dict(request or {})
    payload.update(parameters, operation=operation)
    return invoke("component", payload)["result"]


def write_response(request, response):
    text = json.dumps(response, ensure_ascii=False)
    if request.get("responseFile"):
        Path(request["responseFile"]).write_text(text, encoding="utf-8")
    else:
        print(text)
