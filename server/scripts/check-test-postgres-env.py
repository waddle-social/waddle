#!/usr/bin/env python3
"""Reject reads of the Postgres test environment outside its one reader.

`waddle_ws_test_support::require_test_postgres_url()` is the only code that
may read WADDLE_TEST_POSTGRES_URL or WADDLE_TEST_REQUIRE_POSTGRES: it panics
when a lane requires Postgres and the URL is missing, whereas a direct read
lets a test's early `return` report a skipped test as a pass.

Any Rust string literal (plain, raw, byte or C string) whose value is exactly
one of those names is a violation outside the helper module. That covers
`env::var("...")`, `env::var_os`, `option_env!`, `env!`, and names smuggled
through a `const`. Comments and longer strings that mention a name (such as
skip messages) are allowed.

Run with a repository root, or from the server workspace with no argument.
"""
from __future__ import annotations

import argparse
import re
from pathlib import Path

NAMES = frozenset({"WADDLE_TEST_POSTGRES_URL", "WADDLE_TEST_REQUIRE_POSTGRES"})
HELPER = Path("crates/waddle-ws-test-support/src/test_postgres.rs")

CHARACTER = re.compile(r"b?'(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|.)|[^'\\\n])'")
RAW_STRING = re.compile(r'(?:b|c|br|cr)?r(#{0,255})"')
STRING = re.compile(r'(?:b|c)?"')
ESCAPE = re.compile(r"\\(?:x([0-9a-fA-F]{2})|u\{([0-9a-fA-F_]{1,8})\}|(.))", re.DOTALL)
SIMPLE_ESCAPES = {"n": "\n", "r": "\r", "t": "\t", "0": "\0", "\\": "\\", "'": "'", '"': '"'}


def unescape(body: str) -> str:
    def replace(match: re.Match[str]) -> str:
        if match[1]:
            return chr(int(match[1], 16))
        if match[2]:
            return chr(int(match[2].replace("_", ""), 16))
        return SIMPLE_ESCAPES.get(match[3], match[3])

    # A backslash-newline continuation drops the newline and the next
    # line's leading whitespace.
    return ESCAPE.sub(replace, re.sub(r"\\\n\s*", "", body))


def string_literals(source: str) -> list[tuple[int, str]]:
    """Return (line, value) for every string literal outside comments."""
    literals: list[tuple[int, str]] = []
    cursor = 0
    line = 1
    identifier_before = False
    while cursor < len(source):
        start = cursor
        char = source[cursor]
        if source.startswith("//", cursor):
            end = source.find("\n", cursor)
            cursor = len(source) if end == -1 else end
        elif source.startswith("/*", cursor):
            depth = 1
            cursor += 2
            while depth and cursor < len(source):
                if source.startswith("/*", cursor):
                    depth += 1
                    cursor += 2
                elif source.startswith("*/", cursor):
                    depth -= 1
                    cursor += 2
                else:
                    cursor += 1
        elif not identifier_before and (match := RAW_STRING.match(source, cursor)):
            close = '"' + match[1]
            end = source.find(close, match.end())
            stop = len(source) if end == -1 else end
            literals.append((line, source[match.end():stop]))
            cursor = len(source) if end == -1 else end + len(close)
        elif not identifier_before and (match := STRING.match(source, cursor)):
            cursor = match.end()
            body_start = cursor
            while cursor < len(source):
                if source[cursor] == "\\":
                    cursor += 2
                elif source[cursor] == '"':
                    break
                else:
                    cursor += 1
            literals.append((line, unescape(source[body_start:cursor])))
            cursor += 1
        elif match := CHARACTER.match(source, cursor):
            cursor = match.end()
        else:
            cursor += 1
        identifier_before = cursor - start == 1 and (char.isalnum() or char == "_")
        line += source.count("\n", start, cursor)
    return literals


def violations(source: str) -> list[tuple[int, str]]:
    return [
        (line, f"read {value} through waddle_ws_test_support::require_test_postgres_url()")
        for line, value in string_literals(source)
        if value in NAMES
    ]


def rust_files(root: Path) -> tuple[Path, list[Path]]:
    server = root / "server" if (root / "server").is_dir() else root
    directories = [server / "crates", server / "extensions"]
    files = sorted({path for directory in directories if directory.is_dir()
                    for path in directory.rglob("*.rs")
                    if not {"target", "vendor", "node_modules"}.intersection(path.parts)})
    return server, files


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path, nargs="?", default=Path.cwd())
    args = parser.parse_args()
    root = args.root.resolve()
    server, files = rust_files(root)
    if not files:
        parser.error("no Rust source files found beneath the supplied root")
    helper = server / HELPER
    if helper not in files:
        parser.error(f"the helper module {HELPER} is missing")
    failures = []
    for path in files:
        if path == helper:
            continue
        for line, message in violations(path.read_text()):
            failures.append(f"{path.relative_to(root)}:{line}: {message}")
    if failures:
        print("\n".join(failures))
        return 1
    print(f"Postgres test environment reads passed ({len(files)} files)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
