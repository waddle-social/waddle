#!/usr/bin/env python3
"""Verify Rust import conventions, lock safety, and panic discipline in operator/."""
from __future__ import annotations

import argparse
from dataclasses import dataclass
from pathlib import Path
import re
import sys


@dataclass(frozen=True)
class Token:
    value: str
    line: int


IDENTIFIER = re.compile(r"(?:r#)?[A-Za-z_]\w*", re.ASCII)
CHARACTER = re.compile(r"'(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|.)|[^'\\\n])'")
RAW_STRING = re.compile(r'[bc]?r(#{0,255})"')
POISONED_LOCK_REASON = "poisoned-lock recovery is forbidden"

FORBIDDEN_PATTERNS = (
    (re.compile(r"\bPoisonError\s*::\s*into_inner\b"), POISONED_LOCK_REASON),
    (re.compile(r"\.\s*clear_poison\s*\("), POISONED_LOCK_REASON),
    (
        re.compile(
            r"\|\s*(?P<error>[A-Za-z_]\w*)\s*\|\s*(?:\{[^;{}]*;?\s*)?"
            r"(?P=error)\s*\.\s*into_inner\s*\(",
            re.DOTALL,
        ),
        POISONED_LOCK_REASON,
    ),
    (
        re.compile(
            r"\bErr\s*\(\s*(?:mut\s+)?(?P<error>[A-Za-z_]\w*)\s*\)\s*=>"
            r"[^;]*?\b(?P=error)\s*\.\s*into_inner\s*\(",
            re.DOTALL,
        ),
        POISONED_LOCK_REASON,
    ),
    (re.compile(r"\bcatch_unwind\b"), "panic::catch_unwind is forbidden in first-party Rust code"),
)


def _skip_block_comment(source: str, cursor: int) -> int:
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
    return cursor


def _skip_quoted_string(source: str, cursor: int) -> int:
    cursor += 1 if source[cursor] == '"' else 2
    while cursor < len(source):
        if source[cursor] == "\\":
            cursor += 2
        elif source[cursor] == '"':
            return cursor + 1
        else:
            cursor += 1
    return cursor


def _skip_non_code(source: str, cursor: int) -> int | None:
    char = source[cursor]
    if char.isspace():
        return cursor + 1
    if source.startswith("//", cursor):
        end = source.find("\n", cursor)
        return len(source) if end == -1 else end
    if source.startswith("/*", cursor):
        return _skip_block_comment(source, cursor)
    if match := RAW_STRING.match(source, cursor):
        close = '"' + match[1]
        end = source.find(close, match.end())
        return len(source) if end == -1 else end + len(close)
    if char == '"' or source.startswith(('b"', 'c"'), cursor):
        return _skip_quoted_string(source, cursor)
    if match := CHARACTER.match(source, cursor):
        return match.end()
    return None


def tokens(source: str) -> list[Token]:
    result: list[Token] = []
    cursor = 0
    line = 1
    while cursor < len(source):
        start = cursor
        skipped = _skip_non_code(source, cursor)
        if skipped is not None:
            cursor = skipped
        elif match := IDENTIFIER.match(source, cursor):
            result.append(Token(match[0], line))
            cursor = match.end()
        elif source.startswith("::", cursor):
            result.append(Token("::", line))
            cursor += 2
        else:
            result.append(Token(source[cursor], line))
            cursor += 1
        line += source.count("\n", start, cursor)
    return result


def _parse_use_tree(
    values: list[str],
    index: int,
    prefix: tuple[str, ...],
    leaves: list[tuple[tuple[str, ...], str]],
) -> int:
    if values[index] == "::":
        prefix = ("",) + prefix
        index += 1
    if values[index] == "{":
        index += 1
        while values[index] != "}":
            index = _parse_use_tree(values, index, prefix, leaves)
            if values[index] == ",":
                index += 1
        return index + 1
    name = values[index]
    index += 1
    if index < len(values) and values[index] == "::":
        return _parse_use_tree(values, index + 1, prefix + (name,), leaves)
    leaves.append((prefix, name))
    if index < len(values) and values[index] == "as":
        index += 2
    return index


def use_leaves(values: list[str]) -> list[tuple[tuple[str, ...], str]]:
    """Return (immediate importing module, imported name) for a use tree."""
    leaves: list[tuple[tuple[str, ...], str]] = []
    _parse_use_tree(values, 0, (), leaves)
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


def _inline_mod_openings(values: list[str]) -> set[int]:
    return {
        index + 2
        for index, value in enumerate(values[:-2])
        if value == "mod"
        and IDENTIFIER.fullmatch(values[index + 1])
        and values[index + 2] == "{"
    }


def _find_use_end(values: list[str], start: int) -> int:
    end = start
    while end < len(values) and values[end] != ";":
        end += 1
    return end


def _wildcard_allowed(prefix: tuple[str, ...], public: bool, scopes: list[str]) -> bool:
    inline_parent = prefix == ("super",) and "module" in scopes
    local_enum = bool(prefix and prefix[-1][:1].isupper() and "block" in scopes)
    return public or inline_parent or local_enum


def _use_violations(
    line: int,
    values: list[str],
    index: int,
    end: int,
    scopes: list[str],
) -> list[tuple[int, str]]:
    try:
        leaves = use_leaves(values[index + 1 : end])
    except (IndexError, RecursionError):
        return [(line, "could not parse use tree")]
    failures: list[tuple[int, str]] = []
    if len({prefix for prefix, _ in leaves}) > 1:
        failures.append((line, "one use statement imports from multiple modules"))
    public = reexport(values, index)
    for prefix, name in leaves:
        if name == "*" and not _wildcard_allowed(prefix, public, scopes):
            failures.append((line, "replace wildcard import with explicit names"))
    return failures


def _forbidden_pattern_violations(source: str) -> list[tuple[int, str]]:
    failures: list[tuple[int, str]] = []
    for pattern, reason in FORBIDDEN_PATTERNS:
        for match in pattern.finditer(source):
            line = source.count("\n", 0, match.start()) + 1
            failures.append((line, reason))
    return failures


def violations(source: str) -> list[tuple[int, str]]:
    lexed = tokens(source)
    values = [token.value for token in lexed]
    inline_openings = _inline_mod_openings(values)
    scopes: list[str] = []
    failures: list[tuple[int, str]] = []
    index = 0
    while index < len(values):
        value = values[index]
        if value == "use":
            end = _find_use_end(values, index + 1)
            failures.extend(_use_violations(lexed[index].line, values, index, end, scopes))
            index = end + 1
            continue
        if value == "{":
            scopes.append("module" if index in inline_openings else "block")
        elif value == "}" and scopes:
            scopes.pop()
        index += 1

    failures.extend(_forbidden_pattern_violations(source))
    return sorted(set(failures))


def rust_files(root: Path) -> list[Path]:
    crates = root / "crates"
    return sorted(
        path
        for path in crates.rglob("*.rs")
        if not {"target", "vendor"}.intersection(path.parts)
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "root",
        type=Path,
        nargs="?",
        default=Path(__file__).resolve().parents[1],
    )
    args = parser.parse_args()
    root = args.root.resolve()
    files = rust_files(root)
    if not files:
        parser.error(f"no Rust source files found beneath {root}")
    failures: list[str] = []
    for path in files:
        for line, message in violations(path.read_text()):
            failures.append(f"{path.relative_to(root)}:{line}: {message}")
    if failures:
        print("\n".join(failures), file=sys.stderr)
        return 1
    print(f"Operator Rust structure check passed ({len(files)} files)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
