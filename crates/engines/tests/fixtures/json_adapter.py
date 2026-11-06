import json
from pathlib import Path
import sys

r = json.load(sys.stdin)
root = Path(r['sourceDirectory'])
data = json.loads((root / 'text.json').read_text(encoding='utf8'))
out = dict(protocolVersion=1, success=True)
if r['operation'] == 'probe':
    out['probe'] = dict(engineId='fixture', adapterId='engine.fixture', adapterVersion='1',
                        description='fixture', requiredFiles=['text.json'], warnings=[])
elif r['operation'] == 'extract':
    out['extraction'] = dict(units=[dict(id=str(i),resourcePath='text.json',locator=str(i),
        sourceText=value,context='stable',kind='dialogue') for i,value in enumerate(data)], warnings=[])
elif r['operation'] == 'build':
    output = Path(r['outputDirectory'])
    output.mkdir(parents=True, exist_ok=True)
    translations = json.loads(Path(r['translationsFile']).read_text(encoding='utf8')) if r.get('translationsFile') else r.get('translations', {})
    changed = [translations.get(str(i), value) for i,value in enumerate(data)]
    (output / 'text.json').write_bytes((root / 'text.json').read_bytes() if changed == data else json.dumps(changed, ensure_ascii=False).encode('utf8'))
    out['build'] = dict(files=['text.json'], warnings=[])
elif r['operation'] == 'validate':
    out['validation'] = dict(passed=True, issues=[], runtimeStatus='not-tested')
if r.get('responseFile'):
    with open(r['responseFile'], 'w', encoding='utf8') as output:
        json.dump(out, output, ensure_ascii=False)
else:
    print(json.dumps(out, ensure_ascii=False))
