"""Sparse XP3 probes request bounded missing index/member ranges."""
from contextlib import nullcontext
from pathlib import Path

from kirikiri_text import Script
from xp3_adler import MAGIC, read_index, read_member
from xp3_container import read_member as read_archive_member


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


def rot_probe(request):
    files = request.get('probeFiles')
    if not files:
        files = [dict(path=path, length=(Path(request['sourceDirectory']) / path).stat().st_size)
                 for path in request.get('resourceFiles', [])]
    reads, selected, warnings = [], [], []
    for file in files:
        if not file['path'].lower().endswith('.xp3'):
            continue
        try:
            source = (nullcontext(View(request, file)) if request.get('probeFiles')
                      else (Path(request['sourceDirectory']) / file['path']).open('rb'))
            with source as view:
                index = read_index(view)
                candidates = [entry for entry in index.entries
                              if entry.name.lower().endswith(('.ks', '.ma'))
                              and 0 < entry.raw_size <= 524288 and entry.packed_size <= 524288]
                if not candidates:
                    continue
                entry = min(candidates, key=lambda item: item.packed_size)
                Script(read_archive_member(view, entry, 'rot', max_size=524288))
            selected.append(file['path'].replace('\\', '/'))
        except Missing as missing:
            reads.append(dict(path=file['path'], offset=missing.offset, length=missing.count))
        except Exception as error:
            warnings.append(file['path'] + '：' + str(error))
    if reads:
        return dict(probeReads=reads)
    if not selected:
        if warnings:
            raise ValueError('XP3 索引或脚本与当前过滤器不兼容：' + '；'.join(warnings[:3]))
        return dict(probe=None)
    return dict(probe=dict(engineId='kirikiri-xp3-text', adapterId='kirikiri-xp3-text', adapterVersion='1.0.0',
                           description='KiriKiri Z XP3: KS(KAG)/ma 文本提取，中文回写为重建的同路径 XP3 归档',
                           requiredFiles=selected, originalExecutable=None, warnings=warnings + [
                               '覆盖范围：.ks 剧情/选项与 testo/*.ma 菜单提示文本；.tjs 脚本内 UI/系统消息、图片与视频内嵌文字未提取。',
                               '中文回写为重建同路径 XP3（未改动成员保留原始压缩数据），无需额外 patch 归档。',
                               '未验证运行时：中文脚本（UTF-8）解码、字体字形覆盖与 ACP=936 环境下的窗口/界面文字。']))


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
