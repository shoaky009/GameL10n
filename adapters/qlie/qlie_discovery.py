"""Bounded probes and complete keyed PE/archive discovery."""
from pathlib import Path
import re

from qlie_archive import Archive
from qlie_pe import key30, key31, ensure_unicode
from qlie_script import decode

LAUNCH = '启动汉化.exe'
MAP = 'qlie-l10n/map.json'


def norm(path):
    return str(path).replace('\\', '/')


def full(root, relative):
    root = Path(root).resolve()
    path = root.joinpath(norm(relative)).resolve()
    if not path.is_relative_to(root) or path == root:
        raise ValueError('资源路径越界。')
    return path


def ordered(path):
    return re.sub(r'\d+', lambda m: m[0].rjust(16, '0'), norm(path).lower())


def header(root, path, size=65536):
    with full(root, path).open('rb') as f:
        return f.read(size)


def unique(paths):
    return list({p.lower(): p for p in paths}.values())


class Discovery:
    def __init__(self, root, files, variant, probe=False):
        self.root, self.files, self.variant = root, files, variant
        self.unicode = variant['unicode']
        self.modern = variant['version'] == '9a44e166fa6aff9d'
        self.archives = []
        self.executable = self.config = self.game_key = None
        self.keys = [p for p in files if p.lower().endswith('.fkey')]
        self.configs = [p for p in files if Path(p).name.lower() == 'config.txt']
        self.binaries = [p for p in files if p.lower().endswith(('.exe', '.bin') if self.modern else ('.exe',))
                         and (variant['version'] == '6cd70ca295b40fb4' or p.lower() != LAUNCH.lower())]
        if self.unicode:
            self.configs = [p for p in self.configs if self.has_fields(p, 'Font[0].Name=', 'MessageSpeed=', 'CRCCheck=')]
        self.packs = [p for p in files if (p.lower().endswith('.pack') if self.modern or self.unicode else Path(p).name.lower() == 'data0.pack')]
        if self.unicode and len(self.configs) == 1:
            directory = norm(Path(self.configs[0]).parent.parent).rstrip('/')
            if directory == '.':
                directory = ''
            self.packs = [p for p in self.packs if ('' if str(Path(p).parent) == '.' else norm(Path(p).parent)).lower() == directory.lower()]
        if self.modern or self.unicode:
            self.packs.sort(key=ordered)
        if not probe:
            self.read()

    def has_fields(self, path, *fields):
        data = header(self.root, path)
        return data.startswith(b'\xff\xfe') and all(s in data[2:len(data)//2*2].decode('utf-16le') for s in fields)

    def probe(self):
        exes = []
        for p in self.binaries:
            data = header(self.root, p, 64)
            if data.startswith(b'MZ') and (not self.unicode or len(data) >= 64):
                exes.append(p)
        if self.unicode:
            messages = [p for p in self.files if Path(p).name.lower() == 'engine_message.u.txt' and
                        self.has_fields(p, 'filepack_initializeerror=', 'filepack_keyerror=')]
            gui = [p for p in self.files if Path(p).name.lower() == 'engine_gui.u.txt' and
                   self.has_fields(p, '[LocaleFontnameTable]', 'SHIFTJIS_CHARSET')]
            if len(self.configs) != 1 or len(messages) != 1 or len(gui) != 1:
                return None
            profiled = [e for e in exes if any(p.lower() == (e+'.le.config').lower() for p in self.files)]
            if len(profiled) == 1:
                exes = profiled
            if not self.packs or len(exes) != 1:
                return None
            required = self.packs+self.configs+messages+gui+exes+[p for p in self.files if p.lower() == (exes[0]+'.le.config').lower()]
            original = exes[0]
        else:
            if (len(self.keys) != 1 or len(self.configs) != 1 or not self.packs or
                (not self.modern and len(self.packs) != 1) or
                not header(self.root, self.keys[0], len(b'KeyFile ver1.0')).startswith(b'KeyFile ver1.0') or
                not any(p.lower().endswith('.exe') for p in exes)):
                return None
            if self.modern:
                extras = [p for p in self.files if p.lower() == MAP or any(p.lower() == (b+'.le.config').lower() for b in exes)]
            elif self.variant['version'] == '6cd70ca295b40fb4':
                extras = [p for p in self.files if p.lower().startswith('qlie-l10n/')]
            else:
                extras = [p for p in self.files if p.lower().endswith('.exe.le.config')]
            required = self.packs+self.keys+exes+self.configs+extras
            original = None
        response = dict(self.variant['probe'])
        response['requiredFiles'] = sorted(unique(required)) if self.modern or self.unicode else list(dict.fromkeys(required))
        response['originalExecutable'] = original
        return response

    def read(self):
        if len(self.configs) != 1 or not self.packs or (not self.unicode and len(self.keys) != 1):
            raise ValueError('需要封包、唯一密钥和受支持的外置 config.txt。')
        if not self.modern and not self.unicode and len(self.packs) != 1:
            raise ValueError('此版本仅支持唯一 data0.pack。')
        self.config = self.configs[0]
        binaries, keys = {}, {}
        for p in self.binaries:
            data = full(self.root, p).read_bytes()
            if (self.modern or self.unicode) and not data.startswith(b'MZ'):
                continue
            binaries[p] = data
            try:
                key = key31(data) if self.unicode else key30(data)
                if not self.unicode or len(key) >= 128:
                    keys[p] = key
            except (ValueError, IndexError, UnicodeError, OverflowError):
                continue
        candidates = {}
        for p, data in binaries.items():
            if not p.lower().endswith('.exe'):
                continue
            matches = [k for k in keys if k == p or (self.modern and k.lower().endswith('.bin') and
                       len(binaries[k]) >= 512 and len(data) > len(binaries[k]) and binaries[k] in data)]
            if matches:
                candidates[p] = matches
        if len(candidates) != 1:
            raise ValueError('未找到唯一的 QLIE 游戏启动入口及其密钥。')
        self.executable, matches = next(iter(candidates.items()))
        self.game_key = keys[matches[0]]
        if any(keys[p] != self.game_key for p in matches):
            raise ValueError('启动入口关联多个不同密钥的引擎。')
        if self.unicode:
            ensure_unicode(binaries[self.executable])
            key, game_key = self.game_key, None
        else:
            key, game_key = full(self.root, self.keys[0]).read_bytes(), self.game_key
            if self.modern and not key.startswith(b'KeyFile ver1.0'):
                raise ValueError('fkey 版本不受支持。')
        for p in self.packs:
            archive = Archive(full(self.root, p), key, game_key, self.unicode)
            if self.unicode:
                relevant = any(e.name.lower().endswith(('.s', 'fontdata.txt', 'fontlist.txt')) or
                               norm(e.name).lower() == 'avg/setting.txt' for e in archive.entries)
            else:
                relevant = any(e.name.lower().endswith('.s') or norm(e.name).lower() == 'system/font/fontdata.dat' for e in archive.entries)
            if relevant or not self.modern and not self.unicode:
                self.archives.append((p, archive))
        if not self.archives:
            raise ValueError('封包中没有脚本或系统字体布局。')
