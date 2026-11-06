#!/usr/bin/env python3
"""Build fixed NativeAOT parser/inspectors and check JSON fixtures without running games."""
import argparse
import json
import os
from pathlib import Path
import struct
import subprocess
import tempfile
import zipfile


def bundle_sources(repo, output):
    output.mkdir(parents=True, exist_ok=True)
    files = [('vendor/' + path.relative_to(repo / 'third_party/VNTextPatch').as_posix(), path)
             for path in (repo / 'third_party/VNTextPatch').rglob('*.cs')]
    files += [('worker/' + name, repo / 'tools/GameL10n.VnTextPatch' / name)
              for name in ('Worker.cs', 'Protocol.cs', 'YurisTextEncoding.cs', 'Serialization/VnTextPatchJson.cs')]
    with zipfile.ZipFile(output / 'sources.zip', 'w', compression=zipfile.ZIP_DEFLATED) as archive:
        for name, path in sorted(files):
            info = zipfile.ZipInfo(name, (2000, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            archive.writestr(info, path.read_bytes())


def invoke(executable, mode, request):
    process = subprocess.run([str(executable), mode], input=json.dumps(dict(protocolVersion=1, **request), ensure_ascii=False),
                             encoding='utf-8', stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                             creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0, timeout=60)
    result = json.loads(process.stdout)
    if process.returncode or not result.get('success'):
        raise RuntimeError(result.get('error') or process.stderr[-4000:])
    return result['result']


def verify(executable):
    mapping = invoke(executable, 'component', dict(operation='glyph_plan', texts=['汉'], allowedSlots=[0xE000], preservedTexts=[], map={}))['map']
    assert mapping == {'57344': ord('汉')}, mapping
    encoded = invoke(executable, 'component', dict(operation='glyph_encode', texts=['汉かな'], map=mapping))['values']
    decoded = invoke(executable, 'component', dict(operation='glyph_decode', values=encoded, map=mapping))['texts']
    assert decoded == ['汉かな'], decoded
    imports = invoke(executable, 'inspect', dict(operation='pe_imports', path=str(executable), query='GetProcAddress', limit=4))
    assert imports['totalImports'] > 0, imports
    data = executable.read_bytes()
    pe = struct.unpack_from('<I', data, 60)[0]
    entry_rva = struct.unpack_from('<I', data, pe+24+16)[0]
    code = invoke(executable, 'inspect', dict(operation='pe_code', path=str(executable), addressKind='rva',
                  ranges=[dict(address=str(entry_rva), length=64)], instructionLimit=12))
    assert code['status'] == 'completed' and code['ranges'][0]['decodedBytes'] > 0, code
    with tempfile.TemporaryDirectory(prefix='gamel10n-inspector-') as directory:
        text = '独立中文锚点'.encode('utf-8')
        metadata = bytearray(24)
        struct.pack_into('<6I', metadata, 0, 0xFAB11BAF, 31, 24, 8, 32, len(text))
        metadata += struct.pack('<2I', len(text), 0) + text
        path = Path(directory) / 'fixture-metadata.dat'
        path.write_bytes(metadata)
        literals = invoke(executable, 'inspect', dict(operation='unity_schema', path=str(path), mode='literals', query='中文', limit=4))
        assert literals['literals'][0]['text'] == '独立中文锚点', literals
    return dict(glyphRoundtrip=True, peImports=True, capstoneDecode=True, il2cppLiterals=True)


def main():
    repo = Path(__file__).resolve().parent.parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--dotnet', default='dotnet', help='.NET 10 SDK executable; build time only')
    parser.add_argument('--output', type=Path, default=repo / 'target/runtime/vntextpatch')
    parser.add_argument('--verify-only', action='store_true')
    parser.add_argument('--sources-only', action='store_true', help='Rebuild the bundled parser source index without compiling')
    args = parser.parse_args()
    output = args.output.resolve()
    bundle_sources(repo, output)
    if args.sources_only:
        return
    if not args.verify_only:
        subprocess.run([args.dotnet, 'publish', str(repo / 'tools/GameL10n.VnTextPatch/GameL10n.VnTextPatch.csproj'),
                        '-c','Release','-r','win-x64','--self-contained','true','-p:PublishAot=true',
                        '-p:StripSymbols=true','-p:OptimizationPreference=Size','-o',str(output)], cwd=repo, check=True)
    executable = output / 'GameL10n.VnTextPatch.exe'
    checks = verify(executable)
    print(json.dumps(dict(executable=str(executable), bytes=executable.stat().st_size, checks=checks), ensure_ascii=False))


if __name__ == '__main__':
    main()
