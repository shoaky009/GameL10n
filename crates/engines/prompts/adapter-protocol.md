Python CLI protocol v1, with entry src/adapter.py and no dependency on the translator assembly.
Required request keys: protocolVersion, operation, sourceDirectory. Required response keys: protocolVersion, success.
A non-null probe MUST contain engineId, adapterId, adapterVersion, description and requiredFiles. Do not omit identity
fields or synthesize successful evidence: every CLI operation and test_adapter must actually run against current resources.
All generated analysis and adapter logic uses Python. No C# compilation, csproj, NuGet restore or build tasks.
test_adapter reuses the persisted release selection; optional releaseFiles replaces it. update_release_files changes
only selected entries. New src/*.py and copied built-ins auto-register; other dependencies declare source/runtime/asset/license
roles and short purposes once through write_file/download_tool or update_release_files. tools/* maps to workspace src/tools/*. Include required indirect dependencies/notices, not probes,
samples, reports or unused tools. Only this selected package is syntax-checked, tested and published (manifest v4).
Selected tools are packaged as tools/ beside src/. Locate them relative to request.adapterDirectory;
use subprocess argument arrays, and keep any attribution/source metadata in tools/sources.json.
Read one UTF-8 JSON request from stdin. Write diagnostics to stderr. All JSON properties use camelCase.
User-facing warnings, errors and issue messages must be concise, actionable Chinese; keep detailed research in development notes.
Request: {protocolVersion:1,operation:"probe"|"extract"|"build"|"validate",sourceDirectory,outputDirectory,
  resourceFiles:[relativePath],buildHints,buildMode,outputFiles:[relativePath],probeFiles:[{path,length,headerBytes}],probeRanges:[{path,offset,length,dataPath}],
  probeReadLimits,responseFile,translationsFile,translations,validationContextFile,toolPaths,adapterDirectory}.
tools/engine-reference.json pins the shared Python source library and interpreter; test_adapter prepares it automatically.
use_engine_reference also supplies tools/gamel10n_host.py. toolPaths:{python,engineLibrary,vnTextPatch,hostCommand}
contains host-resolved local paths and the precompiled component command array.
Python can call existing python.engines/python.archives modules without rewriting their algorithms.
from tools.gamel10n_host import parser, component accesses pinned parsers and precompiled font/encoding components.
Implement bounded probe in Python; full-file parsers must never run against truncated probe input.
The same pinned files are used for test_adapter and all published operations; use bundled/shared dependencies, restoring missing files from a complete release.
Never hardcode these paths, download libraries from adapter code, or copy shared runtimes into game output.
Always enumerate the current resourceFiles or sourceDirectory; filenames from the development game must not be pinned.
Resource discovery patterns accept *, ? and ** with / separators (e.g. **/*.dat), and form a union, not required-file conditions.
Optional test_adapter recognition is a separate schemaVersion:1 declaration of required and hints file rules.
Each rule selects ANY of patterns; size constraints and ALL signatures hold on one file. All required rules must match;
hints only rank candidates. Signatures use hex bytes and offset>=0: origin start (default) counts to signature start;
origin end counts backwards from EOF to signature end. Unknown evidence retains a candidate for CLI probe.
Omit recognition to keep saved conditions; do not add game-title filenames or research extra facts merely to fill it.
The host owns indexing, ranking and I/O budgets; declaration changes version the package without changing CLI v1.
During probe, sourceDirectory contains only up to 65536 header bytes per candidate. probeFiles reports original lengths
and actual headerBytes. To inspect a footer, index or PE resource outside that header, return
{protocolVersion:1,success:true,probeReads:[{path,offset,length}]} without a probe result. The host invokes probe again
locally with cumulative probeRanges; read each dataPath from offset 0 for the specified original-file range.
Only current candidate paths are allowed, offsets are absolute and nonnegative. Limits: 4 additional rounds,
64 ranges total, 1 MiB per range and 8 MiB total. Batch independent ranges; never request an identical range twice.
Prefer footer -> index/PE resource -> selected script/font/EXE resources; do not stage full media just to read an index.
Ranges are temporary read-only input, not requiredFiles. Return the final probe (or probe:null) once selection is known.
Do not infer unsupported formats from missing bytes or classify archive contents by filename/size alone.
Full extract/build/validate operations receive host source snapshots. Read sources only; do not modify them.
For build, load the unit-ID-to-text dictionary from translationsFile when non-null, else from inline translations.
Write the full response JSON to the absolute responseFile path when supplied (open with encoding='utf-8' and json.dump
supports large extractions). An inline stdout JSON response is also supported for legacy adapters.
Each CLI operation is cancellable. Probe has a 45-second total budget; other operations have a ten-minute timeout. stdout/stderr diagnostics are limited to 8 MiB for new
packages; extraction responses written to responseFile have no fixed byte limit. Do not print extraction data to stdout.
Response: {protocolVersion:1,success:true,error:null,probe|extraction|build|validation:...}; failures use success:false,error.
An extraction may additionally return errorKind:"incompatible" only when its resource format/filter is unsupported.
The host then records the evidence and tries other adapters. Missing dependencies, I/O errors, implementation errors
and unknown failures must omit this classification and stop the operation; do not label every caught exception incompatible.
Probe: {engineId,adapterId,adapterVersion,description,requiredFiles:[relativePath],warnings:[],originalExecutable:null,buildHints:{}}.
Optional buildHints stores small per-game build choices derived from candidate names, e.g. an unused overlay volume path.
The host persists and forwards it to extract/build/validate, including verification. Compute it in bounded probe from the candidate
filenames; do not stage/hash unrelated media to choose an output name. Real-game hints take precedence over the staged subset's hints.
Reuse the saved name on subsequent builds, even after that patch is installed; do not hardcode game names or increment on every build.
requiredFiles must be a nonempty subset of candidates and include every file needed by full operations. If EXE changes
are needed, declare originalExecutable and include it in requiredFiles so its original bytes are preserved as source.
Discover and include any launch configuration needed by full operations, such as the original EXE's .le.config.
Extraction: {units:[{id,resourcePath,locator,sourceText,context,speaker:null,kind:"dialogue"}],warnings:[]}.
Optional unit.protectedTokens:[string] declares exact engine-specific substitution tokens present in sourceText
(at most 64 nonempty tokens, each <=256 characters). The host masks all occurrences for translation and checks their counts.
Only declare runtime syntax; visible prose must remain translatable. Preserve declarations during build/re-extraction.
IDs must be unique and stable across changed text lengths; retain resourcePath, locator, kind and speaker.
Empty or whitespace-only display slots and spacing literals stay unchanged in the script; omit them from translation units.
Keep position-based IDs stable when omitting them. The host also excludes returned whitespace-only units after validating
every original record, including duplicate IDs and resource paths. Repeated visible text at different positions is valid.
Extract player-visible dialogue/narration, names, choices, UI/menu labels, item/equipment names and descriptions,
skills/traits, tutorials and system messages when their storage and usage are established. Never extract internal IDs,
bytecode, resource paths, conditions, developer diagnostics or image/video lettering. No hard count/size limit on units.
Suggested kinds: dialogue, name, choice, ui, item, skill, system. Existing units retain their IDs, context and kind when coverage expands.
For Unity, check relevant TextAssets, serialized MonoBehaviour/ScriptableObject fields, scene/UI components and runtime literals;
Dialogue_* is only one possible source. Internal object names and IL2CPP metadata strings are not automatically display text.
With stripped type trees, establish field layout and consumers before rewriting. Add scenes/metadata to discovery and requiredFiles
only when the implemented path needs them. Report observed unhandled categories in warnings; resource tests do not prove full coverage.
Build: {files:[relativePath],mode:"replace"|"overlay",warnings:[],launchExecutable:null}. Missing mode means replace.
Write files only inside outputDirectory and declare all outputs. New paths including a launch EXE are permitted.
In replace mode, original-text build must reproduce every source resource byte for byte under its original path;
translated builds must also preserve every source resource path.
Declare generated companions (e.g. encoding maps) in build.files even when absent from original inputs.
Replace-mode verification re-extraction receives validated build.files as resourceFiles, with sourceDirectory set to the build output.
Read companions from that directory and preserve original text resource paths/IDs. Original-input companions still belong in requiredFiles.
Overlay mode is opt-in for Python adapters with established runtime mounting/override rules. Output only NEW paths with changed
members and necessary companions, never copies of unchanged originals. It cannot replace a path present in sourceDirectory;
use replace when EXE/config/original-file replacement is needed. A no-change original build should return mode:overlay,files:[].
Overlay re-extraction receives original sourceDirectory/resourceFiles, patch outputDirectory, buildMode:"overlay" and outputFiles
from build.files. Read the effective ORIGINAL + PATCH namespace using the proven engine precedence, not just the patch's members.
Preserve original public IDs, resourcePath, locator, context, kind, speaker and protectedTokens; never switch identity to the new volume.
Validation receives the same buildMode/outputFiles. Independently check member paths, winning payloads and untouched data; do not
trust the mode or a passed flag. Source and patch are read-only during extract/validate. No physical merged-directory copy is needed.
The existing original/partial/full verification stages compare effective texts, unchanged units and source hashes. A nonempty
original overlay must read back unchanged text and pass validation; an empty original overlay must leave source bytes untouched.
Overlay correctness does not establish game load order or rendering at runtime. Without sufficient engine evidence use replace;
use existing evidence and reusable writers/loading components, without extra research calls, hook development, candidate trials or
verification stages solely for smaller output. Retain this choice on continuation unless new evidence or the task requires changing it.
Keep the original executable name and launch entry by default. EXE changes need an identified runtime requirement;
output necessary changes under the original relative path in replace mode. The host handles installation backups;
do not modify sources, create adapter-managed backups or emit a renamed game EXE merely to preserve the original.
Only when launch-time setup requires a launcher, output a standalone EXE, explain the need, include it in files
and set launchExecutable to that relative path.
New EXE/CMD/BAT/PowerShell outputs require an explicit launchExecutable (original EXE or standalone launcher EXE);
the host rejects ambiguous entry points. Per-EXE .le.config files must be discovered and included alongside selected EXEs,
preserved byte for byte during translated builds. Host checks these conditions before publication.
launchExecutable:null retains the original launch entry; it must not hide a required font or locale launcher.
Preserve the original locale launch route/profile and working directory. Existing *.exe.le.config is strong evidence of
a Locale Emulator dependency; directly starting the game EXE can bypass it. Script encoding, cmd chcp and managed culture
do not establish the native process ACP/locale needed for ANSI filenames, movie paths, window titles and UI.
Launcher font registrations must remain available until the actual game exits, including across intermediary launchers.
Every new or changed EXE gets independent host PE checks. Preserve ASLR and valid relocations; static checks do not prove runtime.
Validate success: {passed:true,issues:[],runtimeStatus:"not-tested"}.
Validate failure: {passed:false,issues:[{code,message,unitId:null}],runtimeStatus:"not-tested"}.
Every issue is a blocking validation error. passed:true with nonempty issues is invalid. Put nonblocking coverage,
font differences and untested-runtime notices in probe/extraction/build.warnings, never in validation.issues.
An outer Python adapter can opt into extraction reuse by returning top-level validationContextVersion:1 with extract.
During this full verification only, validate may receive an absolute validationContextFile. Its JSON contains
{schemaVersion:1,sourceDirectory,outputDirectory,resourceFiles,outputFiles,originalUnits,outputUnits}; these are the
host's actual validated extraction results for the current source and build output, with the adapter's public IDs.
from tools.gamel10n_host import validation_context; context=validation_context(request) returns this data plus
translations loaded from the current translationsFile/inline request, or None when no context is supplied.
Reuse these units for generic count/text checks instead of extracting both resources again. Keep independent format,
archive-member, untouched-field, encoding and glyph checks. The helper rejects incompatible versions/resource bindings;
its parser/component bridge strips outer context and inner capability declarations when calling nested tools.
Do not advertise this capability until validate consumes it; retain full extraction fallback when context is absent.
Existing published adapters are unchanged. The temporary file expires after validate and is not a persistent cache.
The host checks source integrity, original-byte roundtrip, distinct Unicode changes, stable identities, declared outputs
and PE structure before publication. A self-reported passed flag alone is insufficient; runtime remains not-tested.
Changed-text re-extraction can pass when encoder and decoder share an incorrect mapping. Verify translated bytes and
expected glyph bitmaps independently; before PUA remapping, check the shipped EXE's string traversal and glyph lookup ranges.
Companion text indexes must match the output's actual string references or relocated command targets. Re-read those
records and check index offsets against them; finding expected text in an unreferenced appended area is not a valid rebuild.
Preserve the original font style by default. For GDI, verify the selected face and required glyphs; do not silently accept substitution.
Check the engine's own font lists, charset/pitch filters and full-name matching too. A font available to a direct GDI probe
may be rejected by the engine (e.g. QLIE system_fix with proportional fonts, or elfFullName's Regular suffix omitted in config).
Locale Emulator may replace enumerated full names from localized font name-table ID 4. Keep this metadata consistent
with configuration and native enumeration; host-only font probes cannot establish selection through the game's locale route.
Compare replacement font size, baseline and advances using the engine's actual height/width requests, including positive cell heights.
Independent glyph probes do not establish game startup, ANSI resource/movie path handling or in-game rendering.
Package identity is an engine or structural template {kind:"engine"|"template",key,name}, never a game title.
Recognition of this game does not establish compatibility for other games; those receive independent verification.

Minimal complete JSON examples (paths and template identities are illustrative; discover current files):

probe request:
```json
{"protocolVersion":1,"operation":"probe","sourceDirectory":"C:/host/source","resourceFiles":["scripts.dat"],"probeFiles":[{"path":"scripts.dat","length":131072,"headerBytes":65536}]}
```
probe response:
```json
{"protocolVersion":1,"success":true,"probe":{"engineId":"example-format","adapterId":"template.example-format","adapterVersion":"1","description":"已识别示例脚本格式。","requiredFiles":["scripts.dat"],"warnings":[],"originalExecutable":null}}
```
Unsupported data returns null; a supported probe must select real required resources:
```json
{"protocolVersion":1,"success":true,"probe":null}
```
Extra probe range response, without a simultaneous probe result:
```json
{"protocolVersion":1,"success":true,"probeReads":[{"path":"scripts.dat","offset":131056,"length":16}]}
```
The next probe request retains the same headers and supplies cumulative ranges:
```json
{"protocolVersion":1,"operation":"probe","sourceDirectory":"C:/host/source","resourceFiles":["scripts.dat"],"probeFiles":[{"path":"scripts.dat","length":131072,"headerBytes":65536}],"probeRanges":[{"path":"scripts.dat","offset":131056,"length":16,"dataPath":"C:/host/ranges/range-0.bin"}]}
```
extract request and response:
```json
{"protocolVersion":1,"operation":"extract","sourceDirectory":"C:/host/source","resourceFiles":["scripts.dat"]}
```
```json
{"protocolVersion":1,"success":true,"extraction":{"units":[{"id":"scripts.dat:record:0","resourcePath":"scripts.dat","locator":"record:0","sourceText":"原文","context":"旁白","speaker":null,"kind":"dialogue"}],"warnings":[]}}
```
build request and response (missing translation IDs retain original text):
```json
{"protocolVersion":1,"operation":"build","sourceDirectory":"C:/host/source","outputDirectory":"C:/host/output","resourceFiles":["scripts.dat"],"translations":{"scripts.dat:record:0":"译文"}}
```
```json
{"protocolVersion":1,"success":true,"build":{"files":["scripts.dat"],"warnings":[],"launchExecutable":null}}
```
Overlay build response and effective re-extraction request (only after confirming runtime override rules):
```json
{"protocolVersion":1,"success":true,"build":{"mode":"overlay","files":["scripts.patch.dat"],"warnings":[],"launchExecutable":null}}
```
```json
{"protocolVersion":1,"operation":"extract","sourceDirectory":"C:/host/source","resourceFiles":["scripts.dat"],"outputDirectory":"C:/host/output","buildMode":"overlay","outputFiles":["scripts.patch.dat"],"buildHints":{"patchPath":"scripts.patch.dat"}}
```
validate request and response (inspect the supplied output, without modifying it):
```json
{"protocolVersion":1,"operation":"validate","sourceDirectory":"C:/host/source","outputDirectory":"C:/host/output","resourceFiles":["scripts.dat"],"translations":{"scripts.dat:record:0":"译文"}}
```
```json
{"protocolVersion":1,"success":true,"validation":{"passed":true,"issues":[],"runtimeStatus":"not-tested"}}
```
A failed resource validation is a successful CLI response with blocking issues:
```json
{"protocolVersion":1,"success":true,"validation":{"passed":false,"issues":[{"code":"text-mismatch","message":"回读文本不一致，请修复脚本写回。","unitId":"scripts.dat:record:0"}],"runtimeStatus":"not-tested"}}
```
A failed CLI operation reports its actual error:
```json
{"protocolVersion":1,"success":false,"error":"脚本索引超出文件范围，请检查格式版本。"}
```
