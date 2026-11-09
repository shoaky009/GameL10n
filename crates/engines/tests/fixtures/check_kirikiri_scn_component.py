"""Synthetic PSB reference aliasing, SCN derived-field and language checks."""
import importlib.util
import sys


spec = importlib.util.spec_from_file_location('component', sys.argv[1])
component = importlib.util.module_from_spec(spec)
spec.loader.exec_module(component)


from kirikiri_scn_fixture import psb


message = r'Hello\n%f123;world.'
cache = 'Helloworld.'
row = ['actor-id', [[None, message, len(cache), cache, cache]], None, None, None]
named_row = ['actor-id', [['Alice', message, len(cache), cache, cache]], None, None, None]
tree = dict(name='sample', instructions='do-not-translate', scenes=[dict(title='Opening', texts=[row, named_row],
            selects=[dict(text='Continue'), dict(selidx=1, storage='sample', target='*end')])])
raw = psb(tree)
scene = component.SceneText(raw)
paths = {p: t for p, t, _, _ in scene.fields}
body = 'scenes/0/texts/0/1/0/1'
assert len(paths) == 5 and paths[body] == message
assert 'scenes/0/texts/0/0' not in paths  # Internal speaker ID, not display name.
assert 'scenes/0/texts/0/1/0/3' not in paths and len(scene.derived_paths) == 4
assert scene.build(paths) is None
assert component.scn.patch(scene.p, scene.records, [dict(r['row']) for r in scene.records])[0] == raw

translated = r'中文\n%f123;正文。'
after = component.SceneText(scene.build({body: translated}))
assert {p: t for p, t, _, _ in after.fields} == paths | {body: translated}
first = after.records[0]
assert [after.locate(path)[0].value for path in [first['length']]] == [len('中文正文。')]
assert [after.p.text(after.locate(path)[0]) for path in first['caches']] == ['中文正文。'] * 2
assert after.records[1]['row']['message'] == message.replace('\\n', '\n')  # Aliased source strings remain local.

changes = {'scenes/0/title': '开场', 'scenes/0/texts/1/1/0/0': '爱丽丝', 'scenes/0/selects/0/text': '继续'}
changed = component.SceneText(scene.build(changes))
assert {p: t for p, t, _, _ in changed.fields} == paths | changes

def rejects(action):
    try:
        action()
    except ValueError:
        return
    raise AssertionError('invalid SCN edit was accepted')

rejects(lambda: scene.build({'scenes/0/texts/0/1/0/3': 'independent-cache-translation'}))
rejects(lambda: scene.build({body: 'control token removed'}))
bad = scene.p.patch({scene.records[0]['message']: 'desynchronized body'})
rejects(lambda: component.SceneText(bad))

# A different language is selected explicitly; unselected slot IDs/data remain intact.
row[1].append([None, 'Bonjour', 7, 'Bonjour', 'Bonjour'])
tree['scenes'][0]['texts'] = [row]
tree['scenes'][0]['selects'] = [dict(language=[dict(text='Continue'), dict(text='Continuer')])]
multilingual = psb(tree)
french = component.SceneText(multilingual, language_index=1)
french_path = 'scenes/0/texts/0/1/1/1'
result = french.build({french_path: '你好'})
assert component.SceneText(result, language_index=0).records[0]['row']['message'] == message.replace('\\n', '\n')
assert component.SceneText(result, language_index=1).records[0]['row']['message'] == '你好'

# Partial mode isolates unknown controls and inconsistent caches to their message.
for bad_message, bad_cache in [('Unmatched]', 'Unmatched]'), ('Original', 'different-cache')]:
    tree = dict(scenes=[dict(title='Opening', texts=[
        ['internal-id', [['Alice', bad_message, len(bad_cache), bad_cache, bad_cache]], None, None, None],
        ['internal-id', [[None, 'Good', 4, 'Good', 'Good']], None, None, None]])])
    raw = psb(tree)
    rejects(lambda: component.SceneText(raw))
    partial = component.SceneText(raw, allow_partial=True)
    assert len(partial.skipped_fields) == 1
    path = 'scenes/0/texts/1/1/0/1'
    first = partial.records[0]
    expected_original = component.scn.fingerprint(partial.p, {partial.records[1]['message'],
        *partial.records[1]['caches'], partial.records[1]['length']})
    rebuilt = component.SceneText(partial.build({path: '中文'}), allow_partial=True)
    assert len(rebuilt.skipped_fields) == 1
    assert component.scn.fingerprint(rebuilt.p, {rebuilt.records[1]['message'],
        *rebuilt.records[1]['caches'], rebuilt.records[1]['length']}) == expected_original
    assert rebuilt.p.text(rebuilt.locate(first['message'])[0]) == bad_message
    assert rebuilt.p.text(rebuilt.locate(first['message'][:-1] + (3,))[0]) == bad_cache
    named = component.SceneText(partial.build({'scenes/0/texts/0/1/0/0': '爱丽丝'}), allow_partial=True)
    assert named.p.text(named.locate(first['message'])[0]) == bad_message
    assert named.p.text(named.locate(first['message'][:-1] + (3,))[0]) == bad_cache
    rejects(lambda: partial.build({'scenes/0/texts/0/1/0/1': 'uncertain body'}))
print('scn-component-ok')
