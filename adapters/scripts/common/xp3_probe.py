"""Sparse XP3 probes request bounded missing index/member ranges."""
from pathlib import Path
import struct
import zlib

from kirikiri_text import Script
from xp3_adler import MAGIC, read_index, read_member


class Missing(Exception):
    def __init__(self, offset, count):
        self.offset, self.count = offset, count


class View:
    def __init__(self, request, file):
        self.size = file['length']
        self.position = 0
        self.ranges = [(0, (Path(request['sourceDirectory']) / file['path']).read_bytes())]
        self.ranges += [(item['offset'], Path(item['dataPath']).read_bytes()) for item in request.get('probeRanges', []) if item['path'] == file['path']]

    def tell(self):
        return self.position

    def seek(self, offset, whence=0):
        self.position = offset + (self.position if whence == 1 else self.size if whence == 2 else 0)
        return self.position

    def read(self, count=-1):
        if self.position >= self.size:
            return b''
        count = min(count if count >= 0 else self.size - self.position, self.size - self.position)
        output = bytearray()
        while len(output) < count:
            found = next(((start, data) for start, data in self.ranges if start <= self.position < start + len(data)), None)
            if found is None:
                following = min((start for start, _ in self.ranges if start > self.position), default=self.size)
                raise Missing(self.position, min(1048576, following - self.position, max(65536, count - len(output))))
            start, data = found
            part = data[self.position - start:self.position - start + count - len(output)]
            output += part
            self.position += len(part)
        return bytes(output)


def adler_probe(request):
    files = request.get('probeFiles')
    if not files:
        files = [dict(path=path, length=(Path(request['sourceDirectory']) / path).stat().st_size) for path in request.get('resourceFiles', [])]
    reads, selected, warnings = [], [], []
    for file in files:
        if not file['path'].lower().endswith('.xp3'):
            continue
        try:
            view = View(request, file)
            index = read_index(view)
            candidates = [entry for entry in index.entries if entry.name.lower().endswith(('.ks', '.tjs', '.ma', '.csv')) and 0 < entry.raw_size <= 524288 and entry.packed_size <= 524288]
            if not candidates:
                continue
            entry = min(candidates, key=lambda item: item.packed_size)
            Script(read_member(view, entry, max_size=524288))
            selected.append(file['path'].replace('\\', '/'))
        except Missing as missing:
            reads.append(dict(path=file['path'], offset=missing.offset, length=missing.count))
        except Exception as error:
            warnings.append(file['path'] + '：' + str(error))
    if reads:
        return dict(probeReads=reads)
    if not selected:
        return dict(probe=None)
    warnings += ['覆盖 KS 剧情、注音、名称及已识别选项、TJS caption/hint/title 标签、已确认的静态输入框标签、jp 字符串表和背景列表显示名称；其他动态界面及潜在文本类别未全面调查，图片与视频文字除外。',
                 '中文按原 Unicode 编码回填并重建同路径 XP3；实际启动、TFT 缺字回退与排版仍待游戏内验证。']
    return dict(probe=dict(engineId='kirikiri-xp3-adler-filter', adapterId='kirikiri-xp3-adler-filter', adapterVersion='1.0.0',
                           description='KiriKiri XP3 / Adler 派生过滤器 / KS、TJS、字符串表', requiredFiles=selected,
                           warnings=warnings, originalExecutable=None))
