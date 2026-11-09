# Third-party notices

GameL10n's main application is licensed under GPL-3.0-only; see LICENSE.
Independently licensed components retain their own terms, including the MIT-licensed
FontBridge component in `tools/GameL10n.FontBridge`.
Third-party code, tools and reference materials retain their own licenses and
copyright notices. The project license does not replace those terms.

AOS/SCR format implementation references:

- AosTools: https://github.com/Lasriel/AosTools
- GARbro: https://github.com/morkt/GARbro

Applicable MIT notices follow. The Rust desktop application does not distribute Windows App SDK or a shared .NET runtime. The fixed VNTextPatch tool retains its source notices and the applicable NativeAOT/.NET component licenses.

## Slint and Rust dependencies

The desktop layer uses Slint 1.18.1 under the GPL-3.0-only option of its upstream
alternative license terms. Slint also offers LicenseRef-Slint-Royalty-free-2.0
and LicenseRef-Slint-Software-3.0; those alternatives are not selected here.
See https://slint.dev/license. Rust dependencies retain their respective licenses;
the dependency versions are recorded in Cargo.lock and each standalone tool's Cargo.lock.

## Engine research toolkit

The intelligent adaptation tools can query documentation and source links from
https://github.com/GalTransl/GalTransl-GalGameReverse-Skill.
The complete engine-tool integration uses a commit-pinned source archive containing Python
implementations, catalog, engine guides, LICENSE and provenance notices in
`third_party/engine-tools`. The application package includes these pinned libraries under
`tools/engine-tools`. Previously installed versions may remain in the data
directory's shared `extensions/engine-tools` library. These materials retain
their upstream licenses, including GPLv3 and file-specific third-party notices.
Adapters referring to this library must meet the corresponding source and distribution
obligations. UIF binaries, fonts and hook DLLs are not installed by this integration.

The portable Windows x64 Python 3.14.10 interpreter is bundled with standard releases.
Packaging obtains its original distribution from
https://www.python.org/ftp/python/3.14.10/python-3.14.10-embed-amd64.zip.
Its original LICENSE.txt and distribution files remain in the shared runtime directory.
Versions and content hashes are recorded with the adapter dependency; the interpreter
is used by the translator, not installed in the game directory or required to play patches.

## VNTextPatch-net8 / VNTranslationTools

The built-in script parsers include adapted source from
https://github.com/rafael-vasconcellos/VNTextPatch-net8,
commit `e68e174f3db6181e03fc7fabfa7570e1b293d148`, originally developed in
https://github.com/arcusmaximus/VNTranslationTools.
Source and local modification notes are in `third_party/VNTextPatch`.

MIT License

Copyright (c) 2021 arcusmaximus

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.

## AosTools

MIT License

Copyright (c) 2025 Lasriel

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.


## GARbro

The MIT License (MIT)

Copyright (c) 2014-2020 morkt

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.

## Capstone

PE code inspection bundles the unmodified Capstone 5.0.6 Windows x64 native library from its official PyPI wheel.
Source: https://github.com/capstone-engine/capstone
Distribution: https://pypi.org/project/capstone/5.0.6/
License: BSD 3-Clause; the complete license is distributed at `tools/analysis/Capstone-LICENSE.txt`.
Pinned artifact hashes and repository license: `third_party/Capstone/README.md` and `third_party/Capstone/LICENSE.TXT`.

## SiglusSceneScriptUtility

Siglus support bundles the official Windows x64 wheel of SiglusSceneScriptUtility 0.5.7.
Source: https://github.com/Jirehlov/SiglusSceneScriptUtility
License: Unlicense OR 0BSD (the distribution below uses the 0BSD grant).
Local bridge modifications add ZIP import support and expose string references from the existing argument stack.

BSD Zero Clause License

Copyright (C) 2026 by SiglusSceneScriptUtility contributors

Permission to use, copy, modify, and/or distribute this software for any purpose
with or without fee is hereby granted.

THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES WITH
REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF MERCHANTABILITY AND
FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT,
INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES WHATSOEVER RESULTING FROM LOSS
OF USE, DATA OR PROFITS, WHETHER IN AN ACTION OF CONTRACT, NEGLIGENCE OR OTHER
TORTIOUS ACTION, ARISING OUT OF OR IN CONNECTION WITH THE USE OR PERFORMANCE OF
THIS SOFTWARE.
