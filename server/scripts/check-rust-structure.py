#!/usr/bin/env python3
"""Check Rust import conventions without requiring a nightly formatter.

Run with a repository root, or from the server workspace with --server-only.
Comments, string literals and use-tree braces are excluded from scope tracking.
"""
from __future__ import annotations

import argparse
import re
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class Token:
    value: str
    line: int


IDENTIFIER = re.compile(r"(?:r#)?[A-Za-z_][A-Za-z_0-9]*")
CHARACTER = re.compile(r"'(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|.)|[^'\\\n])'")
RAW_STRING = re.compile(r'(?:b|c)?r(#{0,255})"')


def tokens(source: str) -> list[Token]:
    result: list[Token] = []
    cursor = 0
    line = 1
    while cursor < len(source):
        start = cursor
        char = source[cursor]
        if char.isspace():
            cursor += 1
        elif source.startswith("//", cursor):
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
        elif match := RAW_STRING.match(source, cursor):
            close = '"' + match[1]
            end = source.find(close, match.end())
            cursor = len(source) if end == -1 else end + len(close)
        elif char == '"' or source.startswith(('b"', 'c"'), cursor):
            cursor += 1 if char == '"' else 2
            while cursor < len(source):
                if source[cursor] == "\\":
                    cursor += 2
                elif source[cursor] == '"':
                    cursor += 1
                    break
                else:
                    cursor += 1
        elif match := CHARACTER.match(source, cursor):
            cursor = match.end()
        elif match := IDENTIFIER.match(source, cursor):
            result.append(Token(match[0], line))
            cursor = match.end()
        elif source.startswith("::", cursor):
            result.append(Token("::", line))
            cursor += 2
        else:
            result.append(Token(char, line))
            cursor += 1
        line += source.count("\n", start, cursor)
    return result


def use_leaves(values: list[str]) -> list[tuple[tuple[str, ...], str]]:
    """Return (immediate importing module, imported name) for a use tree."""
    leaves: list[tuple[tuple[str, ...], str]] = []

    def tree(index: int, prefix: tuple[str, ...]) -> int:
        if values[index] == "::":
            prefix = ("",) + prefix
            index += 1
        if values[index] == "{":
            index += 1
            while values[index] != "}":
                index = tree(index, prefix)
                if values[index] == ",":
                    index += 1
            return index + 1
        name = values[index]
        index += 1
        if index < len(values) and values[index] == "::":
            return tree(index + 1, prefix + (name,))
        leaves.append((prefix, name))
        if index < len(values) and values[index] == "as":
            index += 2
        return index

    tree(0, ())
    return leaves


def reexport(values: list[str], index: int) -> bool:
    if index and values[index - 1] == "pub":
        return True
    if index and values[index - 1] == ")":
        cursor = index - 2
        while cursor >= 0 and values[cursor] not in ("(", ";", "{", "}"):
            cursor -= 1
        return cursor > 0 and values[cursor] == "(" and values[cursor - 1] == "pub"
    return False


def violations(source: str) -> list[tuple[int, str]]:
    lexed = tokens(source)
    values = [token.value for token in lexed]
    # An external file's module does not appear on this stack. Only a literal
    # inline `mod name { ... }` permits the parent-module wildcard exception.
    inline_openings = {
        index + 2
        for index, value in enumerate(values[:-2])
        if value == "mod" and IDENTIFIER.fullmatch(values[index + 1])
        and values[index + 2] == "{"
    }
    scopes: list[str] = []
    failures: list[tuple[int, str]] = []
    index = 0
    while index < len(values):
        value = values[index]
        if value == "use":
            end = index + 1
            while end < len(values) and values[end] != ";":
                end += 1
            try:
                leaves = use_leaves(values[index + 1:end])
            except (IndexError, RecursionError):
                failures.append((lexed[index].line, "could not parse use tree"))
                index = end + 1
                continue
            if len({prefix for prefix, _ in leaves}) > 1:
                failures.append((lexed[index].line, "one use statement imports from multiple modules"))
            public = reexport(values, index)
            for prefix, name in leaves:
                if name != "*":
                    continue
                inline_parent = prefix == ("super",) and "module" in scopes
                local_enum = bool(prefix and prefix[-1][:1].isupper() and "block" in scopes)
                if not (public or inline_parent or local_enum):
                    failures.append((lexed[index].line, "replace wildcard import with explicit names"))
            index = end + 1
            continue
        if value == "{":
            scopes.append("module" if index in inline_openings else "block")
        elif value == "}" and scopes:
            scopes.pop()
        index += 1
    return failures


def rust_files(root: Path, server_only: bool) -> list[Path]:
    server = root / "server" if (root / "server").is_dir() else root
    directories = [server / "crates", server / "extensions"]
    if not server_only and (root / "apps").is_dir():
        directories.append(root / "apps")
    return sorted({path for directory in directories if directory.is_dir()
                   for path in directory.rglob("*.rs")
                   if not {"target", "vendor", "node_modules"}.intersection(path.parts)})


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path, nargs="?", default=Path.cwd())
    parser.add_argument("--server-only", action="store_true")
    args = parser.parse_args()
    root = args.root.resolve()
    failures = []
    files = rust_files(root, args.server_only)
    if not files:
        parser.error("no Rust source files found beneath the supplied root")
    for path in files:
        for line, message in violations(path.read_text()):
            failures.append(f"{path.relative_to(root)}:{line}: {message}")
    if failures:
        print("\n".join(failures))
        return 1
    print(f"Rust import structure passed ({len(files)} files)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
