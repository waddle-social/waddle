#!/usr/bin/env python3
"""Report Rust source files whose names flatten a module hierarchy into underscores.

Usage: check_flat_modules.py [ROOT ...] [--strict]

Scans `*/src` trees under each ROOT (default: crates) and reports:

- prefix: three or more sibling files share their first `_`-delimited segment,
  such as `execute_archive.rs`, `execute_offline.rs` and `execute_invite.rs`;
- satellite: `foo_bar.rs` sits beside `foo.rs` or a `foo/` directory;
- path: a module is wired with a `#[path = "..."]` attribute.

Snake_case alone is not a finding; single-concept names stay as they are.
The report is advisory unless --strict is given.
"""

import argparse
from collections import defaultdict
from pathlib import Path
import re

PATH_ATTR = re.compile(r'^\s*#\[path\s*=')
ROOT_FILES = {"lib.rs", "main.rs", "mod.rs"}
PREFIX_THRESHOLD = 3


def source_dirs(root):
    for crate in sorted(Path(root).iterdir()):
        src = crate / "src"
        if src.is_dir():
            yield src


def prefix_findings(directory, files):
    groups = defaultdict(list)
    for path in files:
        if "_" in path.stem:
            groups[path.stem.split("_", 1)[0]].append(path)
    return [
        ("prefix", directory / f"{prefix}_*.rs", len(members))
        for prefix, members in sorted(groups.items())
        if len(members) >= PREFIX_THRESHOLD
    ]


def satellite_parent(path, stems):
    parts = path.stem.split("_")
    for cut in range(1, len(parts)):
        parent = "_".join(parts[:cut])
        if parent in stems:
            return parent
    return None


def directory_findings(directory):
    files = sorted(p for p in directory.glob("*.rs") if p.name not in ROOT_FILES)
    stems = {p.stem for p in files} | {p.name for p in directory.iterdir() if p.is_dir()}
    found = prefix_findings(directory, files)
    for path in files:
        parent = satellite_parent(path, stems)
        if parent:
            found.append(("satellite", path, parent))
    return found


def path_attribute_findings(src):
    return [
        ("path", path, number)
        for path in sorted(src.rglob("*.rs"))
        for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1)
        if PATH_ATTR.match(line)
    ]


def findings(root):
    found = []
    for src in source_dirs(root):
        for directory in sorted({src, *(p for p in src.rglob("*") if p.is_dir())}):
            found.extend(directory_findings(directory))
        found.extend(path_attribute_findings(src))
    return found


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("roots", nargs="*", default=["crates"])
    parser.add_argument("--strict", action="store_true")
    args = parser.parse_args()
    found = [item for root in args.roots for item in findings(root)]
    for kind, path, detail in found:
        if kind == "prefix":
            print(f"prefix    {path} ({detail} files)")
        elif kind == "satellite":
            print(f"satellite {path} (beside {detail})")
        else:
            print(f"path      {path}:{detail}")
    counts = defaultdict(int)
    for kind, _, _ in found:
        counts[kind] += 1
    summary = ", ".join(f"{counts[k]} {k}" for k in ("prefix", "satellite", "path"))
    print(f"# {summary}")
    if args.strict and found:
        parser.exit(1)


if __name__ == "__main__":
    main()
