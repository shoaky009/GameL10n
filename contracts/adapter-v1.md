# Adapter CLI protocol v1

One UTF-8 JSON request is read from standard input; one UTF-8 JSON response is written to standard output. Diagnostics belong on standard error. Both streams and execution time are bounded by the host. The executable has no network access or credentials.

Request fields: `protocolVersion` (1), `operation` (`probe`, `extract`, `build`, `validate`), `sourceDirectory`, optional `outputDirectory`, optional `translations` (object mapping unit ID to translated text).

Response fields: `protocolVersion` (1), `success` (boolean), optional `error` (string), and the operation's result under `probe`, `extraction`, `build`, or `validation`.

All JSON property names use camelCase. Records correspond to the documented DTOs in `src/AvgTranslator.Core/Contracts.cs`; generated executables define their own equivalent DTOs and have no dependency on the host assembly. A `probe` result may be null for unsupported data. Every output path is relative to outputDirectory and must stay within it. `requiredFiles` are relative to sourceDirectory.

An extract response's `extraction` contains `units` and `warnings`. Every unit has `id`, `resourcePath`, `locator`, `sourceText`, `context`, optional `speaker`, and `kind`. IDs must be unique and repeatable. Only narrative/choice/name strings are extracted; program instructions, resource paths, and conditions are not translatable.

A build response's `build` contains `files` (relative paths), `warnings`, and optional `launchExecutable`. Building with a dictionary containing all original texts must reproduce original resources byte for byte. Unchanged non-text bytes must be preserved. A validate response's `validation` contains `passed`, `issues` (each code, message, optional unitId), and `runtimeStatus` (`not-tested` unless an actual runtime test occurred).

The host performs independent path, hash, non-empty extraction, unique identity, roundtrip, and changed-text re-extraction checks before publishing an extension. An extension cannot publish itself or claim a successful game runtime test.

Package identity is separate from this CLI protocol. Published packages declare a family `{kind,key,name}`: kind is `engine` or `template`; key names the engine/format or a concrete structural template, never a game. The host assigns `adapterId` as `<kind>.<key>` and pins the source/resource-scope fingerprint as its version. Model proposals contain exactly `reuseAdapterId`, `sourceCode`, `summary`, and `family` (null for reuse). A source proposal must supply a family; missing classification cannot be published.

Game titles, source hashes, resource-specific limitations and verification results belong to compatibility records under the family. Recognition of one game does not establish compatibility with other games using that engine. Reusing a generated template for a different game requires independent roundtrip verification before pinning that version. The final patch consists of game-consumable files; it has no dependency on the adapter CLI or the translator application at game runtime.
