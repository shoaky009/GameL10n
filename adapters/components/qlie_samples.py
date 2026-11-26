"""Probe QLIE layouts and retain the results of bounded archive and script trials."""
from contextlib import ExitStack
from dataclasses import dataclass
import json
from pathlib import Path
import tempfile

from python.archives import qlie_keys

from . import engine_archive as archive
from . import qlie_archive as pack
from . import qlie_script as script_format

PROFILE = {
    'avg/projectsetting.txt': ('FormatType', '1'),
    'avg/setting.txt': ('ReturnCode', '[n]'),
}
AVG_PROFILE = {
    'library/avg/setting.txt': ('DefaultFormat', 'Multi'),
}
SAMPLE_BYTES = 128 << 10
SAMPLE_ARCHIVE_BYTES = 32 << 20
MAX_EXE = 64 << 20


def _read_file(path, limit):
    with Path(path).open('rb') as stream:
        data = stream.read(limit + 1)
    if len(data) > limit:
        raise ValueError('文件超过 QLIE 小样读取预算')
    return data


def inspect_uninstall_archive(root, files):
    paths = {path.casefold(): path for path in files}
    relative = paths.get('uninstalldata.pack')
    if relative is None:
        return None
    executable = paths.get('uninstall.exe')
    report = dict(archive=relative, executable=executable, status='unconfirmed')
    if executable is None:
        return dict(report, reason='missing-executable')
    try:
        binary = archive.read_file(root / executable, MAX_EXE).lower()
        name = relative.lower()
        if not binary.startswith(b'mz') or not any(
            name.encode(encoding) in binary for encoding in ('ascii', 'utf-16-le')
        ):
            return dict(report, reason='missing-archive-reference')
        layout = pack.LAYOUTS['3.1']
        with archive.checked_path(root / relative).open('rb') as stream:
            index = pack.read_index(stream, layout)
        types = {Path(entry.name).suffix.lower() for entry in index.entries}
        report.update(memberCount=len(index.entries), memberTypes=sorted(types))
        if not types <= {'.bmp', '.ico', '.txt'} or not types & {'.bmp', '.ico'}:
            return dict(report, reason='unexpected-member-types')
    except (OSError, ValueError) as error:
        return dict(report, reason='inspection-failed', error=str(error)[:240])
    return dict(report, status='confirmed', reason='uninstaller-reference-and-index')


def dialect(profile):
    return script_format.AVG_SCENARIO if 'DefaultFormat' in profile else 'imos'


def profile_members(index, layout):
    for entry in index.entries:
        name = entry.name.replace('\\', '/').casefold()
        if layout.unicode_scripts and name.startswith('avg2/'):
            name = 'avg/' + name[len('avg2/'):]
        if name in PROFILE:
            yield entry, PROFILE[name]
        elif name in AVG_PROFILE:
            yield entry, AVG_PROFILE[name]


def script_profile(archives, read, layout):
    imos_sources, avg_sources = {}, {}
    checked = 0
    saw_imos = False
    for path, volume in archives.items():
        for entry, (field, expected) in profile_members(volume[1], layout):
            checked += 1
            if checked > 8:
                raise ValueError('ImoScripter 配置副本超出读取范围')
            raw = read(path, entry, 64 << 10)
            config, _, _ = script_format.decode(raw, unicode_scripts=layout.unicode_scripts)
            values = set()
            for line in config.splitlines():
                key, separator, value = line.partition('=')
                if separator and key.strip().casefold() == field.casefold():
                    values.add(value.strip())
            if values != {expected}:
                raise ValueError(f'{entry.name} 未确认 {field}={expected}')
            target = avg_sources if field == 'DefaultFormat' else imos_sources
            if field != 'DefaultFormat':
                saw_imos = True
            target.setdefault(field, dict(archive=path, member=entry.name, value=expected))
    if saw_imos:
        if len(imos_sources) != len(PROFILE):
            raise ValueError('缺少 ImoScripter FormatType 与 ReturnCode 配置证据')
        return imos_sources
    if len(avg_sources) == len(AVG_PROFILE):
        has_scenario = any(
            entry.name.replace('\\', '/').casefold().startswith('scenario/')
            and entry.name.lower().endswith('.s')
            for _, index in archives.values()
            for entry in index.entries
        )
        if not has_scenario:
            raise ValueError('Library/Avg 配置缺少 scenario 目录下的剧情脚本')
        return avg_sources
    raise ValueError('缺少 ImoScripter FormatType/ReturnCode 或 Library/Avg DefaultFormat=Multi 配置证据')


