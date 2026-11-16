"""Read-only input access across Python and the native parser inside AppContainer."""
import concurrent.futures
import io
import json
import mmap
import os
from pathlib import Path
import subprocess
import sys

request = json.load(sys.stdin)
root = Path(request['sourceDirectory'])
file = root / 'script.ks'
original = file.read_bytes()
assert root.is_dir() and root.exists() and file.is_file()
assert {p.name for p in root.glob('*')} == {'script.ks', 'data.bin'}
assert {p.name for p in root.iterdir()} == {'script.ks', 'data.bin'}
assert file.stat().st_size == len(original)
assert os.listdir(root) == [entry.name for entry in os.scandir(root)]
assert not (root / 'missing').exists()
try:
    (root / 'missing').read_bytes()
except FileNotFoundError:
    pass
else:
    raise AssertionError('Missing input opened')
with file.open('rb') as first, io.open(file, 'rb') as second:
    assert first.name == second.name == str(file)
    assert first.read(4) == second.read(4) == original[:4]
    with mmap.mmap(first.fileno(), 0, access=mmap.ACCESS_READ) as view:
        assert view[:] == original
    try:
        mmap.mmap(first.fileno(), 0, access=mmap.ACCESS_WRITE)
    except OSError:
        pass
    else:
        raise AssertionError('Writable input mapping')
fd = os.open(file, os.O_RDONLY)
try:
    assert os.read(fd, len(original)) == original
finally:
    os.close(fd)
for mode in ('wb', 'r+b'):
    try:
        file.open(mode)
    except PermissionError:
        pass
    else:
        raise AssertionError('Writable input opened')
# Bypassing the Python routing layer still cannot access the original path.
import _io
for path, mode in ((file, 'rb'), (file, 'wb'), (Path(request['outside']), 'rb')):
    try:
        _io.open(path, mode)
    except PermissionError:
        pass
    else:
        raise AssertionError('Direct access escaped AppContainer')

child_code = request['inputBootstrap'] + '\nconfigure_tempfiles(True)\n' + (
    'from pathlib import Path\n'
    f'assert Path({str(file)!r}).read_bytes() == {original!r}\n')

def python_child(_):
    child = subprocess.run([sys.executable, '-I', '-X', 'utf8', '-c', child_code], capture_output=True, text=True, encoding='utf8')
    assert child.returncode == 0, child.stderr

def native(operation, **extra):
    payload = dict(protocolVersion=1, operation=operation, sourceDirectory=str(root), format='KirikiriKs', **extra)
    child = subprocess.run(request['toolPaths']['vnTextPatchCommand'], input=json.dumps(payload),
                           capture_output=True, text=True, encoding='utf8')
    result = json.loads(child.stdout)
    assert child.returncode == 0 and result['success'], result
    return result

with concurrent.futures.ThreadPoolExecutor(4) as pool:
    assert list(pool.map(lambda _: file.read_bytes(), range(20))) == [original] * 20
    list(pool.map(python_child, range(4)))
    extractions = list(pool.map(lambda _: native('extract'), range(4)))
units = extractions[0]['extraction']['units']
assert units and all(item == extractions[0] for item in extractions)
translations = {unit['id']: '変更された文章です。' for unit in units}
output = request['outputDirectory']
native('build', outputDirectory=output, translations=translations)
validation = native('validate', outputDirectory=output, translations=translations)
assert validation['validation']['passed'], validation
assert (Path(output) / 'script.ks').read_bytes() != original
assert file.read_bytes() == original
Path(request['responseFile']).write_text(json.dumps(dict(protocolVersion=1, success=True,
    validation=dict(passed=True, issues=[], runtimeStatus='not-tested'))), encoding='utf8')
