#!/usr/bin/env python3
"""Fail when a Postgres-backed test can run outside nextest's `postgres` group.

Under nextest every test is its own process, so the process-local locks some
Postgres fixtures take do not serialize them. Tests that reset or alter the
shared `public` schema of WADDLE_TEST_POSTGRES_URL must run in the
single-threaded `postgres` test group (`.config/nextest.toml`), or a
concurrent test can see a half-reset database.

This check finds every Rust test that reads the test database (directly via
`require_test_postgres_url()` or the variable name) and requires it to match
the group's filter, or to sit in a module listed in
`scripts/postgres-schema-isolated.txt` (tests that create and drop their own
uniquely named schema and touch nothing shared).

Coverage is decided per test:
- a test counts when its body reads the database, or calls (by name) a
  function that does; calls are traced within the test's own file for
  library unit tests and across the binary's module tree for integration
  tests. A library fixture that hides the read in another file is not traced,
  so keep such fixtures and their callers inside a module the filter covers.

The filter grammar understood here is what the file uses today:
`package(P) & (atom | atom | ...)` or `package(P) & atom`, where an atom is
`binary(name)` or `test(substring)`. Anything else is rejected so the check
cannot silently drift from nextest's semantics.
"""
from __future__ import annotations

import argparse
from dataclasses import dataclass, field
import fnmatch
import os
from pathlib import Path
import re
import sys
import tomllib

GROUP = "postgres"
READ = re.compile(r"require_test_postgres_url\s*\(|\"WADDLE_TEST_POSTGRES_URL\"")
MOD_DECL = re.compile(r"\bmod\s+(r#)?([A-Za-z_][A-Za-z_0-9]*)\s*([;{])")
PATH_ATTR = re.compile(r'#\[\s*path\s*=\s*"([^"]+)"\s*\]')
TEST_ATTR = re.compile(r"#\[\s*(?:[A-Za-z_][A-Za-z_0-9]*::)*test\b[^\]]*\]")
FN_DECL = re.compile(r"\bfn\s+(r#)?([A-Za-z_][A-Za-z_0-9]*)")
ATOM = re.compile(r"^(binary|test)\((.+)\)$")
HELPER_CRATE = "waddle-ws-test-support"


class CheckError(Exception):
    pass


@dataclass
class Filter:
    package: str
    binaries: list[str] = field(default_factory=list)
    tests: list[str] = field(default_factory=list)

    def covers(self, package: str, binary: str, test: str) -> bool:
        if package != self.package:
            return False
        if any(fnmatch.fnmatchcase(binary, pattern) for pattern in self.binaries):
            return True
        return any(substring in test for substring in self.tests)


def parse_filter(expression: str) -> Filter:
    text = " ".join(expression.split())
    match = re.fullmatch(r"package\(([^()]+)\)\s*&\s*(.+)", text)
    if not match:
        raise CheckError(f"unsupported postgres group filter: {text!r}")
    body = match[2].strip()
    if body.startswith("(") and body.endswith(")"):
        body = body[1:-1]
    result = Filter(match[1].strip())
    for raw in body.split("|"):
        atom = ATOM.match(raw.strip())
        if not atom or any(char in atom[2] for char in "()&|"):
            raise CheckError(f"unsupported atom {raw.strip()!r} in postgres group filter")
        kind, value = atom[1], atom[2].strip()
        (result.binaries if kind == "binary" else result.tests).append(value)
    return result


def group_filters(nextest_toml: Path) -> list[Filter]:
    config = tomllib.loads(nextest_toml.read_text())
    filters = []
    for profile in config.get("profile", {}).values():
        for override in profile.get("overrides", []):
            if override.get("test-group") == GROUP:
                filters.append(parse_filter(override["filter"]))
    if not filters:
        raise CheckError(f"{nextest_toml}: no override assigns test-group = '{GROUP}'")
    return filters


LEXEME = re.compile(
    r"//|/\*|(?P<raw>(?:b|c|br|cr)?r(?P<hashes>#*)\")|(?P<str>(?:b|c)?\")"
    r"|(?P<char>b?'(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|.)|[^'\\\n])')"
)
STRING_TAIL = re.compile(r'(?:[^"\\]|\\.)*"', re.DOTALL)
BLOCK_COMMENT = re.compile(r"/\*|\*/")


