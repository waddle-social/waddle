#!/usr/bin/env python3
"""Check detected Postgres-backed tests against nextest's `postgres` group.

Under nextest every test is its own process, so the process-local locks some
Postgres fixtures take do not serialize them. Tests that reset or alter the
shared `public` schema of WADDLE_TEST_POSTGRES_URL must run in the
single-threaded `postgres` test group (`.config/nextest.toml`), or a
concurrent test can see a half-reset database.

This check finds Rust tests that it can trace to a test-database read (via
`require_test_postgres_url()` or the variable name) and requires them to match
the group's filter, or to sit in a module listed in
`scripts/postgres-schema-isolated.txt` (tests that create and drop their own
uniquely named schema and touch nothing shared).

Coverage is decided per test:
- a test counts when its body reads the database or calls a function that
  does. `include!`d files are spliced in. Calls by qualified name
  (`IngressFixture::postgres(`, `messages_offline::postgres_state(`) are
  traced across the crate (library) or the binary's module tree
  (integration tests); calls by bare name are traced within one file for
  library tests (crate-wide, every `open(` would match some fixture) and
  across the binary for integration tests. Integration binaries are
  `tests/*.rs` and `tests/<name>/main.rs`.
- this is a source-level heuristic, not a complete Rust call graph. Cross-file
  library calls through imported bare names or aliases, method dispatch,
  macro-generated calls and helpers in other crates can be missed. Review new
  database fixtures and their callers for correct grouping independently.
- the allowlist names files whose tests are isolated, or single fixtures
  (`<file>::<Type>::<fn>`) that always create their own schema; a test
  whose only route to the database is such a fixture needs no listing.

Requires Python 3.11+ (tomllib).

The filter grammar understood here is what the file uses today:
`package(P) & (atom | atom | ...)` or `package(P) & atom`, where an atom is
`binary(name)` or `test(substring)`. Anything else is rejected so the check
cannot silently drift from nextest's semantics. Group assignments are limited
to platform-independent `postgres` overrides in `profile.default`, and the
group must have `max-threads = 1`; other assignments fail closed until their
precedence can be modeled.
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
INCLUDE = re.compile(
    r'include!\s*\(\s*(?:concat!\s*\(\s*env!\s*\(\s*"CARGO_MANIFEST_DIR"\s*\)\s*,\s*"([^"]+)"\s*\)'
    r'|"([^"]+)")\s*\)\s*;?'
)
IDENT = re.compile(r"[A-Za-z_][A-Za-z_0-9]*")
BRACE = re.compile(r"[{}]")


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
        if value.startswith("~"):
            value = value[1:]  # nextest's explicit "contains" matcher
        elif value[:1] in ("=", "/", "#"):
            raise CheckError(f"unsupported matcher in {raw.strip()!r}: only the default "
                             "(contains for test, glob for binary) and ~ are understood")
        (result.binaries if kind == "binary" else result.tests).append(value)
    return result


def group_filters(nextest_toml: Path) -> list[Filter]:
    config = tomllib.loads(nextest_toml.read_text())
    group_config = config.get("test-groups", {}).get(GROUP, {})
    max_threads = group_config.get("max-threads")
    if type(max_threads) is not int or max_threads != 1:
        raise CheckError(f"{nextest_toml}: {GROUP} group must set max-threads = 1")
    filters = []
    for name, profile in config.get("profile", {}).items():
        for override in profile.get("overrides", []):
            group = override.get("test-group")
            if group is None:
                continue
            # Nextest resolves settings per key using the first matching
            # override, and a named profile can override the default profile.
            # Until this checker models arbitrary filter precedence, reject
            # any alternate group assignment rather than unioning filters and
            # silently accepting tests that a later profile can ungroup.
            if name != "default" or group != GROUP:
                raise CheckError(
                    f"profile.{name}: unsupported test-group assignment {group!r}; "
                    "the postgres guard requires all group assignments in profile.default"
                )
            if "platform" in override or "filter" not in override:
                raise CheckError(
                    f"profile.{name}: postgres group assignment requires a platform-independent filter"
                )
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


@dataclass(eq=False)
class FnItem:
    name: str
    qualifier: str  # impl self type, else the enclosing module's name
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
    included: set[Path]  # files spliced in through include!


def crate_root(path: Path) -> Path:
    for parent in path.parents:
        if (parent / "Cargo.toml").is_file():
            return parent
    raise CheckError(f"{path}: no enclosing Cargo.toml")


def expand_includes(source: str, path: Path, included: set[Path], depth: int = 0) -> str:
    """Splice `include!`d files into `source`, like the compiler does."""
    if depth > 8:
        raise CheckError(f"{path}: include! nested too deeply")
    if "include!" not in source:
        return source
    readable = strip_comments_and_strings(source, keep_strings=True)
    pieces, last = [], 0
    for match in INCLUDE.finditer(readable):
        target = (crate_root(path) / match[1].lstrip("/")) if match[1] else (path.parent / match[2])
        target = Path(os.path.normpath(target))
        if not target.is_file():
            raise CheckError(f"{path}: include! target {target} does not exist")
        included.add(target)
        pieces.append(source[last:match.start()])
        pieces.append(expand_includes(target.read_text(), target, included, depth + 1))
        last = match.end()
    pieces.append(source[last:])
    return "".join(pieces)


def impl_self_type(header: str) -> str:
    """`impl<T> Trait for path::Type<T> where ...` -> `Type`."""
    text = header.strip()
    if text.startswith("<"):
        depth = 0
        for index, char in enumerate(text):
            depth += char == "<"
            depth -= char == ">"
            if depth == 0:
                text = text[index + 1:]
                break
    text = re.split(r"\bwhere\b", text)[0]
    if re.search(r"\bfor\b", text):
        text = re.split(r"\bfor\b", text)[-1]
    text = text.split("<")[0]
    names = IDENT.findall(text.split("::")[-1]) if text.strip() else []
    names = [name for name in names if name not in ("dyn", "mut", "const")]
    return names[0] if names else ""


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
    included: set[Path] = set()
    source = expand_includes(path.read_text(), path, included)
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
    impls: list[tuple[str, int]] = []  # (impl self type, brace depth it opened at)
    depth = 0
    pending_test = False
    pending_path: str | None = None
    i = 0
    tokens = re.compile(r"#\[[^\]]*\]|\bimpl\b[^{};]*\{|[{}]|\bmod\s+(?:r#)?[A-Za-z_][A-Za-z_0-9]*\s*[;{]|\bfn\s+(?:r#)?[A-Za-z_][A-Za-z_0-9]*")
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
        if token.startswith("impl"):
            depth += 1
            impls.append((impl_self_type(token[4:-1]), depth))
            continue
        if token == "{":
            depth += 1
            continue
        if token == "}":
            depth -= 1
            while inline and inline[-1][1] > depth:
                inline.pop()
            while impls and impls[-1][1] > depth:
                impls.pop()
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
        scope = module + tuple(name_ for name_, _ in inline)
        test = scope + (fn_name,) if pending_test else None
        # Methods are called as `Type::name(`, free fns as `module::name(`.
        qualifier = impls[-1][0] if impls and (not inline or impls[-1][1] > inline[-1][1]) \
            else (scope[-1] if scope else "")
        body = body_span(code, match.end())
        if body is not None:
            start, end = body
            fns.append(FnItem(fn_name, qualifier, bool(READ.search(readable[start:end])),
                              code[start:end], test))
        pending_test = False
        pending_path = None
    return ModuleFile(path, module, reads, fns, children, included)


def body_span(code: str, after: int) -> tuple[int, int] | None:
    brace = code.find("{", after)
    semicolon = code.find(";", after)
    if brace == -1 or (semicolon != -1 and semicolon < brace):
        return None
    depth = 0
    for match in BRACE.finditer(code, brace):
        depth += 1 if match.group(0) == "{" else -1
        if depth == 0:
            return brace, match.end()
    return brace, len(code)


CALL_SUFFIX = r"\s*(?:::<[^>]*>)?\s*\("


def postgres_tests(files: list[ModuleFile], bare_across_files: bool,
                   isolated: "IsolatedFixtures") -> list[tuple[ModuleFile, tuple[str, ...]]]:
    """Tests that reach the database: a fn is marked when its body reads it
    (unless it is a listed schema-isolated fixture), calls a marked fn of the
    same file by bare name (of any file in `files` if `bare_across_files`),
    or calls a marked fn of any file in `files` by its qualified form
    (`Type::name(` or `module::name(`)."""
    items = [(module_file, fn) for module_file in files for fn in module_file.fns]
    marked = {id(fn) for module_file, fn in items if fn.reads and not isolated.covers(module_file, fn)}
    changed = True
    while changed:
        changed = False
        qualified = sorted({f"{fn.qualifier}::{fn.name}" for _, fn in items
                            if id(fn) in marked and fn.qualifier})
        qualified_call = re.compile(r"\b(?:" + "|".join(map(re.escape, qualified)) + ")" + CALL_SUFFIX) \
            if qualified else None
        bare_by_scope: dict[object, set[str]] = {}
        for module_file, fn in items:
            if id(fn) in marked:
                bare_by_scope.setdefault(None if bare_across_files else id(module_file), set()).add(fn.name)
        bare_calls = {scope: re.compile(r"\b(?:" + "|".join(map(re.escape, sorted(names))) + ")" + CALL_SUFFIX)
                      for scope, names in bare_by_scope.items()}
        for module_file, fn in items:
            if id(fn) in marked or isolated.covers(module_file, fn):
                continue
            bare_call = bare_calls.get(None if bare_across_files else id(module_file))
            if (bare_call and bare_call.search(fn.code)) or (qualified_call and qualified_call.search(fn.code)):
                marked.add(id(fn))
                changed = True
    return [(module_file, fn.test) for module_file, fn in items
            if fn.test is not None and id(fn) in marked]


def walk(module_file: ModuleFile):
    yield module_file
    for child in module_file.children:
        yield from walk(child)


class IsolatedFixtures:
    """`<file>::<Qualifier>::<fn>` allowlist entries: fixtures that only ever
    use their own uniquely named schema."""

    def __init__(self, entries: set[str], rel) -> None:
        self.entries = {}
        for entry in entries:
            file, _, rest = entry.partition(".rs::")
            qualifier, _, name = rest.rpartition("::")
            self.entries[entry] = (file + ".rs", qualifier, name)
        self.rel = rel
        self.used: set[str] = set()

    def covers(self, module_file: ModuleFile, fn: FnItem) -> bool:
        if not fn.reads:
            return False
        files = {self.rel(module_file.path)} | {self.rel(path) for path in module_file.included}
        for entry, (file, qualifier, name) in self.entries.items():
            if fn.reads and fn.name == name and fn.qualifier == qualifier and file in files:
                self.used.add(entry)
                return True
        return False


def load_allowlist(path: Path) -> set[str]:
    entries = set()
    for number, raw in enumerate(path.read_text().splitlines(), start=1):
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        if len(line.split()) != 1:
            raise CheckError(f"{path}:{number}: expected one file path or fixture, got {raw!r}")
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

    isolated = IsolatedFixtures({entry for entry in allowlist if ".rs::" in entry}, rel)
    isolated_files = {entry for entry in allowlist if ".rs::" not in entry}

    def covered(package: str, binary: str, test: tuple[str, ...]) -> bool:
        name = "::".join(test)
        return any(f.covers(package, binary, name) for f in filters)

    def report(module_file: ModuleFile, label: str, test: tuple[str, ...]) -> None:
        key = rel(module_file.path)
        if key in isolated_files:
            used_allowlist.add(key)
        else:
            failures.append(f"{key}: {label} {'::'.join(test)}")

    for crate_dir in sorted((server / "crates").iterdir()):
        if not (crate_dir / "Cargo.toml").is_file():
            continue
        package = package_name(crate_dir)
        if package == HELPER_CRATE:
            continue
        lib = crate_dir / "src" / "lib.rs"
        if lib.is_file():
            # Bare-name calls are traced within one file only (crate-wide
            # they would tie every `open(` to some fixture); qualified calls
            # (`IngressFixture::postgres(`) are traced across the crate.
            tree = list(walk(scan_module_file(lib, (), True, set())))
            for module_file, test in postgres_tests(tree, False, isolated):
                if not covered(package, package, test):
                    report(module_file, package, test)
        tests_dir = crate_dir / "tests"
        if tests_dir.is_dir():
            # Every tests/*.rs and tests/<name>/main.rs is its own binary,
            # including support files such as tests/ingress_support.rs, which
            # simply declare no tests.
            roots = [(root, root.stem) for root in sorted(tests_dir.glob("*.rs"))]
            roots += [(root, root.parent.name) for root in sorted(tests_dir.glob("*/main.rs"))]
            for root, binary in roots:
                tree = list(walk(scan_module_file(root, (), True, set())))
                for module_file, test in postgres_tests(tree, True, isolated):
                    if not covered(package, binary, test):
                        report(module_file, f"{package}::{binary}", test)
    return failures, used_allowlist | isolated.used


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
    print(f"Postgres test group covers detected Postgres-backed tests ({len(allowlist)} allowlist entries)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
