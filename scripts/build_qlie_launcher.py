#!/usr/bin/env python3
"""Build the standalone QLIE launcher and verify its parsing fixtures without launching a game."""

import argparse
import os
from pathlib import Path
import subprocess


def main() -> None:
    repo = Path(__file__).resolve().parent.parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dotnet", default="dotnet", help=".NET 10 SDK executable used only at build time")
    parser.add_argument("--output", type=Path, default=repo / "target/runtime/adapter-components")
    args = parser.parse_args()
    output = args.output.resolve()
    project = repo / "tools" / "GameL10n.QlieLauncher" / "GameL10n.QlieLauncher.csproj"
    subprocess.run(
        [args.dotnet, "publish", str(project), "-c", "Release", "-r", "win-x64", "--self-contained", "true", "-o", str(output)],
        cwd=repo,
        check=True,
    )
    launcher = output / "QlieLauncher.exe"
    subprocess.run(
        [str(launcher), "--self-test"],
        check=True,
        creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0,
    )
    print(f"{launcher} ({launcher.stat().st_size:,} bytes); fixture checks passed")


if __name__ == "__main__":
    main()
