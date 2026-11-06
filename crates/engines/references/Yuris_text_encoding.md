# YU-RIS text encoding

The bundled parser defaults to strict CP932 for both reading and writing. Its
legacy `SjisTunnelEncoding` name alone does not imply runtime tunneling support.
Use the optional `yurisEncoding` request only after choosing a compatible runtime
plan. It applies to YU-RIS text fields, not all raw attributes or other engines.
Keep the original extraction IDs, grouping, context and field selection. Pass
the original unpacked scripts directly to the fixed parser; do not prepare a
CP932 shadow copy or rewrite attributes before extraction or build.

`yurisEncoding` accepts `source`, `output` (`cp932` or `sjis-tunnel`, each defaults
to `cp932`) and `mapping` (the ordered characters in `sjis_ext.bin`). Extract reads
`source`; build reads `source`, writes `output` and re-extracts using `output`;
validate uses the respective modes for the original and rebuilt scripts.
Native requests never allocate new slots or write the mapping. Invalid modes,
missing mappings, duplicate/non-BMP/NUL characters and unmapped text are rejected.
An empty mapping is valid when no extended characters are needed.

Extract once from the current source snapshot and retain the native units for
ID lookup and field selection. Request `includeYurisSources=True` when command
semantics are needed; reuse that metadata instead of decrypting the whole script
again for each unit. A container adapter maps its public IDs to these native IDs.
Reject unknown translation IDs before discarding entries equal to their source.

For an empty translation map, or when all supplied translations equal the source,
call native build directly with `source` and `output` both set to the input codec.
Skip new mapping planning, shadow conversion, and installation of new runtime
assets. The native build preserves unchanged resource bytes and verifies them;
the container layer copies the original archive when no member changed. This
does not bypass required verification or change the encoding of an unchanged file.

When text changes require tunneling, copy the `sjis-tunnel` component and plan
one mapping for the whole patch. Reuse an existing mapping without reordering
it. Include both retained and translated logical text; inspect the remaining
display paths for incompatible bytes.
If a returned unit contains NUL, stop mapping that field and inspect its
`yurisSource` plus the original archive member. A selected WORD field can still
contain an unknown binary/control prefix; successful CP932 extraction does not
prove it is plain display text. Do not strip that prefix, silently drop the unit,
or remap NUL to a character. Preserve the evidence and resolve the field semantics
before claiming full tunnel support for that resource.

```python
from tools.gamel10n_host import parser
from tools.sjis_tunnel import Tunnel


def build_scripts(request, source_directory, output_directory, script_paths,
                  native_units, translations, source_mode="cp932", mapping_bytes=b""):
    # native_units came from one extraction of this unchanged source snapshot.
    source_texts = {unit["id"]: unit["sourceText"] for unit in native_units}
    if translations.keys() - source_texts.keys():
        raise ValueError("Translation IDs do not match the source scripts")
    changed = {key: text for key, text in translations.items()
               if text != source_texts[key]}
    options = dict(source=source_mode, output=source_mode)
    if source_mode == "sjis-tunnel":
        options["mapping"] = mapping_bytes.decode("utf-16-le")
    new_mapping = None
    if changed:
        tunnel = Tunnel(mapping_bytes)
        for key, original in source_texts.items():
            tunnel.encode(changed.get(key, original))
        options.update(output="sjis-tunnel", mapping="".join(tunnel.chars))
        new_mapping = tunnel.serialize()
    response = parser(request, "Yuris", operation="build",
                      sourceDirectory=str(source_directory),
                      outputDirectory=str(output_directory),
                      resourceFiles=script_paths, translations=changed,
                      yurisEncoding=options)
    return response, new_mapping

# After build succeeds, write a non-None new_mapping as the patch's sjis_ext.bin.
# For unchanged builds, retain existing companions alongside the original archive.
```

For already tunneled input use `source="sjis-tunnel"` with its existing mapping.
Supply that same mapping when extracting the rebuilt patch. Do not introduce
temporary shadow characters or reinterpret non-text attributes to bypass CP932.
New encoding support requires the current native parser. For a new adapter
without an existing native dependency pin, `copy_adapter` registers the current
parser. Continuing an existing adapter
preserves its pin, including when copying or preparing dependencies again; do not
assume the new option is available there. If its parser lacks this interface,
preserve `src`, existing IDs and translations, and report the capability gap.
Do not recreate the adapter, reset its source or identities, or edit dependency
hashes to obtain a newer binary. Use a supported explicit dependency update and
repeat compatibility checks before adopting the interface; if no such update
is available, retain the pin.

`inspect_adapter_component(id="vntextproxy-runtime")` describes the pinned
official proxy, allowed DLLs, font loading and the local preparation helper.
The proxy and a matching `sjis_ext.bin` must be available to the actual game EXE.
Archive offsets, proxy loading, engine byte traversal, fonts and locale remain
separate checks. Resource round-trip success is not a runtime rendering result.
