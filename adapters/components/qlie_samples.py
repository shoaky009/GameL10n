"""Probe a keyed QLIE PACK 3.0 + ImoScripter installation using only selected members."""
from contextlib import ExitStack
from pathlib import Path

from python.archives import qlie, qlie_keys, qlie_writer
from python.engines import qlie_imos


PROFILE = {
    'avg/projectsetting.txt': ('FormatType', '1'),
    'avg/setting.txt': ('ReturnCode', '[n]'),
}
KEY_BYTES = 1 << 20
SAMPLE_BYTES = 128 << 10


def _read_file(path, limit):
    with Path(path).open('rb') as stream:
        data = stream.read(limit + 1)
    if len(data) > limit:
        raise ValueError('文件超过 QLIE 小样读取预算')
    return data


def internal_key(archive, external_key, game_key):
    stream, index = archive
    first = index.entries[0]
    if first.name != qlie.PACK_KEY_NAME or any(e.name == qlie.PACK_KEY_NAME for e in index.entries[1:]):
        raise ValueError('封包缺少唯一的首项内部密钥')
    return qlie.read_member(stream, index, first, mode='keyed', key_file=external_key,
                            game_key=game_key, max_stored=KEY_BYTES, max_output=KEY_BYTES)


def read_member(archive, entry, key, game_key, limit):
    stream, index = archive
    return qlie.read_member(stream, index, entry, mode='keyed', key_file=key,
                            game_key=game_key, max_stored=limit, max_output=limit)


def profile_members(index):
    for entry in index.entries:
        name = entry.name.replace('\\', '/').casefold()
        if name in PROFILE:
            yield entry, PROFILE[name]


def script_profile(archives, read):
    sources = {}
    checked = 0
    for path, archive in archives.items():
        for entry, (field, expected) in profile_members(archive[1]):
            checked += 1
            if checked > 8:
                raise ValueError('ImoScripter 配置副本超出读取范围')
            config = read(path, entry, 64 << 10).decode('cp932')
            values = set()
            for line in config.splitlines():
                key, separator, value = line.partition('=')
                if separator and key.strip().casefold() == field.casefold():
                    values.add(value.strip())
            if values != {expected}:
                raise ValueError(f'{entry.name} 未确认 {field}={expected}')
            sources.setdefault(field, dict(archive=path, member=entry.name, value=expected))
    if len(sources) != len(PROFILE):
        raise ValueError('缺少 ImoScripter FormatType 与 ReturnCode 配置证据')
    return sources


def _script_trial(entry, raw):
    script = raw.decode('cp932')
    original = qlie_imos.rows(script)
    row = next((i for i, value in enumerate(original) if value['message']), None)
    if row is None:
        raise ValueError('脚本小样没有可回填文本')
    if qlie_imos.patch(script, original).encode('cp932') != raw:
        raise ValueError('脚本原文回填字节发生变化')

    translated = original.copy()
    translated[row] = dict(original[row], message=original[row]['message'] + '汉')
    changed = qlie_imos.patch(script, translated)
    trial = dict(member=entry.name, bytes=len(raw), unitCount=len(original), parseStatus='passed',
                 roundtripStatus='passed', scriptTranslationStatus='passed')
    try:
        encoded = changed.encode('cp932')
    except UnicodeEncodeError:
        return dict(trial, translationStatus='failed', errorType='UnicodeEncodeError')
    if qlie_imos.rows(encoded.decode('cp932')) != translated:
        raise ValueError('编码后的脚本回读与请求译文不符')
    return dict(trial, translationStatus='passed')


def probe(request):
    files = request['files']
    key_file = request['keyFile']
    external_key = _read_file(files[key_file], KEY_BYTES)
    executable_keys = []
    for exe in request['executables']:
        try:
            game_key = qlie_keys.game_key_from_pe(_read_file(files[exe], 64 << 20))
        except (OSError, ValueError):
            continue
        executable_keys.append((exe, game_key))
    if len(executable_keys) != 1:
        return dict(status='executable-unconfirmed', error='未确认唯一带 IconKeyImage 的游戏 EXE')
    exe, game_key = executable_keys[0]

    with ExitStack() as stack:
        archives = {}
        keys = {}
        eligible = []
        for path in request['archives']:
            stream = stack.enter_context(Path(files[path]).open('rb'))
            index = qlie.read_index(stream, max_entries=16_000, max_index_bytes=2 << 20)
            archives[path] = stream, index
            eligible.extend((path, entry) for entry in index.entries
                            if entry.name.lower().endswith('.s')
                            and 0 < entry.size <= SAMPLE_BYTES
                            and 0 < entry.unpacked_size <= SAMPLE_BYTES)
        def read(path, entry, limit):
            archive = archives[path]
            if path not in keys:
                keys[path] = internal_key(archive, external_key, game_key)
            return read_member(archive, entry, keys[path], game_key, limit)

        profile = script_profile(archives, read)
        # Prefer longer scene members over short bootstrap and interface scripts.
        eligible.sort(key=lambda item: (not item[1].name.replace('\\', '/').casefold().startswith('scenario/'),
                                        not (item[1].packed and item[1].encryption),
                                        -item[1].unpacked_size, item[0], item[1].name))
        error = '封包中没有预算内的 ImoScripter 脚本小样'
        for path, entry in eligible[:6]:
            archive = archives[path]
            try:
                qlie_writer.hash13_tail(*archive, max_bytes=4 << 20)
                raw = read(path, entry, SAMPLE_BYTES)
                trial = _script_trial(entry, raw)
            except ValueError as exc:
                error = str(exc)[:160]
                continue
            return dict(status='sampled', archive=path, version='FilePackVer3.0', writerLayout='HashVer1.3',
                        mode='keyed', executable=exe, keyFile=key_file, scriptParser=qlie_imos.REFERENCE,
                        profile=profile, trials=[trial],
                        nextAction='沿用已通过的小样及 python.archives.qlie、qlie_keys 和 python.engines.qlie_imos。'
                                   '脚本原文已逐字节回填，简体字已在脚本结构层读回，严格 CP932 编码仍需解决。'
                                   '按现有 qlie_writer 的模板条件实现回包，定向验证译文编码和游戏内显示，再做完整 test_adapter')
        return dict(status='script-unconfirmed', error=error)