def strip_comments_and_strings(source: str, keep_strings: bool = False) -> str:
    """Blank out comments and (unless kept) string/char literal bodies,
    keeping offsets so that positions in the result match the source."""
    out = list(source)
    n = len(source)

    def blank(start: int, end: int) -> None:
        for k in range(start, end):
            if out[k] != "\n":
                out[k] = " "

    i = 0
    while (match := LEXEME.search(source, i)) is not None:
        start = match.start()
        token = match.group(0)
        if token == "//":
            end = source.find("\n", start)
            end = n if end == -1 else end
            blank(start, end)
        elif token == "/*":
            depth, j = 1, start + 2
            while depth and (inner := BLOCK_COMMENT.search(source, j)) is not None:
                depth += 1 if inner.group(0) == "/*" else -1
                j = inner.end()
            end = j if depth == 0 else n
            blank(start, end)
        elif match.group("raw") is not None:
            close = '"' + match.group("hashes")
            found = source.find(close, match.end())
            end = n if found == -1 else found + len(close)
            if not keep_strings:
                blank(match.end(), max(match.end(), end - len(close)))
        elif match.group("str") is not None:
            tail = STRING_TAIL.match(source, match.end())
            end = n if tail is None else tail.end()
            if not keep_strings:
                blank(match.end(), max(match.end(), end - 1))
        else:
            end = match.end()
            if not keep_strings:
                blank(start, end)
        i = max(end, start + 1)
    return "".join(out)


@dataclass
class FnItem:
    name: str
    reads: bool  # body reads the test database directly
    code: str  # comment- and string-free body, for call references
    test: tuple[str, ...] | None  # full test name when this is a test fn


@dataclass
class ModuleFile:
    path: Path
    module: tuple[str, ...]
    reads: bool
    fns: list[FnItem]
    children: list["ModuleFile"]


_SCANS: dict[tuple[Path, tuple[str, ...], bool], ModuleFile] = {}


def scan_module_file(path: Path, module: tuple[str, ...], is_mod_rs: bool,
                     seen: set[Path]) -> ModuleFile:
    if path in seen:
        raise CheckError(f"module cycle at {path}")
    key = (Path(os.path.normpath(path)), module, is_mod_rs)
    if key in _SCANS:
        return _SCANS[key]
    seen = seen | {path}
    _SCANS[key] = result = _scan_module_file(path, module, is_mod_rs, seen)
    return result


def _scan_module_file(path: Path, module: tuple[str, ...], is_mod_rs: bool,
                      seen: set[Path]) -> ModuleFile:
    source = path.read_text()
    # The variable name only appears inside a string literal, so reads are
    # matched on comment-free text that keeps strings; structure and call
    # references come from text without comments or strings.
    readable = strip_comments_and_strings(source, keep_strings=True)
    reads = bool(READ.search(readable))
    code = strip_comments_and_strings(source)
    own_dir = path.parent if is_mod_rs else path.parent / path.stem

    fns: list[FnItem] = []
    children: list[ModuleFile] = []
    inline: list[tuple[str, int]] = []  # (module name, brace depth it opened at)
    depth = 0
    pending_test = False
    pending_path: str | None = None
    i = 0
    tokens = re.compile(r"#\[[^\]]*\]|[{}]|\bmod\s+(?:r#)?[A-Za-z_][A-Za-z_0-9]*\s*[;{]|\bfn\s+(?:r#)?[A-Za-z_][A-Za-z_0-9]*")
    for match in tokens.finditer(code):
        token = match.group(0)
        if token.startswith("#["):
            if TEST_ATTR.fullmatch(token):
                pending_test = True
            # String bodies are blanked in `code`; read the path from source.
            path_attr = PATH_ATTR.fullmatch(source[match.start():match.end()])
            if path_attr:
                pending_path = path_attr[1]
            continue
        if token == "{":
            depth += 1
            continue
        if token == "}":
            depth -= 1
            while inline and inline[-1][1] > depth:
                inline.pop()
            continue
        if token.startswith("mod"):
            decl = MOD_DECL.match(token)
            name, terminator = decl[2], decl[3]
            inline_names = tuple(name_ for name_, _ in inline)
            if terminator == "{":
                depth += 1
                inline.append((name, depth))
            else:
                base = own_dir.joinpath(*inline_names)
                if pending_path is not None:
                    anchor = path.parent if not inline_names else base
                    child_path = Path(os.path.normpath(anchor / pending_path))
                    child_is_mod_rs = child_path.name == "mod.rs"
                else:
                    child_path = base / f"{name}.rs"
                    child_is_mod_rs = False
                    if not child_path.exists():
                        child_path = base / name / "mod.rs"
                        child_is_mod_rs = True
                if child_path.exists():
                    children.append(scan_module_file(
                        child_path, module + inline_names + (name,), child_is_mod_rs, seen))
            pending_path = None
            pending_test = False
            continue
        # fn declaration: its body is the brace block before any `;`.
        fn_name = FN_DECL.match(token)[2]
        test = module + tuple(name_ for name_, _ in inline) + (fn_name,) if pending_test else None
        body = body_span(code, match.end())
        if body is not None:
            start, end = body
            fns.append(FnItem(fn_name, bool(READ.search(readable[start:end])), code[start:end], test))
        pending_test = False
        pending_path = None
    return ModuleFile(path, module, reads, fns, children)


