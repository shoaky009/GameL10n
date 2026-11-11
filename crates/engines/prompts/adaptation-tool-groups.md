All tool definitions are supplied in a fixed catalog order throughout adaptation. Call the required tool directly; load_tool_groups is not a prerequisite and does not change the API schemas.
Capability directory (group: purpose; tool names):
reuse: Local engine knowledge, published source, bundled parsers, reusable text/font components and adapter templates; inspect_engine_knowledge, copy_engine_knowledge_tools, inspect_existing_text, inspect_builtin_adapter, inspect_adapter_source, try_builtin_adapter, copy_adapter, inspect_engine_component, run_engine_component, compose_adapter_template, inspect_adapter_component, copy_adapter_components
development: Adapter protocol, source edits, local probes, font corpus diagnostics, verification and publication; adapter_protocol, write_file, replace_text, remove_file, run_tool, run_engine_python, inspect_font_coverage, test_adapter, update_release_files, finish
research: Shared engine guides, Python adapter bridge, repository search, web pages and extra downloads; search_engine_references, inspect_engine_reference, use_engine_reference, search_repositories, fetch_url, download_tool
native: Launch/file diagnostics and PE imports/disassembly; inspect_game_launch, find_game_file, inspect_pe_imports, inspect_pe_code
unity: Unity managed/IL2CPP types, fields, literals and native reference candidates; inspect_unity_schema
siglus: Siglus scene commands, display rules and bundled parser sources; inspect_siglus_game
Groups classify local capabilities. Changing a classification, saving a checkpoint, resuming or compacting history does not remove or reorder tool definitions. Applicability still depends on actual format and runtime evidence.
