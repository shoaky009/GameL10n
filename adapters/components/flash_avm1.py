"""UTF-8 AVM1 constant pools in FWS/CWS timeline and clip actions."""
import base64
import bisect
from collections import defaultdict
import json
import re
import struct
import zlib


MAX_MOVIE = 128 << 20
LIMIT = 65535
META = re.compile(rb"<\?gamel10n-avm1-v1 ([A-Za-z0-9+/=]+)\?>")
CJK = re.compile(r"[\u3000-\u303f\u3040-\u30ff\u3400-\u9fff\uff00-\uffef…―—]")


def u16(data, offset):
    return struct.unpack_from("<H", data, offset)[0]


def fail(message):
    raise ValueError(message)


def classify(text):
    if not text.strip() or not CJK.search(text):
        return None
    if text.startswith("※"):
        return "system"
    if text.startswith(("「", "（")):
        return "dialogue"
    if len(text) <= 12 and not any(c in text for c in "。！？!?"):
        return "name"
    return "dialogue"


def bit_field_end(data, pos, matrix):
    bit = pos * 8

    def bits(count):
        nonlocal bit
        if bit + count > len(data) * 8:
            fail("truncated SWF bit field")
        value = 0
        for _ in range(count):
            value = value * 2 + ((data[bit // 8] >> (7 - bit % 8)) & 1)
            bit += 1
        return value

    if matrix:
        for _ in range(2):
            if bits(1):
                width = bits(5)
                bit += 2 * width
        width = bits(5)
        bit += 2 * width
    else:
        add, mul, width = bits(1), bits(1), bits(4)
        bit += (add + mul) * width * 4
    if bit > len(data) * 8:
        fail("truncated PlaceObject2 transform")
    return (bit + 7) // 8


def clip_records(data):
    if not data or not data[0] & 128:
        return
    flags, p = data[0], 3
    if flags & 2:
        p += 2
    if flags & 4:
        p = bit_field_end(data, p, True)
    if flags & 8:
        p = bit_field_end(data, p, False)
    if flags & 16:
        p += 2
    if flags & 32:
        p = data.index(0, p) + 1
    if flags & 64:
        p += 2
    p += 6  # Reserved UI16, AllEventFlags UI32 in SWF 6+
    while p + 4 <= len(data):
        events = int.from_bytes(data[p:p + 4], "little")
        if not events:
            if p + 4 != len(data):
                fail("trailing data after ClipActions")
            return
        size = struct.unpack_from("<I", data, p + 4)[0]
        start = p + 8 + bool(events & 0x20000)
        end = p + 8 + size
        if not start <= end <= len(data):
            fail("ClipActionRecord exceeds PlaceObject2")
        yield p + 4, start, end
        p = end
    fail("unterminated ClipActions")


def push_values(payload):
    widths = (None, 4, 0, 0, 1, 1, 8, 4, 1, 2)
    p = 0
    values = []
    while p < len(payload):
        start, kind = p, payload[p]
        p += 1
        if kind == 0:
            p = payload.index(0, p) + 1
        elif kind < len(widths):
            p += widths[kind]
        else:
            fail(f"unknown AVM1 Push operand type {kind}")
        if p > len(payload):
            fail("truncated AVM1 Push operand")
        values.append(payload[start:p])
    return values


def pool_strings(data):
    length = u16(data, 1)
    payload = data[3:3 + length]
    if len(payload) != length or len(payload) < 2 or (len(payload) > 2 and payload[-1] != 0):
        fail("invalid AVM1 ConstantPool length")
    strings = payload[2:].split(b"\0")[:-1]
    if len(strings) != u16(payload, 0):
        fail("AVM1 ConstantPool count differs from its payload")
    return strings


class PoolScript:
    """One activation with an initial pool and flat AVM1 actions."""
    def __init__(self, data, offset, slots, mapping):
        self.offset = offset
        self.slots = slots
        self.actions = []
        self.pushes = {}
        self.references = defaultdict(list)
        self.targets = {}
        self.fields = {}
        self.inline = {}
        starts = {}
        cursor = 0
        while cursor < len(data):
            start = cursor
            opcode = data[cursor]
            cursor += 1
            size = 0
            if opcode >= 128:
                size = u16(data, cursor)
                cursor += 2
            end = cursor + size
            i = len(self.actions)
            if end > len(data) or (opcode == 0 and end != len(data)):
                fail(f"invalid AVM1 instruction boundary at {offset + start}")
            if opcode in (0x8e, 0x8f, 0x94, 0x9b) or (opcode == 0x88 and i):
                fail(f"nested action or additional pool 0x{opcode:02x} at {offset + start}")
            payload = data[cursor:end]
            starts[start] = i
            self.actions.append((opcode, payload))
            if opcode == 0x96:
                values = push_values(payload)
                self.pushes[i] = values
                for operand, value in enumerate(values):
                    if value[0] in (8, 9):
                        slot = int.from_bytes(value[1:], "little")
                        if slot >= len(slots):
                            fail(f"Push at {offset + start} refers to missing pool slot {slot}")
                        self.references[slot].append((i, operand))
            elif opcode in (0x99, 0x9d):
                if size != 2:
                    fail(f"invalid branch length at {offset + start}")
                self.targets[i] = end + struct.unpack("<h", payload)[0]
            cursor = end
        starts[len(data)] = len(self.actions)
        self.raw_targets = set()
        for i, target in self.targets.items():
            if target in starts:
                self.targets[i] = starts[target]
            elif 0 < target < 3 + len(self.actions[0][1]):
                # AVM1 can enter a byte within a long record. Keep that pool verbatim.
                self.raw_targets.add(target)
                self.targets[i] = ("pool-byte", target)
            else:
                fail(f"unmapped AVM1 branch target at {offset + target}")
        self.collapse_relays(mapping.get("relays", []))
        self.kinds = {}
        occupied = set()
        for key, item in mapping.get("fields", {}).items():
            slot = int(key)
            if not 0 <= slot < len(slots) or not isinstance(item, list) or not item:
                fail("invalid AVM1 field locator")
            kind, *sites = item
            if kind not in ("system", "dialogue", "name"):
                fail("invalid AVM1 field kind")
            self.kinds[slot] = kind
            if sites:
                if any(not isinstance(site, list) or len(site) != 2
                       or any(type(n) is not int for n in site) for site in sites):
                    fail("invalid AVM1 inline Push locator")
                sites = [tuple(site) for site in sites]
                for i, operand in sites:
                    if (i not in self.pushes or not 0 <= operand < len(self.pushes[i])
                            or self.pushes[i][operand][0] != 0 or (i, operand) in occupied
                            or self.references[slot]):
                        fail("AVM1 inline locator differs from bytecode")
                    occupied.add((i, operand))
                self.inline[slot] = sites
        for slot, raw in enumerate(slots):
            if slot in self.inline:
                literals = {self.pushes[i][operand][1:-1] for i, operand in self.inline[slot]}
                if len(literals) != 1:
                    fail("inconsistent inline values for one AVM1 pool slot")
                raw = literals.pop()
            text = raw.decode("utf-8")
            kind = self.kinds.get(slot) or classify(text)
            if kind:
                self.fields[slot] = (text, kind)

    def collapse_relays(self, relays):
        if not relays:
            return
        hidden = set()
        for i in relays:
            if (not isinstance(i, int) or i < 1 or i + 1 >= len(self.actions)
                    or self.actions[i] != (0x99, b"\x05\x00") or self.actions[i + 1][0] != 0x99
                    or hidden.intersection((i, i + 1))):
                fail("invalid AVM1 branch relay")
            hidden.update((i, i + 1))
        order = [i for i in range(len(self.actions)) if i not in hidden]
        indices = {i: n for n, i in enumerate([*order, len(self.actions)])}
        targets = {}
        for i, target in self.targets.items():
            if i in hidden:
                continue
            seen = set()
            while target in hidden:
                if target in seen:
                    fail("cyclic AVM1 relay")
                seen.add(target)
                target = self.targets[target]
            targets[indices[i]] = target if isinstance(target, tuple) else indices[target]
        self.targets = targets
        self.actions = [self.actions[i] for i in order]
        self.pushes = {indices[i]: values for i, values in self.pushes.items()}
        for slot, sites in self.references.items():
            self.references[slot] = [(indices[i], n) for i, n in sites]

    def signature(self):
        """Logical operands and instruction-index branch targets, independent of byte sizes."""
        inline = {site: slot for slot, sites in self.inline.items() for site in sites}
        for i, (opcode, payload) in enumerate(self.actions):
            if opcode == 0x88:
                payload = tuple(None if j in self.fields else v for j, v in enumerate(self.slots))
            elif opcode == 0x96:
                operands = []
                for n, value in enumerate(self.pushes[i]):
                    if (i, n) in inline:
                        operands.append(inline[i, n])
                    elif value[0] in (8, 9):
                        operands.append(int.from_bytes(value[1:], "little"))
                    else:
                        operands.append(value)
                payload = tuple(operands)
            elif i in self.targets:
                payload = self.targets[i]
            yield opcode, payload

    def build(self, edits):
        slots = self.slots.copy()
        pushes = {i: values.copy() for i, values in self.pushes.items()}
        inline = self.inline.copy()
        kinds = self.kinds.copy()
        for slot, text in edits.items():
            if not text.strip() or "\0" in text:
                fail(f"invalid text for AVM1 slot {slot}")
            raw = text.encode("utf-8")
            kind = self.fields[slot][1]
            if classify(text) != kind:
                kinds[slot] = kind
            elif slot in kinds:
                del kinds[slot]
            if slot in inline or self.raw_targets:
                sites = inline.get(slot) or self.references[slot]
                if not sites:
                    fail(f"pool at {self.offset}: slot {slot} has no editable Push reference")
                for i, operand in sites:
                    pushes[i][operand] = b"\0" + raw + b"\0"
                inline[slot] = sites
            else:
                slots[slot] = raw

        size = 2 + sum(len(value) + 1 for value in slots)
        push_sizes = {i: sum(map(len, values)) for i, values in pushes.items()}
        if size > LIMIT:
            # Move only edited, uniquely referenced entries. Other pool consumers retain
            # their original slot bytes, including later activations inheriting this pool.
            candidates = sorted((len(slots[j]) - len(self.slots[j]), j) for j in edits
                                if j not in inline and len(self.references[j]) == 1)
            for saving, slot in reversed(candidates):
                if size <= LIMIT or saving <= 0:
                    break
                i, operand = self.references[slot][0]
                literal = b"\0" + slots[slot] + b"\0"
                growth = len(literal) - len(pushes[i][operand])
                if push_sizes[i] + growth > LIMIT:
                    continue
                pushes[i][operand] = literal
                push_sizes[i] += growth
                slots[slot] = self.slots[slot]
                inline[slot] = [(i, operand)]
                size -= saving
        if size > LIMIT:
            fail(f"ActionConstantPool at {self.offset}: original={len(self.actions[0][1])}, "
                 f"required={size}, limit={LIMIT}; insufficient eligible single-use fields")
        payloads = [v for _, v in self.actions]
        payloads[0] = struct.pack("<H", len(slots)) + b"".join(v + b"\0" for v in slots)
        for i, values in pushes.items():
            payloads[i] = b"".join(values)
        bytecode, relays = encode_actions(self.actions, payloads, self.targets, self.offset)
        fields = {str(slot): [kind] for slot, kind in kinds.items()}
        for slot, sites in inline.items():
            fields[str(slot)] = [self.fields[slot][1], *sites]
        mapping = {}
        if fields:
            mapping["fields"] = fields
        if relays:
            mapping["relays"] = relays
        return bytecode, mapping


def encode_actions(actions, payloads, targets, block_offset):
    """Route long signed displacements through skipped pairs of Jump actions."""
    code = []
    for i, ((op, original), data) in enumerate(zip(actions, payloads)):
        if len(data) > LIMIT:
            fail(f"action 0x{op:02x} at instruction {i}, block {block_offset}: "
                 f"original={len(original)}, required={len(data)}, limit={LIMIT}")
        header = bytes([op]) + (struct.pack("<H", len(data)) if op >= 128 else b"")
        code.append(header + data)
    jumps = dict(targets)
    islands = defaultdict(list)
    routes = {}
    count = len(code)
    while True:
        starts = []
        positions = {}
        physical = []
        relay_indices = []
        offset = 0
        for i in range(count + 1):
            starts.append(offset)
            for relay in islands.get(i, []):
                relay_indices.append(len(physical))
                physical.append((None, b"\x99\x02\x00\x05\x00"))
                positions[relay] = offset + 5
                physical.append((relay, b"\x99\x02\x00\x00\x00"))
                offset += 10
            positions[i] = offset
            if i < count:
                physical.append((i, code[i]))
                offset += len(code[i])
        def address(label):
            return label[1] if isinstance(label, tuple) else positions[label]

        unresolved = []
        for label, target in jumps.items():
            origin = positions[label] + 5
            distance = address(target) - origin
            if not -32768 <= distance <= 32767:
                unresolved.append((label, target, origin, distance))
        if not unresolved:
            result = bytearray()
            for label, record in physical:
                if label in jumps:
                    delta = address(jumps[label]) - positions[label] - 5
                    record = record[:3] + struct.pack("<h", delta)
                result.extend(record)
            return bytes(result), relay_indices
        if any(op in (0x8a, 0x8d) for op, _ in actions):
            fail("AVM1 frame-skip actions require instruction-count relocation")
        previous = jumps.copy()
        for label, target, origin, distance in unresolved:
            if distance < 0:
                cut = max(1, bisect.bisect_left(starts, origin - 24000))
            else:
                cut = min(count, bisect.bisect_right(starts, origin + 24000) - 1)
            if cut <= 0 or cut > count or not min(origin, address(target)) < starts[cut] < max(origin, address(target)):
                fail(f"AVM1 block {block_offset}: no relay boundary for displacement {distance}")
            key = (cut, target)
            if key not in routes:
                relay = -len(routes) - 1
                routes[key] = relay
                islands[cut].append(relay)
                jumps[relay] = target
            jumps[label] = routes[key]
        if jumps == previous or len(routes) > 65535:
            fail(f"AVM1 block {block_offset}: branch relay layout did not converge")



def tag_bytes(code, payload, long_header=False):
    length = len(payload)
    if long_header or length >= 63:
        return struct.pack("<HI", code << 6 | 63, length) + payload
    return struct.pack("<H", code << 6 | length) + payload


class Movie:
    def __init__(self, source):
        self.source = source
        if len(source) < 9 or source[:3] not in (b"FWS", b"CWS") or source[3] < 8:
            fail("expected a UTF-8 FWS/CWS version 8+ movie")
        size = struct.unpack_from("<I", source, 4)[0] - 8
        if not 0 < size <= MAX_MOVIE:
            fail("SWF declared length outside supported range")
        if source[:3] == b"CWS":
            decoder = zlib.decompressobj()
            self.body = decoder.decompress(source[8:], size + 1)
            if not decoder.eof or decoder.unused_data:
                fail("incomplete CWS stream or trailing compressed data")
        else:
            self.body = source[8:]
        if len(self.body) != size:
            fail("SWF declared and decoded lengths differ")
        body = self.body
        p = (5 + 4 * (body[0] >> 3) + 7) // 8 + 4
        self.header = body[:p]
        self.tags = []
        self.scripts = {}
        self.records = defaultdict(list)
        self.meta = None
        self.attributes = None
        self.mapping = {}
        while p + 2 <= size:
            word = u16(body, p)
            code, length = word >> 6, word & 63
            long_header = length == 63
            p += 2
            if long_header:
                length = struct.unpack_from("<I", body, p)[0]
                p += 4
            end = p + length
            if end > size:
                fail(f"SWF tag {code} exceeds movie at {p}")
            payload = body[p:end]
            index = len(self.tags)
            self.tags.append((code, p, payload, long_header))
            if code == 69:
                if len(payload) != 4:
                    fail("invalid FileAttributes")
                self.attributes = index
            if code == 77:
                if self.meta is not None or not payload.endswith(b"\0"):
                    fail("invalid SWF Metadata")
                self.meta = index
                marker = META.search(payload)
                if marker:
                    self.mapping = json.loads(base64.b64decode(marker[1], validate=True))
                    if not isinstance(self.mapping, dict):
                        fail("invalid AVM1 locator map")
            p = end
            if code == 0:
                break
        if not self.tags or self.tags[-1][0] != 0 or p != size:
            fail("invalid SWF End tag")
        if self.attributes is not None and self.tags[self.attributes][2][0] & 8:
            return  # ActionScript 3
        ordinal = 0
        for index, (code, offset, payload, _) in enumerate(self.tags):
            if code in (12, 59):
                records = [(None, 2 if code == 59 else 0, len(payload))]
            elif code == 26:
                records = clip_records(payload)
            else:
                continue
            for size_pos, start, end in records:
                data = payload[start:end]
                if data[:1] != b"\x88":
                    continue
                slots = pool_strings(data)
                mapping = self.mapping.get(str(ordinal), {})
                if (not isinstance(mapping, dict) or not isinstance(mapping.get("fields", {}), dict)
                        or not isinstance(mapping.get("relays", []), list)):
                    fail("invalid AVM1 pool locator metadata")
                if mapping or any(classify(v.decode("utf8")) for v in slots):
                    self.scripts[ordinal] = PoolScript(data, offset + start, slots, mapping)
                    self.records[index].append((size_pos, start, end, ordinal))
                ordinal += 1
        if self.mapping.keys() - {str(i) for i in self.scripts}:
            fail("SWF Metadata references a missing AVM1 pool")

    def fields(self):
        for i, script in self.scripts.items():
            for slot, (text, kind) in script.fields.items():
                yield f"pool:{i}:{slot}", text, kind

    def build(self, translations):
        edits = defaultdict(dict)
        expected = {}
        for key, source, kind in self.fields():
            value = translations.get(key, source)
            expected[key] = (value, kind)
            if value != source:
                _, i, slot = key.split(":")
                edits[int(i)][int(slot)] = value
        if translations.keys() - expected.keys():
            fail("translation refers to a missing AVM1 slot")
        if not edits:
            return self.source
        mapping = self.mapping.copy()
        replacements = {}
        for index, records in self.records.items():
            payload = self.tags[index][2]
            parts = []
            cursor = 0
            for size_pos, start, end, i in records:
                if i not in edits:
                    continue
                new, locators = self.scripts[i].build(edits[i])
                if locators:
                    mapping[str(i)] = locators
                else:
                    mapping.pop(str(i), None)
                if size_pos is not None:
                    parts += [payload[cursor:size_pos], struct.pack("<I", len(new) + start - size_pos - 4)]
                    cursor = size_pos + 4
                parts += [payload[cursor:start], new]
                cursor = end
            if parts:
                replacements[index] = b"".join(parts) + payload[cursor:]

        # An XML processing instruction in the standard Metadata tag records only
        # changed locator/kind information. It has no ActionScript runtime effect.
        extra = b""
        if mapping != self.mapping:
            marker = b""
            if mapping:
                encoded = json.dumps(mapping, separators=(",", ":"), sort_keys=True).encode("ascii")
                marker = b"<?gamel10n-avm1-v1 " + base64.b64encode(encoded) + b"?>"
            if self.meta is not None:
                old = self.tags[self.meta][2]
                replacements[self.meta] = META.sub(b"", old[:-1]) + marker + b"\0"
            else:
                if self.attributes is None:
                    fail("AVM1 locator metadata requires SWF FileAttributes")
                attr = self.tags[self.attributes][2]
                replacements[self.attributes] = bytes([attr[0] | 16]) + attr[1:]
                extra = tag_bytes(77, b'<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"/>' + marker + b"\0")
        parts = [self.header]
        for index, (code, _, payload, long_header) in enumerate(self.tags):
            parts.append(tag_bytes(code, replacements.get(index, payload), long_header))
            if extra and index == self.attributes:
                parts.append(extra)
        body = b"".join(parts)
        if len(body) > MAX_MOVIE:
            fail("rebuilt SWF exceeds movie limit")
        return (self.source[:4] + struct.pack("<I", len(body) + 8)
                + (zlib.compress(body) if self.source[:3] == b"CWS" else body))

    def validate(self, patched, translations):
        expected = {key: (translations.get(key, value), kind) for key, value, kind in self.fields()}
        if {key: (value, kind) for key, value, kind in patched.fields()} != expected:
            fail("AVM1 re-extraction differs from expected text or identity")
        if self.scripts.keys() != patched.scripts.keys():
            fail("AVM1 script set changed")
        for i, script in self.scripts.items():
            if script.raw_targets and script.actions[0] != patched.scripts[i].actions[0]:
                fail(f"AVM1 pool {i}: raw branch destination bytes changed")
            if list(script.signature()) != list(patched.scripts[i].signature()):
                fail(f"AVM1 pool {i}: operand or branch target changed")
