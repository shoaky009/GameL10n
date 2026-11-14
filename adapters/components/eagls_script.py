"""EAGLS script fields over the pinned source-text reader and writer.

Archive indexing, encryption and label repair belong to the shared library.
Only established message, choice and literal name syntax is exposed here.
"""
from dataclasses import asdict
import json
from pathlib import Path
import sys

from python.engines.eagls_text import Profile, export_script, parse_script, rebuild_script


def _profiles(data):
    """Bound the text-area hint by the label table; full parsing confirms it.

    EAGLS_script_tool/scpacker.py uses zero-filled label space and 100-byte
    allocation units. Keys and alternative label layouts remain explicit.
    """
    offsets = {3600}
    limit = min(len(data) - 2, 1 << 20)
    for slot in range(0, limit - 35, 36):
        if data[slot] != 0:
            continue
        end = slot + 36
        pos = end
        while pos < limit and data[pos] == 0:
            pos += 1
        if pos < limit:
            block = end + (pos - end) // 36 * 36
            offset = (block + 99) // 100 * 100
            if end <= offset < len(data) - 2:
                offsets.add(offset)
        break
    return [Profile(text_offset=offset, version=version)
            for offset in sorted(offsets) for version in (2, 1)]


class ScriptText:
    """Stable command locators for complete encrypted script members."""

    def __init__(self, data, profile=None):
        candidates = [profile] if profile is not None else _profiles(data)
        matches, failures = [], []
        for candidate in candidates:
            try:
                _, rows, locators = parse_script(data, candidate)
                matches.append((candidate, rows, locators))
            except (ValueError, UnicodeError) as error:
                failures.append(str(error)[:180])
        if len(matches) != 1:
            raise ValueError('script profile is ambiguous' if matches else
                             'no supported script profile: ' + '; '.join(failures))
        self.profile, self.rows, self.locators = matches[0]
        self.data = data
        _, self.manifest = export_script(data, self.profile)
        self.fields = []
        self._targets = {}
        self.speaker_refs = {}
        counts = {}
        for index, (row, locator) in enumerate(zip(self.rows, self.locators)):
            kind = 'dialogue' if locator['kind'] == 'message' else 'choice'
            key = (kind, locator['id'])
            ordinal = counts.get(key, 0)
            counts[key] = ordinal + 1
            path = f'{kind}|{locator["id"]}|{ordinal}'
            if row['message']:
                self.fields.append((path, row['message'], kind))
                self._targets[path] = (index, 'message')
            name = row.get('name')
            if name and not name.startswith(':'):
                name_path = path + '|name'
                self.fields.append((name_path, name, 'name'))
                self._targets[name_path] = (index, 'name')
                self.speaker_refs[path] = name_path
            elif name:
                self.speaker_refs[path] = name

    def build(self, translations):
        """Rebuild selected fields; shared checks preserve labels and untouched rows."""
        if not isinstance(translations, dict) or translations.keys() - self._targets.keys():
            raise ValueError('translation references an unknown field')
        rows = [dict(row) for row in self.rows]
        for path, value in translations.items():
            index, field = self._targets[path]
            if not isinstance(value, str) or not value:
                raise ValueError('translation must be a nonempty string')
            if field == 'name' and value.startswith(':'):
                raise ValueError('literal name cannot become a variable reference')
            rows[index][field] = value
        result = rebuild_script(self.data, rows, self.manifest, self.profile)
        after = ScriptText(result, self.profile)
        expected = {path: text for path, text, _ in self.fields} | translations
        if {path: text for path, text, _ in after.fields} != expected or after.speaker_refs != self.speaker_refs:
            raise ValueError('rebuild changed text identity, speaker references or untouched fields')
        return result


def main(request=None):
    request = json.load(sys.stdin) if request is None else request
    operation = request.get('operation', 'inspect')
    if operation not in ('inspect', 'verify', 'build'):
        raise ValueError('operation must be inspect, verify or build')
    path = Path(request['input'])
    if path.stat().st_size > 32 << 20:
        raise ValueError('script input exceeds 32 MiB')
    data = path.read_bytes()
    settings = request.get('profile')
    if settings is not None:
        settings = dict(settings)
        if 'key' in settings:
            settings['key'] = settings['key'].encode('ascii')
    result = dict(success=False, stage='parse', parseStatus='failed', roundtripStatus='not-run',
                  translationStatus='not-run', fieldCount=0, samples=[], runtimeStatus='not-tested')
    try:
        scene = ScriptText(data, Profile(**settings) if settings is not None else None)
        profile = asdict(scene.profile)
        profile['key'] = profile['key'].decode('ascii')
        result.update(parseStatus='passed', fieldCount=len(scene.fields), profile=profile,
                      kinds={kind: sum(field[2] == kind for field in scene.fields)
                             for kind in sorted({field[2] for field in scene.fields})},
                      samples=[dict(path=p, text=t[:120], kind=k) for p, t, k in scene.fields[:2]])
        if operation == 'verify':
            if request.get('checkRoundtrip', True):
                result.update(stage='original-roundtrip', roundtripStatus='failed')
                if scene.build({}) != data:
                    raise ValueError('original-byte roundtrip differs')
                result['roundtripStatus'] = 'passed'
            else:
                result['roundtripStatus'] = 'skipped'
            if request.get('checkTranslation', True):
                result.update(stage='translation-readback', translationStatus='inconclusive')
                trial = next(((p, t) for p, t, kind in scene.fields if kind in ('dialogue', 'choice')), None)
                if trial is not None:
                    result['translationStatus'] = 'failed'
                    scene.build({trial[0]: trial[1] + '汉化验证'})
                    result['translationStatus'] = 'passed'
            else:
                result['translationStatus'] = 'skipped'
        if operation == 'build':
            result['stage'] = 'build'
            output = Path(request['outputFile'])
            if output.resolve() == path.resolve():
                raise ValueError('output must differ from the source sample')
            rebuilt = scene.build(request.get('translations', {}))
            with output.open('xb') as stream:
                stream.write(rebuilt)
            result['outputFile'] = str(output)
        result.update(success=True, stage='complete')
    except (ValueError, UnicodeError) as error:
        result.update(error=type(error).__name__ + ': ' + str(error)[:1000],
                      diagnostic=dict(errorType=type(error).__name__))
    return result


if __name__ == '__main__':
    try:
        result = main()
        print(json.dumps(result, ensure_ascii=True))
        if not result['success']:
            raise SystemExit(1)
    except Exception as error:
        print(json.dumps(dict(success=False, error=type(error).__name__ + ': ' + str(error)[:1000])))
        raise SystemExit(1)