def body_span(code: str, after: int) -> tuple[int, int] | None:
    brace = code.find("{", after)
    semicolon = code.find(";", after)
    if brace == -1 or (semicolon != -1 and semicolon < brace):
        return None
    depth, i = 0, brace
    while i < len(code):
        if code[i] == "{":
            depth += 1
        elif code[i] == "}":
            depth -= 1
            if depth == 0:
                return brace, i + 1
        i += 1
    return brace, len(code)


def postgres_tests(files: list[ModuleFile]) -> list[tuple[ModuleFile, tuple[str, ...]]]:
    """Tests whose body reads the database, directly or through fns in `files`."""
    items = [(module_file, fn) for module_file in files for fn in module_file.fns]
    marked = {fn.name for _, fn in items if fn.reads}
    changed = True
    while changed:
        changed = False
        pattern = re.compile(r"\b(?:" + "|".join(map(re.escape, sorted(marked))) + r")\s*(?:::<[^>]*>)?\s*\(") if marked else None
        for _, fn in items:
            if fn.name not in marked and pattern and pattern.search(fn.code):
                marked.add(fn.name)
                changed = True
    return [(module_file, fn.test) for module_file, fn in items
            if fn.test is not None and (fn.reads or (fn.name in marked) or
                                        (marked and pattern.search(fn.code)))]


def walk(module_file: ModuleFile):
    yield module_file
    for child in module_file.children:
        yield from walk(child)


def load_allowlist(path: Path) -> set[str]:
    entries = set()
    for number, raw in enumerate(path.read_text().splitlines(), start=1):
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        if len(line.split()) != 1:
            raise CheckError(f"{path}:{number}: expected one file path, got {raw!r}")
        entries.add(line)
    return entries


def package_name(crate_dir: Path) -> str:
    manifest = tomllib.loads((crate_dir / "Cargo.toml").read_text())
    return manifest["package"]["name"]


def uncovered(server: Path, filters: list[Filter], allowlist: set[str]) -> tuple[list[str], set[str]]:
    failures: list[str] = []
    used_allowlist: set[str] = set()

    def rel(path: Path) -> str:
        return Path(os.path.normpath(path)).relative_to(os.path.normpath(server)).as_posix()

    def covered(package: str, binary: str, test: tuple[str, ...]) -> bool:
        name = "::".join(test)
        return any(f.covers(package, binary, name) for f in filters)

    for crate_dir in sorted((server / "crates").iterdir()):
        if not (crate_dir / "Cargo.toml").is_file():
            continue
        package = package_name(crate_dir)
        if package == HELPER_CRATE:
            continue
        lib = crate_dir / "src" / "lib.rs"
        if lib.is_file():
            # Library fixtures are traced within their own file: crate-wide
            # name matching would tie every `open(` call to a fixture.
            for files in ([m] for m in walk(scan_module_file(lib, (), True, set()))):
                for module_file, test in postgres_tests(files):
                    if covered(package, package, test):
                        continue
                    key = rel(module_file.path)
                    if key in allowlist:
                        used_allowlist.add(key)
                        continue
                    failures.append(f"{key}: {package} {'::'.join(test)}")
        tests_dir = crate_dir / "tests"
        if tests_dir.is_dir():
            for root in sorted(tests_dir.glob("*.rs")):
                binary = root.stem
                tree = list(walk(scan_module_file(root, (), True, set())))
                for module_file, test in postgres_tests(tree):
                    if covered(package, binary, test):
                        continue
                    key = rel(module_file.path)
                    if key in allowlist:
                        used_allowlist.add(key)
                        continue
                    failures.append(f"{key}: {package}::{binary} {'::'.join(test)}")
    return failures, used_allowlist


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("server", type=Path, nargs="?", default=Path.cwd(),
                        help="the server workspace (default: current directory)")
    args = parser.parse_args()
    server = args.server
    try:
        filters = group_filters(server / ".config" / "nextest.toml")
        allowlist = load_allowlist(server / "scripts" / "postgres-schema-isolated.txt")
        failures, used = uncovered(server, filters, allowlist)
    except CheckError as error:
        print(error, file=sys.stderr)
        return 2
    stale = sorted(allowlist - used)
    for entry in stale:
        print(f"stale allowlist entry (no uncovered Postgres test there): {entry}", file=sys.stderr)
    for failure in failures:
        print(f"Postgres-backed test outside the '{GROUP}' group: {failure}", file=sys.stderr)
    if failures or stale:
        print("Add the module or binary to the postgres test-group filter in .config/nextest.toml, "
              "or, if every such test uses its own uniquely named schema, list the file in "
              "scripts/postgres-schema-isolated.txt with the reason.", file=sys.stderr)
        return 1
    print(f"Postgres test group covers every Postgres-backed test ({len(allowlist)} schema-isolated files allowlisted)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
