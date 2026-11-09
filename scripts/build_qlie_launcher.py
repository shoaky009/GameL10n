#!/usr/bin/env python3
"""Build the standalone Rust QLIE launcher and verify fixtures without launching a game."""

import argparse
import os
from pathlib import Path
import shutil
import subprocess


def main() -> None:
    repo = Path(__file__).resolve().parent.parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cargo", default="cargo", help="Cargo executable used only at build time")
    parser.add_argument("--output", type=Path, default=repo / "target/runtime/adapter-components")
    args = parser.parse_args()
    output = args.output.resolve()
    manifest = repo / "tools" / "GameL10n.QlieLauncher" / "Cargo.toml"
    target = repo / "target" / "qlie-launcher" / "cargo"
    subprocess.run(
        [args.cargo, "rustc", "--manifest-path", str(manifest), "--locked", "--release",
         "--target", "x86_64-pc-windows-msvc", "--target-dir", str(target), "--bin", "qlie-launcher",
         "--", "-C", "target-feature=+crt-static"],
        cwd=repo,
        check=True,
    )
    output.mkdir(parents=True, exist_ok=True)
    launcher = output / "QlieLauncher.exe"
    shutil.copyfile(target / "x86_64-pc-windows-msvc/release/qlie-launcher.exe", launcher)
    subprocess.run(
        [str(launcher), "--self-test"],
        check=True,
        creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0,
        timeout=60,
    )
    print(f"{launcher} ({launcher.stat().st_size:,} bytes); fixture checks passed")


if __name__ == "__main__":
    main()
