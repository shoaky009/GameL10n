# SiglusSceneScriptUtility

- Source: https://github.com/Jirehlov/SiglusSceneScriptUtility
- Version: 0.5.7
- Upstream license: Unlicense OR 0BSD
- Distribution: `siglus_ssu-0.5.7-cp314-cp314-win_amd64.whl`
- SHA-256: `709752e8048400d326824729dddb167b21005cd3a7878c5827b370dfae2df23e`
- Download: https://files.pythonhosted.org/packages/ca/83/a4aa6717a051913c27e18ca64a89c5f1e68ddb3f5d751ff08dd457e31f3e/siglus_ssu-0.5.7-cp314-cp314-win_amd64.whl

The official wheel is embedded in the application. `adapters/siglus/upstream-patches.json` adds ZIP import support and exposes existing argument-stack string references; it does not replace the upstream parser. Host glue and byte-preserving writers reside in `adapters/siglus`. Published extensions pin the combined archive by SHA-256. The native compression module is extracted once beside that shared archive, never into a game directory.
