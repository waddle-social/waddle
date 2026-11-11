#!/usr/bin/env python3
"""Reject physical attestation of an ingress transaction outside the typed entry.

`ingress_substrate/typed_entry.rs` is the one place an ingress correctness
transaction runs `attest_transaction(`: the entry begins the transaction,
bounds it, attests its connection and proves the protocol epoch before it
hands out the witness ingress repositories need. A second call site in the
ingress modules would be a transaction that can attest and write without
passing through that sequence.

Attach- and verify-time checks that attest a read-only transaction and never
write ingress rows are listed in `ingress-typed-entry-allowlist.txt`, one
`<path> <call count>` per line. A listed file must contain exactly that many
calls, so a new call in an allowlisted file, or a stale entry, also fails.

Calls are found in code only; comments and string literals are ignored.

Run with a repository root, or from the server workspace with no argument.
"""
from __future__ import annotations

import argparse
import re
from pathlib import Path

SOURCE = Path("crates/waddle-server/src")
INGRESS_MODULES = ("ingress", "ingress_uow", "ingress_substrate")
ENTRY = SOURCE / "ingress_substrate/typed_entry.rs"
ALLOWLIST = Path("scripts/ingress-typed-entry-allowlist.txt")
CALL = re.compile(r"\battest_transaction\s*\(")
DEFINITION = re.compile(r"\bfn\s+$")

# Comments, string literals and character literals, in the order they must be
# tried. Lifetimes (`'a`) do not match the character-literal alternative.
NON_CODE = re.compile(
    r"""//[^\n]*
      | /\*.*?\*/
      | (?:b|c|br|cr)?r(?P<hashes>\#{0,255})".*?"(?P=hashes)
      | (?:b|c)?"(?:\\.|[^"\\])*"
      | b?'(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|.)|[^'\\\n])'
    """,
    re.DOTALL | re.VERBOSE,
)


def code_only(source: str) -> str:
    """Blank comments and literals, keeping every newline in place."""
    return NON_CODE.sub(lambda match: re.sub(r"[^\n]", " ", match[0]), source)


def call_lines(source: str) -> list[int]:
    """Lines holding an `attest_transaction(` call, not its definition."""
    code = code_only(source)
    return [
        code.count("\n", 0, match.start()) + 1
        for match in CALL.finditer(code)
        if not DEFINITION.search(code, 0, match.start())
    ]


def read_allowlist(path: Path) -> dict[Path, int]:
    allowed: dict[Path, int] = {}
    for number, raw in enumerate(path.read_text().splitlines(), 1):
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        fields = line.split()
        if len(fields) != 2 or not fields[1].isdigit() or int(fields[1]) < 1:
            raise ValueError(f"{path.name}:{number}: expected '<path> <call count>'")
        entry = Path(fields[0])
        if entry in allowed:
            raise ValueError(f"{path.name}:{number}: duplicate entry {entry}")
        allowed[entry] = int(fields[1])
    return allowed


def ingress_files(server: Path) -> list[Path]:
    return sorted(
        path
        for module in INGRESS_MODULES
        if (server / SOURCE / module).is_dir()
        for path in (server / SOURCE / module).rglob("*.rs")
    )


def check(server: Path) -> tuple[list[str], int]:
    """Return (failures, scanned file count) for one server workspace."""
    files = ingress_files(server)
    if server / ENTRY not in files:
        return [f"the typed entry {ENTRY} is missing"], len(files)
    try:
        allowed = read_allowlist(server / ALLOWLIST)
    except (OSError, ValueError) as error:
        return [str(error)], len(files)

    failures = []
    if not call_lines((server / ENTRY).read_text()):
        failures.append(f"{ENTRY}: the typed entry no longer calls attest_transaction(")
    for entry in allowed:
        if entry == ENTRY or server / entry not in files:
            failures.append(f"{ALLOWLIST}: {entry} is not an ingress source file outside the entry")
    for path in files:
        relative = path.relative_to(server)
        if relative == ENTRY:
            continue
        lines = call_lines(path.read_text())
        expected = allowed.get(relative, 0)
        if len(lines) == expected:
            continue
        if expected:
            failures.append(
                f"{relative}: allowlisted for {expected} attest_transaction( call(s), "
                f"found {len(lines)} (lines {lines})"
            )
        else:
            failures.extend(
                f"{relative}:{line}: attest_transaction( outside the typed ingress entry; "
                f"begin through {ENTRY.name} or allowlist a read-only attach-time check"
                for line in lines
            )
    return failures, len(files)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path, nargs="?", default=Path.cwd())
    args = parser.parse_args()
    root = args.root.resolve()
    server = root / "server" if (root / "server").is_dir() else root
    failures, scanned = check(server)
    if not scanned:
        parser.error("no ingress Rust source files found beneath the supplied root")
    if failures:
        print("\n".join(failures))
        return 1
    print(f"ingress typed-entry attestation check passed ({scanned} files)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
