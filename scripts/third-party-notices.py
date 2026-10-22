#!/usr/bin/env python3
"""Write THIRD-PARTY-NOTICES.txt: every crate built into Xenon, with its licence and
notice files, plus DuckDB, the icons and the fonts. Run at build time:

    scripts/third-party-notices.py <output file>
"""

import json
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
NAMES = ("license", "licence", "copying", "notice", "unlicense")
# The libraries built into libduckdb.dylib, from DuckDB's source at the pinned version
# (scripts/verify-duckdb.sh). Bump them together.
DUCKDB_LICENSES = ROOT / "assets/licenses/duckdb-1.5.6"
# Standard texts for crates that ship no licence file of their own.
SPDX = ROOT / "assets/licenses/spdx"


def run(*args):
    return subprocess.run(args, cwd=ROOT, check=True, capture_output=True, text=True).stdout


def runtime_crates():
    """(name, version) of what the app links: normal dependencies, this platform."""
    tree = run(
        "cargo", "tree", "-p", "xenon", "-e", "normal", "--prefix", "none",
        "--format", "{p}", "--target", "aarch64-apple-darwin",
    )
    crates = set()
    for line in tree.splitlines():
        parts = line.replace(" (*)", "").split()
        if len(parts) >= 2 and parts[1].startswith("v"):
            crates.add((parts[0], parts[1][1:]))
    return crates


def notice_files(directory):
    return sorted(
        path for path in directory.iterdir()
        if path.is_file() and path.name.lower().startswith(NAMES)
    )


def main():
    out = pathlib.Path(sys.argv[1])
    metadata = json.loads(run("cargo", "metadata", "--format-version", "1"))
    wanted = runtime_crates()
    packages = sorted(
        (p for p in metadata["packages"]
         if (p["name"], p["version"]) in wanted and p.get("source")),
        key=lambda p: (p["name"], p["version"]),
    )
    parts = [
        "Xenon is free software under the GNU General Public License v3.0 (LICENSE, beside\n"
        "this file). It's built with the open-source work below; each keeps its own licence.\n",
    ]

    def section(title, text):
        parts.append(f"\n{'=' * 78}\n{title}\n{'=' * 78}\n\n{text.strip()}\n")

    duckdb = sorted(DUCKDB_LICENSES.iterdir())
    if not duckdb:
        sys.exit(f"no DuckDB licences in {DUCKDB_LICENSES}")
    section(
        "DuckDB (libduckdb.dylib), and the libraries built into it",
        "\n\n".join(f"--- {f.name.replace('__', '/')} ---\n{f.read_text(errors='replace')}" for f in duckdb),
    )
    for name, title in [("LICENSE-LUCIDE", "Lucide icons"), ("LICENSE-SIMPLE-ICONS", "Simple Icons database logos")]:
        section(title, (ROOT / "assets/icons" / name).read_text())
    for font in sorted((ROOT / "assets/fonts").glob("OFL-*.txt")):
        section(f"Font: {font.stem.removeprefix('OFL-')}", font.read_text())

    needed = set()
    for package in packages:
        directory = pathlib.Path(package["manifest_path"]).parent
        header = f"{package['name']} {package['version']} ({package.get('license') or 'see below'})"
        files = notice_files(directory)
        if files:
            text = "\n\n".join(f"--- {f.name} ---\n{f.read_text(errors='replace')}" for f in files)
        else:
            # No licence file in the package: the standard text, in the appendix.
            expression = package.get("license") or ""
            ids = set(re.findall(r"[A-Za-z0-9.+-]+", expression)) - {"OR", "AND", "WITH"}
            missing = [i for i in ids if not (SPDX / f"{i}.txt").exists()]
            if not ids or missing:
                sys.exit(f"{package['name']} {package['version']}: no licence file, and no "
                         f"standard text for {missing or expression!r} in {SPDX}")
            needed |= ids
            authors = ", ".join(package.get("authors") or [])
            text = (f"Licensed under {expression}: the package has no licence file, so the "
                    f"full text is in the appendix at the end."
                    + (f" Authors, as its manifest lists them: {authors}." if authors else ""))
        if package.get("repository"):
            text = f"{package['repository']}\n\n{text}"
        section(header, text)

    for spdx in sorted(needed):
        section(f"Appendix: {spdx}", (SPDX / f"{spdx}.txt").read_text())

    out.write_text("".join(parts))
    print(f"wrote {out} ({len(packages)} crates)")


if __name__ == "__main__":
    main()