def _script_trial(entry, raw, layout, *, script_dialect='imos'):
    script, encoding, bom = script_format.decode(raw, unicode_scripts=layout.unicode_scripts)
    options = dict(unicode_scripts=layout.unicode_scripts, dialect=script_dialect)
    original = [record.row() for record in script_format.scan(script, **options)]
    row = next((i for i, value in enumerate(original) if value.get('message')), None)
    if row is None:
        raise ValueError('脚本小样没有可回填文本')
    if bom + script_format.patch(script, original, **options).encode(encoding) != raw:
        raise ValueError('脚本原文回填字节发生变化')

    translated = original.copy()
    message = original[row]['message']
    translated[row] = dict(original[row], message=message + '汉')
    changed = script_format.patch(script, translated, **options)
    trial = dict(member=entry.name, bytes=len(raw), unitCount=len(original), parseStatus='passed',
                 roundtripStatus='passed', scriptTranslationStatus='passed', encoding=encoding)
    try:
        encoded = bom + changed.encode(encoding)
    except UnicodeEncodeError:
        # Exercise the archive writer independently of the system-font trial.
        trial.update(translationStatus='failed', errorType='UnicodeEncodeError')
        translated[row] = dict(original[row], message=message + '字')
        encoded = bom + script_format.patch(script, translated, **options).encode(encoding)
    else:
        trial['translationStatus'] = 'passed'
    decoded, _, _ = script_format.decode(encoded, unicode_scripts=layout.unicode_scripts)
    if [record.row() for record in script_format.scan(decoded, **options)] != translated:
        raise ValueError('编码后的脚本回读与请求译文不符')
    return trial, encoded


def check_rebuild(layout, source, before, output, key, game_key, changes, *, limit):
    after = pack.read_index(output, layout)
    if (layout.tail(source, before) != layout.tail(output, after)
            or [entry.raw_name for entry in before.entries] != [entry.raw_name for entry in after.entries]):
        raise ValueError('QLIE 封包的成员顺序或查找表发生变化')
    for old, new in zip(before.entries, after.entries, strict=True):
        if old.encryption != new.encryption:
            raise ValueError('QLIE 封包成员的加密方式发生变化')
        if old.name in changes:
            actual = layout.read(output, after, new, key, game_key, limit)
            if new.packed or actual != changes[old.name]:
                raise ValueError(f'QLIE 封包成员回读与目标译文不一致（{old.name}）')
        elif (old.size, old.unpacked_size, old.packed, old.checksum) != (
                new.size, new.unpacked_size, new.packed, new.checksum
        ) or archive.hash_range(source, old.offset, old.size) != archive.hash_range(output, new.offset, new.size):
            raise ValueError(f'QLIE 封包未翻译成员发生变化（{old.name}）')


def probe(request):
    files = request['files']
    with ExitStack() as stack:
        streams = {path: stack.enter_context(Path(files[path]).open('rb')) for path in request['archives']}
        versions = {path: pack.version(stream) for path, stream in streams.items()}
        layout = pack.LAYOUTS.get(next(iter(versions.values())))
        report = dict(volumes=[dict(archive=path, version=version) for path, version in versions.items()])
        if layout is None or set(versions.values()) != {layout.version}:
            return dict(report, status='unsupported-layout', stage='index',
                        error='当前启动目录的 FilePack 版本没有匹配的组合读写布局')
        report.update(version=f'FilePackVer{layout.version}', keySource=layout.key_source,
                      stage='index', status='unavailable')
        try:
            volumes = {}
            for path, stream in streams.items():
                try:
                    volumes[path] = stream, pack.read_index(stream, layout)
                except (OSError, ValueError) as error:
                    return dict(report, failedArchive=path, error=str(error)[:240])
            return trial_volumes(request, report, layout, volumes)
        except (OSError, ValueError) as error:
            return dict(report, error=str(error)[:240])


