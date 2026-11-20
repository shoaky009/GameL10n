"""FilePack layouts that can supply a QLIE script adapter."""
import re

from python.archives import qlie, qlie_writer

from . import engine_archive as archive
from . import qlie_pack31


KEY_BYTES = 1 << 20
MAX_INDEX = 2 << 20
MAX_TAIL = 4 << 20
MAX_INDEX_REGION = MAX_INDEX + MAX_TAIL + 28
FOOTER_VERSION = re.compile(rb"FilePackVer([0-9]\.[0-9])\0\0")


def version(stream):
    size = stream.seek(0, 2)
    if size is None:
        size = stream.tell()
    if size < 28:
        return None
    signature = qlie_pack31.read_at(stream, size - 28, 16)
    match = FOOTER_VERSION.fullmatch(signature)
    return match[1].decode("ascii") if match else None


class Pack30:
    version = "3.0"
    template = "qlie-pack3-imos"
    key_source = "external-and-executable"
    unicode_scripts = False

    def index(self, stream):
        return qlie.read_index(stream, max_entries=16_000, max_index_bytes=MAX_INDEX)

    def tail(self, stream, index):
        return qlie_writer.hash13_tail(stream, index, max_bytes=MAX_TAIL)

    def key(self, stream, index, external_key, game_key):
        first = index.entries[0]
        if first.name != qlie.PACK_KEY_NAME or any(e.name == qlie.PACK_KEY_NAME for e in index.entries[1:]):
            raise ValueError("FilePack 3.0 需要唯一的首项卷内密钥")
        return self.read(stream, index, first, external_key, game_key, KEY_BYTES)

    def read(self, stream, index, entry, key, game_key, limit):
        return qlie.read_member(stream, index, entry, mode="keyed", key_file=key,
                                game_key=game_key, max_stored=limit, max_output=limit)

    def rebuild(self, source, output, index, key, external_key, game_key, replacements):
        qlie_writer.rebuild(source, output, replacements, mode="keyed",
                            key_file=external_key, game_key=game_key)


class Pack31:
    version = qlie_pack31.VERSION
    template = "qlie-pack31-imos"
    key_source = "embedded"
    unicode_scripts = True

    def index(self, stream):
        return qlie_pack31.read_index(stream)

    def tail(self, stream, index):
        return qlie_pack31.writer_tail(stream, index)

    def key(self, stream, index, external_key, game_key):
        return qlie_pack31.volume_key(stream, index, KEY_BYTES)

    def read(self, stream, index, entry, key, game_key, limit):
        return qlie_pack31.read_member(stream, index, entry, key, limit)

    def rebuild(self, source, output, index, key, external_key, game_key, replacements):
        qlie_pack31.rebuild(source, output, index, key, replacements)


LAYOUTS = {layout.version: layout for layout in (Pack30(), Pack31())}


def read_index(stream, layout):
    index = layout.index(stream)
    if index.file_size - index.index_offset > MAX_INDEX_REGION:
        raise ValueError("QLIE 封包索引超出读取范围")
    archive.validate_names(index.entries)
    return index
