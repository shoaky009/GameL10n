"""Aligned KaGuYa TBLSTR XOR-FF tables with an ARI primary-record index.

The read_table/units/rebuild API and IDs match the kaguya_table reference.
Only types 0, 2 and 3 are supported; confirm their text semantics separately.
Unedited records retain every original byte, including nonzero alignment
padding. Custom codecs support independently established encoding routes;
this module does not install a runtime, modify an EXE, or prove rendering.

Read-only JSON CLI: {"operation":"probe","input":"TBLSTR.ARC",
                     "index":"TBLSTR.ARI"}
"""
from dataclasses import dataclass
import json
import os
from pathlib import Path
import stat
import struct
import sys


MAX_TABLE_BYTES = 64 << 20
MAX_RECORDS = 500_000
MAX_INDEX_BYTES = (MAX_RECORDS + 1) * 4
FORMAT = 'kaguya-tblstr-xor-ff-aligned'
KINDS = {0: 'dialogue', 2: 'name', 3: 'choice'}


@dataclass
class Record:
    offset: int
    type: int
    raw: bytes
    text: str
    group: int
    field: int


def _structure(data):
    if len(data) > MAX_TABLE_BYTES:
        raise ValueError('TBLSTR exceeds the 64 MiB table limit')
    if len(data) < 16 or struct.unpack_from('<I', data)[0] != len(data):
        raise ValueError('TBLSTR total length does not match the ARC bytes')
    records = []
    pos, group, field = 4, -1, 0
    while pos < len(data):
        if len(records) >= MAX_RECORDS:
            raise ValueError('TBLSTR record count limit exceeded')
        if pos + 8 > len(data):
            raise ValueError(f'TBLSTR record header is truncated at {pos:#x}')
        kind, length = struct.unpack_from('<II', data, pos)
        if kind not in KINDS:
            raise ValueError(f'Unsupported TBLSTR record type {kind} at {pos:#x}')
        if not length or length % 4 or pos + 8 + length > len(data):
            raise ValueError(f'Invalid aligned TBLSTR payload length at {pos:#x}')
        end = data.find(b'\0', pos + 8, pos + 8 + length)
        if end < 0:
            raise ValueError(f'TBLSTR terminator is missing at {pos:#x}')
        text_length = end - pos - 8
        if length != ((text_length + 4) & ~3):
            raise ValueError(f'TBLSTR payload has bytes beyond alignment padding at {pos:#x}')
        if kind == 0:
            group += 1
            field = 0
        elif group < 0:
            raise ValueError('TBLSTR secondary record precedes the first primary record')
        records.append(Record(pos, kind, data[pos:pos + 8 + length], '', group, field))
        field += 1
        pos += 8 + length
    return records


def _index(index, records):
    if len(index) < 8 or len(index) % 4 or len(index) > MAX_INDEX_BYTES:
        raise ValueError('TBLSTR ARI length is invalid or exceeds the index limit')
    if struct.unpack_from('<I', index)[0] != len(index) // 4:
        raise ValueError('TBLSTR ARI count must include its own count element')
    primary = (r.offset for r in records if r.type == 0)
    addresses = (item[0] for item in struct.iter_unpack('<I', index[4:]))
    sentinel = object()
    while True:
        expected, actual = next(primary, sentinel), next(addresses, sentinel)
        if expected is sentinel and actual is sentinel:
            return
        if expected != actual:
            raise ValueError('TBLSTR ARI addresses do not exactly match primary records')


def _decode(records, decode):
    for record in records:
        payload = record.raw[8:]
        end = payload.index(0)
        record.text = decode(bytes(value ^ 255 for value in payload[:end]))
        if not isinstance(record.text, str):
            raise ValueError('TBLSTR decoder must return text')
    return records


def read_table(data, index, decode=None):
    """Validate ARC/ARI completely, then decode text (strict CP932 by default)."""
    records = _structure(data)
    _index(index, records)
    return _decode(records, decode or (lambda value: value.decode('cp932')))


def locator(record):
    return f'group:{record.group}/field:{record.field}/type:{record.type}'


def unit_id(path, record):
    return path + '#' + locator(record)


def units(path, records):
    return [dict(id=unit_id(path, r), resourcePath=path, locator=locator(r),
                 sourceText=r.text, context=f'TBLSTR group {r.group}, field {r.field}',
                 speaker=None, kind=KINDS[r.type])
            for r in records if r.text.strip()]