def trial_volumes(request, report, layout, volumes):
    report['indexStatus'] = 'passed'
    report['stage'] = 'key'
    files = request['files']
    external_key = game_key = None
    if layout.key_source == 'external-and-executable':
        key_file = request['keyFile']
        external_key = _read_file(files[key_file], pack.KEY_BYTES)
        if len(external_key) < 256:
            raise ValueError('QLIE 外部密钥长度不足')
        executable_keys = []
        for exe in request['executables']:
            try:
                executable_keys.append((exe, qlie_keys.game_key_from_pe(_read_file(files[exe], MAX_EXE))))
            except (OSError, ValueError):
                continue
        if len(executable_keys) != 1:
            raise ValueError('未确认唯一带 IconKeyImage 的游戏 EXE')
        exe, game_key = executable_keys[0]
        report.update(executable=exe, keyFile=key_file)

    keys = {}
    config_hashes = {}

    def read(path, entry, limit):
        stream, index = volumes[path]
        if path not in keys:
            try:
                keys[path] = layout.key(stream, index, external_key, game_key)
            except ValueError as error:
                raise ValueError(f'{path} 卷内密钥：{error}') from error
        try:
            return layout.read(stream, index, entry, keys[path], game_key, limit)
        except ValueError as error:
            raise ValueError(f'{path}/{entry.name}：{error}') from error

    def read_config(path, entry, limit):
        raw = read(path, entry, limit)
        config_hashes[(path, entry.name)] = archive.digest(raw)
        return raw

    report['stage'] = 'script-profile'
    profile = script_profile(volumes, read_config, layout)
    script_dialect = dialect(profile)
    report['profile'] = profile
    eligible = [(path, entry) for path, (_, index) in volumes.items() for entry in index.entries
                if entry.name.lower().endswith('.s') and 0 < entry.size <= SAMPLE_BYTES
                and 0 < entry.unpacked_size <= SAMPLE_BYTES
                and (script_dialect != script_format.AVG_SCENARIO
                     or entry.name.replace('\\', '/').casefold().startswith('scenario/'))
                and (not layout.unicode_scripts or index.file_size <= SAMPLE_ARCHIVE_BYTES)]
    eligible.sort(key=lambda item: (not item[1].name.replace('\\', '/').casefold().startswith('scenario/'),
                                    not (item[1].packed and item[1].encryption),
                                    -item[1].unpacked_size, item[0], item[1].name))

    proof = None
    if layout.unicode_scripts:
        indices = [(path, archive.hash_range(stream, index.index_offset, index.file_size - index.index_offset))
                   for path, (stream, index) in volumes.items()]
        proof = archive.digest(json.dumps([indices, sorted(config_hashes.items())]).encode())
        previous = request.get('previous') or {}
        previous_path = previous.get('archive')
        if previous.get('indexFingerprint') == proof and any(path == previous_path for path, _ in eligible):
            stream, index = volumes[previous_path]
            if previous.get('sampleHash') == archive.hash_range(stream, 0, index.file_size):
                return dict(previous, reused=True)

    report.update(stage='script', error='封包中没有预算内的 QLIE 脚本小样')
    for path, entry in eligible[:6]:
        stream, index = volumes[path]
        try:
            raw = read(path, entry, SAMPLE_BYTES)
            trial, replacement = _script_trial(entry, raw, layout, script_dialect=script_dialect)
        except ValueError as error:
            if 'trials' not in report:
                report.update(error=str(error)[:240])
            continue
        report.update(stage='archive-rebuild', archive=path, trials=[trial], memberReadStatus='passed')
        try:
            tail = layout.tail(stream, index)
            if layout.unicode_scripts:
                changes = {entry.name: replacement}
                with tempfile.TemporaryFile() as output:
                    layout.rebuild(stream, output, index, keys[path], external_key, game_key, changes)
                    check_rebuild(layout, stream, index, output, keys[path], game_key, changes, limit=2 * SAMPLE_BYTES)
                trial['archiveRoundtripStatus'] = 'passed'
        except ValueError as error:
            report['error'] = str(error)[:240]
            continue
        writer_layout = 'HashVer1.4' if tail.startswith(b'HashVer1.4\0\0\0\0\0\0') else 'HashVer1.3'
        result = dict(report, status='sampled', stage='completed', templateId=layout.template,
                      writerLayout=writer_layout)
        result.pop('error', None)
        if proof:
            result.update(indexFingerprint=proof, sampleHash=archive.hash_range(stream, 0, index.file_size))
        return result
    return report
