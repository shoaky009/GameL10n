"""Visible SCN fields over the pinned GalTransl PSB/SCN reader and writer.

Requires python.engines.kirikiri_{psb,scn} from tools/engine-reference.json.
This layer selects fields and exposes stable semantic locators; it does not
decrypt XP3, choose archive precedence, or establish runtime/font support.
"""
import json
from pathlib import Path
import re
import sys

from python.engines.kirikiri_psb import Psb
from python.engines import kirikiri_scn as scn


def tokens(text):
    """Protect SCN controls, including the on-disk spelling of line breaks."""
    return list(dict.fromkeys(scn.controls(text.replace('\\n', '\n'))
                              + re.findall(r'\\n', text)))


class SceneText:
    """One explicit language slot; internal speaker IDs are context by default.

    fields contains (semantic_path, original_text, kind, PSB_node). Body/name
    locators follow the original tree, so omitting derived caches never shifts
    visible IDs. Unknown layouts, controls or cache rules fail explicitly.
    """
    def __init__(self, data, language_index=0, *, speaker_name=False):
        self.p = Psb(data)
        self.language_index = language_index
        self.speaker_name = speaker_name
        self.records = scn.records(self.p, language_index, speaker_name=speaker_name)
        self.fields = []
        self.routes = {}
        self.derived_paths = set()
        scenes = scn.fields(self.p, self.p.root, ())['scenes']
        by_scene = {}
        for record in self.records:
            try:
                scn.controls(record['row']['message'])
            except ValueError as error:
                raise ValueError(f"SCN scene={record['scene']}, text={record['index']}: {error}") from error
            by_scene.setdefault(record['scene'], []).append(record)
            for path in record['caches']:
                self.derived_paths.add(self.locate(path)[1])
        for index, (node, route) in enumerate(scn.list_nodes(scenes)):
            scene = scn.fields(self.p, node, route)
            if 'title' in scene and 21 <= scene['title'][0].tag <= 24:
                self.add(scene['title'][1], 'ui')
            for record in by_scene.get(index, []):
                if record['name'] is not None:
                    self.add(record['name'], 'name')
                self.add(record['message'], 'choice' if record['kind'] == 'choice' else 'dialogue')

    def locate(self, route):
        node = self.p.root
        parts = []
        for index in route:
            if node.tag == 33:
                key, node = node.value[index]
                parts.append(self.p.keys[key].replace('~', '~0').replace('/', '~1'))
            elif node.tag == 32:
                parts.append(str(index))
                node = node.value[index]
            else:
                raise ValueError('SCN path traverses a scalar')
        return node, '/'.join(parts)

    def add(self, route, kind):
        node, path = self.locate(route)
        text = self.p.text(node)
        if not text.strip():
            return
        if path in self.routes:
            raise ValueError('duplicate SCN field: ' + path)
        self.routes[path] = route
        self.fields.append((path, text, kind, node))

    def route(self, path):
        return self.routes[path]

    def build(self, translations):
        if not isinstance(translations, dict) or set(translations) - self.routes.keys():
            raise ValueError('unknown SCN field; derived speech/search text is not independently translatable')
        original = {path: text for path, text, _, _ in self.fields}
        for text in translations.values():
            if not isinstance(text, str) or '\0' in text:
                raise ValueError('invalid SCN translation')
        changed = {self.routes[path]: text for path, text in translations.items() if text != original[path]}
        if not changed:
            return None
        rows = [dict(record['row']) for record in self.records]
        allowed = set(changed)
        semantic_paths = set()
        for record, row in zip(self.records, rows):
            semantic_paths.add(record['message'])
            if record['message'] in changed:
                row['message'] = changed[record['message']].replace('\\n', '\n')
                allowed.update(record['caches'])
                if 'length' in record:
                    allowed.add(record['length'])
            if record['name'] is not None:
                semantic_paths.add(record['name'])
                if record['name'] in changed:
                    row['name'] = changed[record['name']]
        rebuilt, _ = scn.patch(self.p, self.records, rows)
        # Scene titles are not part of the upstream message/choice table.
        titles = {path: text for path, text in changed.items() if path not in semantic_paths}
        if titles:
            rebuilt = Psb(rebuilt).patch(titles)
        after = SceneText(rebuilt, self.language_index, speaker_name=self.speaker_name)
        expected = original | translations
        if {path: text for path, text, _, _ in after.fields} != expected:
            raise ValueError('SCN re-extraction changed a translated or untouched field')
        if scn.fingerprint(self.p, allowed) != scn.fingerprint(after.p, allowed):
            raise ValueError('SCN rebuild changed data outside the selected fields and their derived values')
        return rebuilt


def main():
    """Bounded JSON CLI for run_engine_python; importing the class has no I/O."""
    request = json.load(sys.stdin)
    operation = request.get('operation', 'inspect')
    if operation not in ('inspect', 'verify', 'build'):
        raise ValueError('operation must be inspect, verify or build')
    path = Path(request['input'])
    if path.stat().st_size > 32 << 20:
        raise ValueError('SCN input exceeds 32 MiB')
    data = path.read_bytes()
    scene = SceneText(data, request.get('languageIndex', 0), speaker_name=request.get('speakerName', False))
    result = dict(success=True, fieldCount=len(scene.fields), derivedCacheCount=len(scene.derived_paths),
                  kinds={kind: sum(field[2] == kind for field in scene.fields)
                         for kind in sorted({field[2] for field in scene.fields})}, runtimeStatus='not-tested')
    result['samples'] = [dict(path=p, text=t[:160], kind=k) for p, t, k, _ in scene.fields[:4]]
    if operation == 'verify':
        same, _ = scn.patch(scene.p, scene.records, [dict(record['row']) for record in scene.records])
        if same != data:
            raise ValueError('SCN original-byte roundtrip failed')
        result['originalByteIdentical'] = True
        trial = next(((p, t) for p, t, k, _ in scene.fields if k in ('dialogue', 'choice')), None)
        result['changedTextVerified'] = False
        if trial is not None:
            trial_text = trial[1] + '汉化验证'
            scene.build({trial[0]: trial_text})
            result.update(changedTextVerified=True, trialPath=trial[0])
    if operation == 'build':
        output = Path(request['outputFile'])
        if output.resolve() == path.resolve():
            raise ValueError('SCN output must differ from the input')
        rebuilt = scene.build(request.get('translations', {}))
        # Exclusive creation avoids overwriting either a source or prior output.
        with output.open('xb') as stream:
            stream.write(data if rebuilt is None else rebuilt)
        result.update(outputFile=str(output), byteIdentical=rebuilt is None)
    return result


if __name__ == '__main__':
    try:
        print(json.dumps(main(), ensure_ascii=True))
    except Exception as error:
        print(json.dumps(dict(success=False, error=type(error).__name__ + ': ' + str(error)), ensure_ascii=True))
        raise SystemExit(1)
