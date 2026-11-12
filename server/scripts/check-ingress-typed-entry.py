#!/usr/bin/env python3
"""Reject ingress transactions that start outside the typed entry.

`ingress_substrate/typed_entry.rs` is where an ingress correctness
transaction begins: the entry acquires the transaction, bounds it, attests
its connection on an attested pool, and proves the protocol epoch before it
hands out the witness ingress repositories need. In the ingress modules this
check finds five kinds of code that could stand in for the entry:

- `attest`: a call of `attest_transaction(`.
- `begin`: a transaction opened directly on a database handle
  (`.begin()`, `.begin_immediate()`, `.begin_discard_postgres_on_drop()`,
  `.begin_catalog_fenced()`, `.begin_catalog_fenced_read_only()`). A
  `.begin()` whose receiver is named `uow` is the ingress unit of work's own
  begin, which goes through the entry, and is not counted.
- `epoch-lock`: a call of a function that takes the epoch row lock on a
  transaction without the entry's witness (`acquire_epoch_lock_first(`,
  `acquire_supported_epoch_lock_first(`, `install_epoch_proof(`,
  `read_live_epoch_for_share(`).
- `autocommit`: a statement connection taken straight from a database
  handle with `.guard()`. Its statements run outside any transaction, so a
  write issued on it could never have passed through the entry.

- `bare-transaction`: a function parameter of type `&mut Transaction`.
  Functions that write an epoch-guarded relation take `&mut
  EnteredTransaction`, the handle only the entry module constructs, or an
  ingress unit-of-work transaction; a bare transaction parameter is left to
  readers, to helpers the entry itself calls before the handle exists, and
  to schema code.

The scheduler stores outside the ingress modules reach ingress descendants
only through `EffectDescendantRepository::*_raw`. Those methods must take
`EnteredTransaction`, the handle only the entry module constructs, so the
compiler rejects a descendant write on a transaction opened any other way;
this check fails if a `*_raw` method takes anything else, or if the handle is
constructed outside the entry.

Each exception is listed in `ingress-typed-entry-allowlist.txt` as
`<path> <kind> <count>`. A listed file must contain exactly that many
occurrences of that kind, so a new one in an allowlisted file, or a stale
entry, also fails.

Occurrences are found in code only: comments and string literals are
ignored, and so are function definitions. `attest` is checked in every file.
The other kinds skip test code, which opens fixtures directly:
test-only files (`tests.rs`, `*_tests.rs`, `test_support.rs`,
`*_test_hooks.rs`, and anything under a `tests` or `*_tests` directory) and
`#[cfg(test)] mod` blocks.

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
RAW_REPOSITORY = SOURCE / "ingress_uow/effect_descendants.rs"
ENTERED = "EnteredTransaction"
RAW_METHOD = re.compile(r"\bfn\s+(?P<name>\w+_raw)\s*(?:<[^>]*>)?\s*\(\s*(?P<first>[^,)]*)")
ENTERED_FIRST = re.compile(rf"^\w+\s*:\s*&mut\s+(?:\w+::)*{ENTERED}\s*<[^>]*>\s*$")
ENTERED_LITERAL = re.compile(rf"\b{ENTERED}\s*\{{")
ENTERED_DEFINITION = re.compile(rf"\b(?:struct|impl(?:<[^>]*>)?(?:\s+[\w:]+(?:<[^>]*>)?\s+for)?)\s+$")

ATTEST = "attest"
BEGIN = "begin"
EPOCH_LOCK = "epoch-lock"
AUTOCOMMIT = "autocommit"
BARE_TRANSACTION = "bare-transaction"
PATTERNS = {
    ATTEST: re.compile(r"\battest_transaction\s*\("),
    BEGIN: re.compile(
        r"(?P<receiver>\b\w+)?\s*\.\s*begin"
        r"(?P<suffix>_immediate|_discard_postgres_on_drop|_catalog_fenced(?:_read_only)?)?"
        r"\s*\("
    ),
    EPOCH_LOCK: re.compile(
        r"\b(?:acquire_epoch_lock_first|acquire_supported_epoch_lock_first"
        r"|install_epoch_proof|read_live_epoch_for_share)\s*\("
    ),
    AUTOCOMMIT: re.compile(r"\.\s*guard\s*\("),
    BARE_TRANSACTION: re.compile(r"\w+\s*:\s*&mut\s+(?:crate::db::)?Transaction\s*<"),
}
KINDS = tuple(PATTERNS)
DEFINITION = re.compile(r"\bfn\s+$")
TEST_FILE = re.compile(r"(?:^|/)(?:tests|\w*_tests|test_support|\w*_test_hooks)(?:\.rs$|/)")
TEST_MODULE = re.compile(r"#\[cfg\(test\)\]\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+\w+\s*\{")

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


def blank(text: str) -> str:
    return re.sub(r"[^\n]", " ", text)


def code_only(source: str) -> str:
    """Blank comments and literals, keeping every newline in place."""
    return NON_CODE.sub(lambda match: blank(match[0]), source)


def without_test_modules(code: str) -> str:
    """Blank every `#[cfg(test)] mod name { ... }` block in comment-free code."""
    cursor = 0
    while match := TEST_MODULE.search(code, cursor):
        depth, end = 1, match.end()
        while depth and end < len(code):
            depth += {"{": 1, "}": -1}.get(code[end], 0)
            end += 1
        code = code[: match.start()] + blank(code[match.start() : end]) + code[end:]
        cursor = end
    return code


def is_test_file(relative: Path) -> bool:
    return bool(TEST_FILE.search(relative.as_posix()))


def occurrence_lines(source: str, kind: str) -> list[int]:
    """Lines holding one occurrence of `kind`, in code and not a definition."""
    code = code_only(source)
    if kind != ATTEST:
        code = without_test_modules(code)
    lines = []
    for match in PATTERNS[kind].finditer(code):
        if kind == BEGIN:
            if match["receiver"] == "uow" and not match["suffix"]:
                continue
            start = match.start("suffix") if match["suffix"] else match.end() - 1
        else:
            if DEFINITION.search(code, 0, match.start()):
                continue
            start = match.start()
        lines.append(code.count("\n", 0, start) + 1)
    return lines


def call_lines(source: str) -> list[int]:
    """Lines holding an `attest_transaction(` call, not its definition."""
    return occurrence_lines(source, ATTEST)


def untyped_raw_methods(source: str) -> list[tuple[int, str]]:
    """`*_raw` methods whose first parameter is not `&mut EnteredTransaction`."""
    code = code_only(source)
    return [
        (code.count("\n", 0, match.start()) + 1, match["name"])
        for match in RAW_METHOD.finditer(code)
        if not ENTERED_FIRST.match(" ".join(match["first"].split()))
    ]


def handle_constructions(source: str) -> list[int]:
    """Lines that build an `EnteredTransaction` with a struct literal."""
    code = code_only(source)
    return [
        code.count("\n", 0, match.start()) + 1
        for match in ENTERED_LITERAL.finditer(code)
        if not ENTERED_DEFINITION.search(code, 0, match.start())
    ]


def read_allowlist(path: Path) -> dict[tuple[Path, str], int]:
    allowed: dict[tuple[Path, str], int] = {}
    for number, raw in enumerate(path.read_text().splitlines(), 1):
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        fields = line.split()
        if (
            len(fields) != 3
            or fields[1] not in KINDS
            or not fields[2].isdigit()
            or int(fields[2]) < 1
        ):
            raise ValueError(
                f"{path.name}:{number}: expected '<path> <{'|'.join(KINDS)}> <count>'"
            )
        entry = (Path(fields[0]), fields[1])
        if entry in allowed:
            raise ValueError(f"{path.name}:{number}: duplicate entry {fields[0]} {fields[1]}")
        allowed[entry] = int(fields[2])
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
    for entry, kind in allowed:
        if entry == ENTRY or server / entry not in files:
            failures.append(f"{ALLOWLIST}: {entry} is not an ingress source file outside the entry")
        elif kind != ATTEST and is_test_file(entry):
            failures.append(f"{ALLOWLIST}: {entry} is test code, where {kind} is not checked")
    repository = server / RAW_REPOSITORY
    if repository.is_file():
        failures.extend(
            f"{RAW_REPOSITORY}:{line}: {name} must take `&mut {ENTERED}` first, "
            f"so it cannot run on a transaction the entry did not open"
            for line, name in untyped_raw_methods(repository.read_text())
        )
    for path in sorted((server / SOURCE).rglob("*.rs")):
        relative = path.relative_to(server)
        if relative == ENTRY:
            continue
        failures.extend(
            f"{relative}:{line}: {ENTERED} constructed outside {ENTRY.name}"
            for line in handle_constructions(path.read_text())
        )
    for path in files:
        relative = path.relative_to(server)
        if relative == ENTRY:
            continue
        source = path.read_text()
        for kind in KINDS:
            if kind != ATTEST and is_test_file(relative):
                continue
            lines = occurrence_lines(source, kind)
            expected = allowed.get((relative, kind), 0)
            if len(lines) == expected:
                continue
            if expected:
                failures.append(
                    f"{relative}: allowlisted for {expected} {kind} occurrence(s), "
                    f"found {len(lines)} (lines {lines})"
                )
            else:
                failures.extend(
                    f"{relative}:{line}: {kind} outside the typed ingress entry; "
                    f"begin through {ENTRY.name} or allowlist a documented exception"
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
    print(f"ingress typed-entry check passed ({scanned} files)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
