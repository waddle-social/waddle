#!/usr/bin/env python3
"""Classify Rust source regions for the #2046 textual inventory.

Adapted from the historical W1.9 classifier; it is not a Rust compiler.

A hit is TEST when any of:
  * the file sits under a `tests/`, `benches/` or `examples/` directory;
  * the file is `tests.rs`, `*_tests.rs`, `tests_*.rs`, `test_*.rs` or lives in
    a `tests/` module directory, or its module is declared behind
    `#[cfg(test)]` / `#[cfg(all(test, ...))]` in its parent;
  * the line falls inside an item (mod / fn / impl / use / struct ...) that is
    preceded by a `#[cfg(test)]`-family attribute.
A hit is TEST-SUPPORT when the gating attribute is
`#[cfg(any(test, feature = "test-support"))]` or `#[cfg(feature = "test-support")]`
(compiled into production builds only with the `test-support` feature).
Everything else is PROD.
"""

import os
import re
from functools import lru_cache

ROOT = os.getcwd()
TEST_ATTR = re.compile(
    r"#\[cfg\((test|all\(test\b[^\]]*|all\([^\]]*\btest\b[^\]]*)\)\]"
)
SUPPORT_ATTR = re.compile(
    r'#\[cfg\((any\(test, *feature *= *"test-support"\)|feature *= *"test-support"|any\(feature *= *"test-support", *test\))\)\]'
)


def strip_code(text):
    """Return text with comments/strings/char literals blanked (same length, newlines kept)."""
    out = []
    i, n = 0, len(text)
    while i < n:
        c = text[i]
        if c == "/" and i + 1 < n and text[i + 1] == "/":
            j = text.find("\n", i)
            j = n if j == -1 else j
            out.append(" " * (j - i))
            i = j
        elif c == "/" and i + 1 < n and text[i + 1] == "*":
            depth, j = 1, i + 2
            while j < n and depth:
                if text.startswith("/*", j):
                    depth += 1
                    j += 2
                elif text.startswith("*/", j):
                    depth -= 1
                    j += 2
                else:
                    j += 1
            out.append("".join(ch if ch == "\n" else " " for ch in text[i:j]))
            i = j
        elif (
            c == "r"
            and re.match(r'r#*"', text[i : i + 10])
            and (i == 0 or not (text[i - 1].isalnum() or text[i - 1] == "_"))
        ):
            m = re.match(r'r(#*)"', text[i:])
            hashes = m.group(1)
            end = text.find('"' + hashes, i + len(m.group(0)))
            end = n if end == -1 else end + 1 + len(hashes)
            out.append("".join(ch if ch == "\n" else " " for ch in text[i:end]))
            i = end
        elif c == '"':
            j = i + 1
            while j < n and text[j] != '"':
                j += 2 if text[j] == "\\" else 1
            j += 1
            out.append("".join(ch if ch == "\n" else " " for ch in text[i:j]))
            i = j
        elif c == "'":
            if i + 1 < n and text[i + 1] == "\\":
                j = text.find("'", i + 2)
                j = n if j == -1 else j + 1
                out.append(" " * (j - i))
                i = j
            elif i + 2 < n and text[i + 2] == "'":
                out.append("   ")
                i += 3
            else:
                out.append(c)
                i += 1
        else:
            out.append(c)
            i += 1
    return "".join(out)


def item_extent(code, start):
    """From offset `start` (just after attributes), return end offset of the item."""
    n = len(code)
    i = start
    depth_paren = 0
    while i < n:
        c = code[i]
        if c in "([":
            depth_paren += 1
        elif c in ")]":
            depth_paren -= 1
        elif c == ";" and depth_paren == 0:
            return i + 1
        elif c == "{" and depth_paren == 0:
            depth = 1
            i += 1
            while i < n and depth:
                if code[i] == "{":
                    depth += 1
                elif code[i] == "}":
                    depth -= 1
                i += 1
            return i
        i += 1
    return n


@lru_cache(maxsize=None)
def regions(path):
    """List of (start_line, end_line, kind) gated ranges in file."""
    try:
        text = open(path, encoding="utf-8", errors="replace").read()
    except OSError:
        return ()
    code = strip_code(text)
    res = []
    for m in re.finditer(r"#\[cfg\([^\n]*\)\]", code):
        attr = text[m.start() : m.end()]
        if SUPPORT_ATTR.fullmatch(attr):
            kind = "test-support"
        elif TEST_ATTR.fullmatch(attr):
            kind = "test"
        else:
            continue
        # skip further attributes / whitespace
        j = m.end()
        while True:
            k = j
            while k < len(code) and code[k] in " \t\r\n":
                k += 1
            if code.startswith("#[", k):
                d, k2 = 0, k + 1
                while k2 < len(code):
                    if code[k2] == "[":
                        d += 1
                    elif code[k2] == "]":
                        d -= 1
                        if d == 0:
                            break
                    k2 += 1
                j = k2 + 1
                continue
            j = k
            break
        end = item_extent(code, j)
        sl = text.count("\n", 0, m.start()) + 1
        el = text.count("\n", 0, end) + 1
        res.append((sl, el, kind))
    return tuple(res)


@lru_cache(maxsize=None)
def gated_module_files():
    """Files whose `mod x;` declaration is cfg(test)/test-support gated."""
    out = {}
    for base, _dirs, files in os.walk(os.path.join(ROOT, "server", "crates")):
        for f in files:
            if not f.endswith(".rs"):
                continue
            p = os.path.join(base, f)
            text = open(p, encoding="utf-8", errors="replace").read()
            for m in re.finditer(
                r"(#\[cfg\([^\n]*\)\])\s*(?:#\[[^\n]*\]\s*)*(?:pub(?:\([a-z]+\))? )?mod (\w+);",
                text,
            ):
                attr = m.group(1)
                kind = (
                    "test-support"
                    if SUPPORT_ATTR.fullmatch(attr)
                    else ("test" if TEST_ATTR.fullmatch(attr) else None)
                )
                if not kind:
                    continue
                name = m.group(2)
                stem = f[:-3]
                d = base if stem in ("mod", "lib", "main") else os.path.join(base, stem)
                for cand in (
                    os.path.join(d, name + ".rs"),
                    os.path.join(d, name, "mod.rs"),
                ):
                    out[os.path.normpath(cand)] = kind
                # whole subtree of name/ also gated
                out[os.path.normpath(os.path.join(d, name)) + os.sep] = kind
    return out


def file_kind(path):
    ap = os.path.normpath(os.path.join(ROOT, path))
    parts = path.split("/")
    if any(p in ("tests", "benches", "examples") for p in parts[:-1]):
        return "test"
    fname = parts[-1]
    if (
        fname == "tests.rs"
        or fname.endswith("_tests.rs")
        or fname.startswith("tests_")
        or fname.startswith("test_")
    ):
        return "test"
    g = gated_module_files()
    if ap in g:
        return g[ap]
    for k, v in g.items():
        if k.endswith(os.sep) and ap.startswith(k):
            return v
    return "prod"


def classify(path, line):
    fk = file_kind(path)
    if fk != "prod":
        return fk
    best = "prod"
    for sl, el, kind in regions(os.path.join(ROOT, path)):
        if sl <= line <= el:
            if kind == "test":
                return "test"
            best = kind
    return best
