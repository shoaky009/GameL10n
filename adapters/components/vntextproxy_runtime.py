"""Prepare a pinned VNTextProxy runtime without loading it or running a game.

The JSON stdin CLI defaults to operation="describe". operation="check" reads
archive=<downloaded ZIP>, proxy=<exact DLL name> and verifies the release hash,
member hash and x86 PE32 DLL header. operation="prepare" additionally writes
that DLL and its licenses under output=<workspace-relative directory>. Existing
identical files are reused; differing files and unsafe output paths are rejected.
No operation downloads files, installs fonts, modifies an EXE or tests DLL loading.

Use sjis_tunnel.Tunnel to produce the matching ordered, BOM-less UTF-16LE
sjis_ext.bin next to the game EXE. The official DLL has no INI configuration.
It searches the EXE directory for the first *.ttf, then *.ttc, then *.otf and
uses the filename before its first dot as the font face. Confirm that face and
glyph coverage; multiple candidates are ambiguous. Locale Emulator relaunch
is commented out in the upstream build. Static preparation never establishes
engine character traversal, font rendering, side-loading or locale compatibility.
GDI+ font creation is not hooked: AddFontResourceExW does not populate a GDI+
private font collection. Probe the game's actual constructor before replacement.
"""

import hashlib
import io
import json
from pathlib import Path, PureWindowsPath
import struct
import sys
import zipfile


VERSION = "v0.0.41"
URL = ("https://github.com/arcusmaximus/VNTranslationTools/releases/download/"
       "v0.0.41/VNTranslationTools.zip")
ARCHIVE_SHA256 = "37fc44f2c2fb73190b783df44bb58b60ce44e70131942f262ee3bf17dfa12439"
PROXIES = {
    "d2d1.dll": "c3ffe496ebeb561f454f73e520528e12d736ead0f3113a6cfbaf30d6b9c27c96",
    "d3d9.dll": "b605198cbb2b51dcb99472132d97f6bfc72471e8f46bb0f0f2f3669de65b9d5b",
    "ddraw.dll": "e2281b724ef216fa0ca1a76c6eaf70f5d82d98489c895e89e7076193e5e9c827",
    "dinput8.dll": "a783164ea684f3b0ff60a20df69b5772ab80e7d3fbc85f1e1a2c198680745474",
    "msacm32.dll": "2830d9d8cf902769c392a7cdaf9de6fe870a2fede0a079216822ad0a2f6e3cab",
    "version.dll": "ee9d3ecc335636778c8f55f73a6792abc2476a245f73e53434682b59e3e4cc4e",
    "winmm.dll": "268f1c4b2fbddd3644cd3705e6bba79448c060ad890c3eb27001e76ac10a5c83",
    "xinput1_3.dll": "16f35eb64132cf027058a94447c6d85ef93dc470b1c784d479775fb58ac926ce",
}
LICENSE_NAME = "VNTextProxy-LICENSE.txt"
MAX_ARCHIVE_BYTES = 8 * 1024 * 1024
MAX_REQUEST_BYTES = 64 * 1024


def describe():
    """Return fixed release information and the limits of static preparation."""
    return dict(
        success=True, status="described", runtimeTested=False,
        release=dict(version=VERSION, url=URL, sha256=ARCHIVE_SHA256),
        proxies=[dict(name=name, member=f"VNTextProxy/{name}", sha256=digest)
                 for name, digest in PROXIES.items()],
        selection="Explicitly choose one imported/loaded DLL after inspecting the actual x86 EXE. "
                  "Keep its original name; do not rename another proxy or replace an existing game DLL.",
        mapping="sjis_ext.bin beside the EXE: ordered BOM-less UTF-16LE, at most 3422 non-NUL "
                "BMP characters; preserve existing slots and plan translated plus preserved text.",
        font="Optional private font beside the EXE: first *.ttf, otherwise *.ttc, otherwise *.otf; "
             "filename before first dot is the requested face. Match the internal font name, "
             "verify coverage and avoid multiple candidates. No font is bundled or downloaded.",
        configuration="No INI configuration. Official source enables GDI and Direct2D hooks; "
                      "Locale Emulator relaunch is commented out. Other rendering paths need evidence.",
        gdiplus="No GDI+ hooks in v0.0.41. GDI registration/metrics do not establish that "
                "GdipCreateFontFromDC or GdipCreateFontFromLogfontA/W can create the font. "
                "Use windows-font-metrics with renderer='gdiplus' on the original and candidate. "
                "If only privateCollection succeeds, the game's creation path must explicitly "
                "use that collection with a valid lifetime. Inspect the gdiplus-font-bridge component "
                "for ordinary x86 GdipCreateFontFromDC/LogfontA/W imports before writing a custom patch; "
                "dynamic/delay imports are unsupported. Changing OTF to TTF alone is not a fix. "
                "Do not infer a font's outline format from NotTrueTypeFont status alone.",
        license="Retain VNTextProxy-LICENSE.txt with the selected DLL (VNTranslationTools and Detours MIT). "
                "The release ZIP contains no license; the helper includes the upstream notices.",
        validation="Archive/member hashes and x86 PE32 headers only. Verify actual loading, character "
                   "traversal, text rendering, layout and locale separately with the current game.")


