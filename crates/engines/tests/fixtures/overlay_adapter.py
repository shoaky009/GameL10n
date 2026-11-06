"""A generic text-table format with independent numbered patch files."""
import json
from pathlib import Path
import sys

CASE = 'valid'
request = json.load(sys.stdin)
source = Path(request['sourceDirectory'])
operation = request['operation']
original = json.loads((source / 'text.json').read_text(encoding='utf-8'))
response = dict(protocolVersion=1, success=True)


def units(texts):
    return [dict(id=str(i), resourcePath='text.json', locator=str(i),
                 sourceText=text, context='stable', kind='dialogue')
            for i, text in enumerate(texts)]


if operation == 'probe':
    candidates = request.get('resourceFiles', [])
    numbers = [int(path[10:]) for path in candidates
               if path.startswith('text.json.') and path[10:].isdigit()]
    patch = f'text.json.{max(numbers, default=-1) + 1:03d}'
    response['probe'] = dict(engineId='fixture', adapterId='engine.fixture', adapterVersion='1',
                             description='fixture', requiredFiles=['text.json'],
                             buildHints=dict(patchPath=patch))
elif operation == 'extract':
    actual = units(original)
    if request.get('buildMode') == 'overlay':
        paths = request['outputFiles']
        assert paths == [request['buildHints']['patchPath']]
        patch = Path(request['outputDirectory']) / paths[0]
        changes = json.loads(patch.read_text(encoding='utf-8'))
        if CASE != 'ignore-overlay':
            for unit in actual:
                unit['sourceText'] = changes.get(unit['id'], unit['sourceText'])
        if CASE == 'drop-untouched':
            actual = [unit for unit in actual if unit['id'] in changes]
        if CASE == 'mutate-patch':
            patch.write_text('{}', encoding='utf-8')
    response['extraction'] = dict(units=actual, warnings=[])
    response['validationContextVersion'] = 1
elif operation == 'build':
    path = request['buildHints']['patchPath']
    translations = (json.loads(Path(request['translationsFile']).read_text(encoding='utf-8'))
                    if request.get('translationsFile') else request.get('translations', {}))
    changed = {str(i): translations[str(i)] for i, text in enumerate(original)
               if str(i) in translations and translations[str(i)] != text}
    files = []
    if changed or CASE == 'nonempty-original':
        target = Path(request['outputDirectory']) / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(json.dumps(changed, ensure_ascii=False), encoding='utf-8')
        files.append(path)
    response['build'] = dict(mode='overlay', files=files, warnings=[])
elif operation == 'validate':
    assert request['buildMode'] == 'overlay'
    path = request['buildHints']['patchPath']
    assert request['outputFiles'] == [path]
    if request.get('validationContextFile'):
        context = json.loads(Path(request['validationContextFile']).read_text(encoding='utf-8'))
        assert context['resourceFiles'] == ['text.json']
        assert context['outputFiles'] == [path]
        assert context['originalUnits'] == units(original)
    assert not (Path(request['outputDirectory']) / 'text.json').exists()
    response['validation'] = dict(passed=True, issues=[], runtimeStatus='not-tested')

if request.get('responseFile'):
    Path(request['responseFile']).write_text(json.dumps(response, ensure_ascii=False), encoding='utf-8')
else:
    print(json.dumps(response, ensure_ascii=False))
