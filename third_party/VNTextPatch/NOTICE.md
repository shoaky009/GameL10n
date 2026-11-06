# VNTextPatch parser sources

Vendored from [VNTextPatch-net8](https://github.com/rafael-vasconcellos/VNTextPatch-net8),
commit `e68e174f3db6181e03fc7fabfa7570e1b293d148` (MIT; see LICENSE).
Original project: [VNTranslationTools](https://github.com/arcusmaximus/VNTranslationTools).

The 29 registered game script parsers and their required helpers are built directly
into `tools/GameL10n.VnTextPatch`. No checkout under `target` is required.
Spreadsheet/cloud integrations, translation interchange formats, font measurement,
application settings, command-line orchestration and disabled Kirikiri SCN support
are omitted. No upstream NuGet dependency is required.

Local changes disable automatic line wrapping and implicit Shift-JIS tunneling,
use strict encoders, preserve untouched plain-text ranges and Kirikiri comments,
and limit Ren'Py extraction to UTF-8 single-line say/menu statements outside Python
and interface blocks. AGE disassembly is not written into source folders; address
patch failures propagate. Softpal preserves earlier writes to shared text pools.
Artemis TXT flushes messages before commands and at EOF. Hiro text lengths are
validated before reading or writing, and SystemNNN does not alter quote styles.
YU-RIS preserves attribute-free scripts and unchanged attribute groups; decoding
control codes does not mutate the source buffer. Optional extraction metadata
reports command/attribute origins without changing default text identities.
Named-message edits retain the original grouping, including parenthesized thoughts.
The fixed worker accepts explicit YU-RIS-only input/output codecs: strict CP932
by default, or VNTextProxy-compatible SJIS tunneling with a caller-supplied ordered
BMP mapping. Only the existing text fields use that request's codec; control and
other non-text fields retain their original interpretation. The mapping is fixed,
never extended or reordered by the native worker, and is not global parser state.
The XOR helper uses bounded spans instead of unsafe pointers, allowing selected
parser sources to compile with the generated adapter compiler's `/unsafe-` policy.
The worker copies original files for unchanged builds and
re-extracts every changed build to check text and structural consistency. These
checks do not establish compatibility with a game's runtime, font, archive loader
or native locale. Callers selecting tunneling must provide `sjis_ext.bin`, a
compatible runtime hook and font support separately; this worker emits none of them.