def _pe32_dll(data):
    if len(data) < 64 or data[:2] != b"MZ":
        raise ValueError("Proxy is not a PE DLL")
    offset = struct.unpack_from("<I", data, 0x3C)[0]
    if offset > len(data) - 26 or data[offset:offset + 4] != b"PE\0\0":
        raise ValueError("Proxy has an invalid PE header")
    machine = struct.unpack_from("<H", data, offset + 4)[0]
    optional_size, flags, magic = struct.unpack_from("<HHH", data, offset + 20)
    if machine != 0x14C or optional_size < 2 or magic != 0x10B or not flags & 0x2000:
        raise ValueError("Proxy must be an i386 PE32 DLL; x64 games are unsupported")


def _read_proxy(archive, proxy):
    if not isinstance(proxy, str) or proxy not in PROXIES:
        raise ValueError("proxy must be an exact DLL name from describe.proxies")
    if not isinstance(archive, str) or not archive or "\0" in archive:
        raise ValueError("archive must be a downloaded ZIP file path")
    with Path(archive).open("rb") as source:
        raw = source.read(MAX_ARCHIVE_BYTES + 1)
    if len(raw) > MAX_ARCHIVE_BYTES or hashlib.sha256(raw).hexdigest() != ARCHIVE_SHA256:
        raise ValueError("Archive SHA256 does not match official VNTranslationTools v0.0.41")
    with zipfile.ZipFile(io.BytesIO(raw)) as package:
        member = package.getinfo(f"VNTextProxy/{proxy}")
        if member.file_size > 256 * 1024:
            raise ValueError("Unexpected proxy member size")
        data = package.read(member)
    if hashlib.sha256(data).hexdigest() != PROXIES[proxy]:
        raise ValueError("Proxy member SHA256 mismatch")
    _pe32_dll(data)
    return data


def _output_path(output):
    if not isinstance(output, str) or not output or "\0" in output:
        raise ValueError("output must be a workspace-relative directory")
    normalized = output.replace("\\", "/")
    parts = normalized.split("/")
    if (PureWindowsPath(output).drive or any(part in ("", ".", "..") for part in parts)
            or any(":" in part or part.endswith((".", " ")) for part in parts)):
        raise ValueError("output must be a safe workspace-relative directory")
    workspace = Path.cwd().resolve()
    target = workspace.joinpath(*parts)
    for path in (target, *target.parents):
        if path == workspace:
            break
        if path.is_symlink() or (hasattr(path, "is_junction") and path.is_junction()):
            raise ValueError("output cannot traverse a symbolic link or junction")
        if path.exists() and not path.is_dir():
            raise ValueError("output directory is occupied by a file")
    if not target.resolve().is_relative_to(workspace):
        raise ValueError("output leaves the workspace")
    return target


def _prepare(data, proxy, output):
    target = _output_path(output)
    licenses = _output_path(output + "/licenses")
    license_data = Path(__file__).with_name(LICENSE_NAME).read_bytes()
    files = [(target / proxy, data), (licenses / LICENSE_NAME, license_data)]
    reused = []
    for path, content in files:
        if (path.is_symlink() or (hasattr(path, "is_junction") and path.is_junction())
                or not path.resolve().is_relative_to(target.resolve())):
            raise ValueError("Output file cannot traverse a symbolic link or junction")
        if path.parent.exists() and not path.parent.is_dir():
            raise ValueError("Output parent is occupied by a file")
        if path.exists() and (not path.is_file() or path.read_bytes() != content):
            raise ValueError(f"Existing output differs; preserve and inspect it first: {path.name}")
        reused.append(path.exists())
    for (path, content), exists in zip(files, reused):
        if not exists:
            path.parent.mkdir(parents=True, exist_ok=True)
            with path.open("xb") as destination:
                destination.write(content)
    return [dict(path=path.relative_to(Path.cwd().resolve()).as_posix(),
                 sha256=hashlib.sha256(content).hexdigest(), reused=exists,
                 role="license" if path.name == LICENSE_NAME else "asset")
            for (path, content), exists in zip(files, reused)]


def execute(request):
    """Describe, check or explicitly prepare a single proxy; never execute it."""
    result = dict(success=False, status="rejected", runtimeTested=False)
    try:
        if not isinstance(request, dict):
            raise ValueError("request must be a JSON object")
        operation = request.get("operation", "describe")
        if operation == "describe":
            return describe()
        if operation not in ("check", "prepare"):
            raise ValueError("operation must be describe, check or prepare")
        proxy = request.get("proxy")
        data = _read_proxy(request.get("archive"), proxy)
        files = _prepare(data, proxy, request.get("output")) if operation == "prepare" else []
        result.update(success=True, status="prepared" if operation == "prepare" else "checked",
                      version=VERSION, proxy=proxy, sha256=PROXIES[proxy],
                      checks=dict(archiveHash=True, memberHash=True, pe32Dll=True), files=files)
    except (OSError, ValueError, KeyError, zipfile.BadZipFile) as error:
        result["error"] = str(error)
    return result


def main():
    try:
        data = sys.stdin.buffer.read(MAX_REQUEST_BYTES + 1)
        if len(data) > MAX_REQUEST_BYTES:
            raise ValueError("request exceeds 64 KiB")
        result = execute(json.loads(data.decode("utf-8-sig")))
    except (UnicodeError, ValueError) as error:
        result = dict(success=False, status="rejected", runtimeTested=False, error=str(error))
    print(json.dumps(result, ensure_ascii=True))
    return 0 if result["success"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
