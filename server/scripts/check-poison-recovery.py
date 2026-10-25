#!/usr/bin/env python3
"""Reject recovery of poisoned locks in the Rust crates' source trees.

Scan test source too so production/test cfg boundaries cannot hide new recovery.
Ordinary container/writer ``into_inner`` methods are intentionally permitted.
"""

from pathlib import Path
import re
import sys


RECOVERY_PATTERNS = (
    re.compile(r"\bPoisonError\s*::\s*into_inner\b"),
    re.compile(r"\.\s*clear_poison\s*\("),
    re.compile(
        r"\|\s*(?P<error>[A-Za-z_]\w*)\s*\|\s*(?:\{[^;{}]*;?\s*)?"
        r"(?P=error)\s*\.\s*into_inner\s*\(",
        re.DOTALL,
    ),
    re.compile(
        r"\bErr\s*\(\s*(?:mut\s+)?(?P<error>[A-Za-z_]\w*)\s*\)\s*=>"
        r"[^;]*?\b(?P=error)\s*\.\s*into_inner\s*\(",
        re.DOTALL,
    ),
)


def violations(source: str) -> list[int]:
    """Return distinct line numbers of prohibited recovery expressions."""
    return sorted({
        source.count("\n", 0, match.start()) + 1
        for pattern in RECOVERY_PATTERNS
        for match in pattern.finditer(source)
    })


def main() -> int:
    crates = Path(__file__).resolve().parents[1] / "crates"
    failures = []
    for path in sorted(crates.glob("*/src/**/*.rs")):
        for line in violations(path.read_text()):
            failures.append(f"{path.relative_to(crates.parent)}:{line}: poisoned-lock recovery is forbidden")
    if failures:
        print("\n".join(failures), file=sys.stderr)
        print("Return a typed error or use an explicit fail-stop invariant boundary; never inspect the poisoned guard.", file=sys.stderr)
        return 1
    print("Poisoned-lock recovery guard passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