def rebuild(path, records, translations, encode):
    """Return ARC/ARI bytes; offsets change, semantic group/field IDs do not."""
    output = bytearray(4)
    offsets = []
    count = 0
    for record in records:
        count += 1
        if count > MAX_RECORDS:
            raise ValueError('TBLSTR record count limit exceeded')
        if record.type == 0:
            offsets.append(len(output))
        text = translations.get(unit_id(path, record), record.text)
        if not isinstance(text, str):
            raise ValueError('TBLSTR translation must be text')
        if text == record.text:
            raw = record.raw
        else:
            encoded = encode(text)
            if b'\xff' in encoded or b'\0' in encoded:
                raise ValueError('Translation encoding conflicts with the TBLSTR terminator')
            payload = bytes(value ^ 255 for value in encoded) + b'\0'
            payload += bytes((-len(payload)) % 4)
            raw = struct.pack('<II', record.type, len(payload)) + payload
        if len(output) + len(raw) > MAX_TABLE_BYTES:
            raise ValueError('TBLSTR rebuild exceeds the 64 MiB table limit')
        output.extend(raw)
    struct.pack_into('<I', output, 0, len(output))
    index = struct.pack('<I', len(offsets) + 1) + b''.join(struct.pack('<I', p) for p in offsets)
    return bytes(output), index


def _read_file(path, maximum):
    with Path(path).open('rb') as stream:
        if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
            raise ValueError('TBLSTR input must be a regular file')
        data = stream.read(maximum + 1)
    if len(data) > maximum:
        raise ValueError('TBLSTR input size limit exceeded')
    return data


def _chinese_capability():
    sample = '中文测试'
    try:
        sample.encode('cp932')
        capable = True
    except UnicodeEncodeError:
        capable = False
    return dict(sample=sample, canEncode=capable,
                scope='Encoding only; Chinese rendering requires independent runtime evidence')


def probe(input_path, index_path, sample_limit=2):
    """Inspect files without writing; matched establishes this table variant only."""
    result = dict(status='rejected', format=FORMAT,
                  checks=dict(structure=None, index=None, encoding=None, byteRoundtrip=None),
                  counts=None, encoding='cp932', cp932Chinese=_chinese_capability(),
                  runtimeTested=False, samples=[])
    checks = result['checks']
    try:
        if type(sample_limit) is not int or not 0 <= sample_limit <= 2:
            raise ValueError('sampleLimit must be an integer from 0 to 2')
        if not input_path or not index_path:
            result.update(status='needs-evidence', reason='Both ARC input and companion ARI index paths are required')
            return result
        data = _read_file(input_path, MAX_TABLE_BYTES)
        index = _read_file(index_path, MAX_INDEX_BYTES)
        checks['structure'] = False
        records = _structure(data)
        checks['structure'] = True
        counts = {str(kind): 0 for kind in KINDS}
        for record in records:
            counts[str(record.type)] += 1
        result['counts'] = dict(records=len(records), primary=counts['0'], types=counts)
        checks['index'] = False
        _index(index, records)
        checks['index'] = True
        checks['encoding'] = False
        _decode(records, lambda value: value.decode('cp932'))
        checks['encoding'] = True
        checks['byteRoundtrip'] = False
        rebuilt, rebuilt_index = rebuild(str(input_path), records, {}, lambda text: text.encode('cp932'))
        checks['byteRoundtrip'] = rebuilt == data and rebuilt_index == index
        if not checks['byteRoundtrip']:
            raise ValueError('TBLSTR original-byte roundtrip failed')
        nonblank = any(record.text.strip() for record in records)
        for record in records if sample_limit else []:
            if record.text.strip():
                result['samples'].append(dict(locator=locator(record), type=record.type, text=record.text[:100]))
                if len(result['samples']) == sample_limit:
                    break
        if nonblank:
            result.update(status='matched', reason='Aligned XOR-FF CP932 table and primary-record index matched')
        else:
            result.update(status='needs-evidence', reason='Structure matched, but no nonblank text confirms text semantics')
    except FileNotFoundError:
        result.update(status='needs-evidence', reason='A required ARC or ARI file is missing')
    except (OSError, ValueError, TypeError) as error:
        result['reason'] = str(error)[:300]
    return result


def main():
    try:
        raw = sys.stdin.read(16_385)
        if len(raw) > 16_384:
            raise ValueError('Probe request exceeds 16 KiB')
        request = json.loads(raw)
        if not isinstance(request, dict) or request.get('operation') != 'probe':
            raise ValueError('operation must be probe')
        result = probe(request.get('input'), request.get('index'), request.get('sampleLimit', 2))
    except (ValueError, TypeError) as error:
        result = dict(status='rejected', format=FORMAT, reason=str(error)[:300], runtimeTested=False)
    print(json.dumps(result, ensure_ascii=True))
    return 0 if result['status'] == 'matched' else 1


if __name__ == '__main__':
    raise SystemExit(main())
