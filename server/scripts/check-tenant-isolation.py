#!/usr/bin/env python3
"""Enforce structural tenant-isolation invariants across Waddle server crates.

Modeled on `check-ingress-typed-entry.py`, this lexical architecture guard
rejects four categories of tenant-isolation regressions in non-test production
code:

1. `hardcoded-local-waddle`: `WaddleId::try_new("local")` or
   `WaddleId::try_new("room-observation")` outside tests.
2. `unscoped-store-ctor`: direct calls to unscoped registry/store constructors
   (`RoomRegistryActor::new(`, `CommandRegistry::new(`, `ConnectionRegistry::new(`,
   `TursoTenantStore::open_raw(`) in `crates/waddle-server/src/db` or
   `crates/waddle-server/src/server/routes`, unless listed with an exact count
   in `scripts/tenant-isolation-allowlist.txt`.
3. `global-barejid-static`: process-global `static` `HashMap<BareJid` or
   `DashMap<BareJid` maps in `waddle-server` or `waddle-xmpp`.
4. `unbounded-wasm-memories`: direct `StoreLimitsBuilder::new()` outside
   `AggregateStoreLimits` in `crates/waddle-extensions/src/runtime/`, or
   removal of `epoch_interruption(true)` from `runtime/loader.rs`.

Run with a repository root (or server root), or from `server/` with no argument.
"""
from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

ALLOWLIST = Path("scripts/tenant-isolation-allowlist.txt")

HARDCODED_LOCAL_WADDLE = "hardcoded-local-waddle"
UNSCOPED_STORE_CTOR = "unscoped-store-ctor"
GLOBAL_BAREJID_STATIC = "global-barejid-static"
UNBOUNDED_WASM_MEMORIES = "unbounded-wasm-memories"

KINDS = (
    HARDCODED_LOCAL_WADDLE,
    UNSCOPED_STORE_CTOR,
    GLOBAL_BAREJID_STATIC,
    UNBOUNDED_WASM_MEMORIES,
)

TEST_FILE = re.compile(
    r"(?:^|/)(?:tests|\w*_tests|test_support|test_state|\w*_test_hooks|observer_test_support)(?:\.rs$|/)"
)
TEST_MODULE = re.compile(r"#\[cfg\(test\)\]\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+\w+\s*\{")

COMMENTS_ONLY = re.compile(r"//[^\n]*|/\*.*?\*/", re.DOTALL)
NON_CODE = re.compile(
    r"""//[^\n]*
      | /\*.*?\*/
      | (?:b|c|br|cr)?r(?P<hashes>\#{0,255})".*?"(?P=hashes)
      | (?:b|c)?"(?:\\.|[^"\\])*"
      | b?'(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|.)|[^'\\\n])'
    """,
    re.DOTALL | re.VERBOSE,
)

HARDCODED_WADDLE_RE = re.compile(
    r'WaddleId\s*::\s*try_new\s*\(\s*"(?:local|room-observation)"\s*\)'
)
UNSCOPED_CTOR_RE = re.compile(
    r"\b(?:RoomRegistryActor|CommandRegistry|ConnectionRegistry)\s*::\s*new\s*\("
    r"|\bTursoTenantStore\s*::\s*open_raw\s*\("
)
GLOBAL_BAREJID_STATIC_RE = re.compile(
    r"(?<!')\bstatic\s+[A-Za-z_]\w*\s*:[^=;]*\b(?:HashMap|DashMap)\s*<\s*BareJid\b",
    re.DOTALL,
)
STORE_LIMITS_BUILDER_RE = re.compile(r"\bStoreLimitsBuilder\s*::\s*new\s*\(")
EPOCH_INTERRUPTION_RE = re.compile(r"\.\s*epoch_interruption\s*\(\s*true\s*\)")


def blank(text: str) -> str:
    return re.sub(r"[^\n]", " ", text)


def strip_comments(source: str) -> str:
    """Blank comments while preserving string literals and newlines."""
    return COMMENTS_ONLY.sub(lambda match: blank(match[0]), source)


def code_only(source: str) -> str:
    """Blank comments and literals while preserving newlines."""
    return NON_CODE.sub(lambda match: blank(match[0]), source)


def without_test_modules(code: str) -> str:
    """Blank every `#[cfg(test)] mod name { ... }` block."""
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


def find_hardcoded_local_waddle(source: str) -> list[int]:
    """Return 1-based line numbers of hardcoded local/room-observation WaddleIds."""
    cleaned = without_test_modules(strip_comments(source))
    return [
        cleaned.count("\n", 0, m.start()) + 1
        for m in HARDCODED_WADDLE_RE.finditer(cleaned)
    ]


def find_unscoped_store_ctors(source: str) -> list[int]:
    """Return 1-based line numbers of unscoped registry/store constructor calls."""
    cleaned = without_test_modules(code_only(source))
    return [
        cleaned.count("\n", 0, m.start()) + 1
        for m in UNSCOPED_CTOR_RE.finditer(cleaned)
    ]


def find_global_barejid_statics(source: str) -> list[int]:
    """Return 1-based line numbers of static HashMap/DashMap keyed by bare BareJid."""
    cleaned = without_test_modules(code_only(source))
    return [
        cleaned.count("\n", 0, m.start()) + 1
        for m in GLOBAL_BAREJID_STATIC_RE.finditer(cleaned)
    ]


def find_unbounded_wasm_memories(source: str, relative: Path) -> list[int]:
    """Verify StoreLimitsBuilder is only inside AggregateStoreLimits and epoch_interruption is enabled."""
    cleaned = without_test_modules(code_only(source))
    violations: list[int] = []
    rel_posix = relative.as_posix()
    for m in STORE_LIMITS_BUILDER_RE.finditer(cleaned):
        # Allowed strictly inside AggregateStoreLimits in host_state.rs.
        if rel_posix.endswith("runtime/host_state.rs") and "struct AggregateStoreLimits" in cleaned:
            continue
        violations.append(cleaned.count("\n", 0, m.start()) + 1)
    if rel_posix.endswith("runtime/loader.rs") and not EPOCH_INTERRUPTION_RE.search(cleaned):
        violations.append(1)
    return violations


def load_allowlist(path: Path) -> dict[tuple[str, str], int]:
    entries: dict[tuple[str, str], int] = {}
    if not path.exists():
        return entries
    for lineno, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split()
        if len(parts) != 3 or parts[1] not in KINDS:
            raise ValueError(f"{path}:{lineno}: invalid allowlist entry: {raw!r}")
        key = (parts[0], parts[1])
        entries[key] = int(parts[2])
    return entries


def resolve_server_root(root: Path) -> Path:
    if (root / "crates/waddle-server").is_dir():
        return root
    if (root / "server/crates/waddle-server").is_dir():
        return root / "server"
    raise FileNotFoundError(f"could not locate server crates under {root}")


def check_repo(root: Path) -> tuple[int, list[str]]:
    server_root = resolve_server_root(root)
    allowlist = load_allowlist(server_root / ALLOWLIST)
    observed: dict[tuple[str, str], list[int]] = {}
    scanned_files: set[Path] = set()

    def scan_tree(rel_dir: Path, kind: str, finder) -> None:
        base = server_root / rel_dir
        if not base.is_dir():
            return
        for path in sorted(base.rglob("*.rs")):
            rel = path.relative_to(server_root)
            if is_test_file(rel):
                continue
            scanned_files.add(rel)
            source = path.read_text(encoding="utf-8")
            lines = finder(source, rel) if kind == UNBOUNDED_WASM_MEMORIES else finder(source)
            if lines:
                observed[(rel.as_posix(), kind)] = lines

    for crate_src in (
        Path("crates/waddle-extensions/src"),
        Path("crates/waddle-xmpp/src"),
        Path("crates/waddle-server/src"),
    ):
        scan_tree(crate_src, HARDCODED_LOCAL_WADDLE, find_hardcoded_local_waddle)

    for scoped_dir in (
        Path("crates/waddle-server/src/db"),
        Path("crates/waddle-server/src/server/routes"),
    ):
        scan_tree(scoped_dir, UNSCOPED_STORE_CTOR, find_unscoped_store_ctors)

    for static_src in (
        Path("crates/waddle-server/src"),
        Path("crates/waddle-xmpp/src"),
    ):
        scan_tree(static_src, GLOBAL_BAREJID_STATIC, find_global_barejid_statics)

    scan_tree(
        Path("crates/waddle-extensions/src/runtime"),
        UNBOUNDED_WASM_MEMORIES,
        find_unbounded_wasm_memories,
    )

    errors: list[str] = []
    for key, lines in sorted(observed.items()):
        expected = allowlist.get(key, 0)
        if len(lines) != expected:
            rel_path, kind = key
            errors.append(
                f"{rel_path}: {kind}: found {len(lines)} (lines {lines}), allowlist expects {expected}"
            )

    for key, expected in sorted(allowlist.items()):
        if key not in observed and expected > 0:
            rel_path, kind = key
            errors.append(
                f"{rel_path}: {kind}: stale allowlist entry expects {expected}, found 0"
            )

    return len(scanned_files), errors


def run_self_test() -> int:
    """Verify each isolation detector triggers on synthetic negative boundary probes."""
    import tempfile

    with tempfile.TemporaryDirectory(prefix="waddle_tenant_guard_selftest_") as tmp:
        root = Path(tmp)
        srv = root / "crates" / "waddle-server" / "src" / "server" / "routes"
        db_dir = root / "crates" / "waddle-server" / "src" / "db"
        ext_rt = root / "crates" / "waddle-extensions" / "src" / "runtime"
        xmpp_src = root / "crates" / "waddle-xmpp" / "src"
        scripts_dir = root / "scripts"
        for d in (srv, db_dir, ext_rt, xmpp_src, scripts_dir):
            d.mkdir(parents=True, exist_ok=True)
        (scripts_dir / "tenant-isolation-allowlist.txt").write_text("", encoding="utf-8")

        # 1. Negative probe: hardcoded local waddle id
        (xmpp_src / "probe_local.rs").write_text(
            'fn probe() { let _ = WaddleId::try_new("local"); }\n', encoding="utf-8"
        )
        # 2. Negative probe: unscoped store/registry constructors
        (db_dir / "probe_ctor.rs").write_text(
            "fn probe() { let _ = RoomRegistryActor::new(); let _ = TursoTenantStore::open_raw(); }\n",
            encoding="utf-8",
        )
        (srv / "probe_route.rs").write_text(
            "fn probe() { let _ = ConnectionRegistry::new(); }\n", encoding="utf-8"
        )
        # 3. Negative probe: global BareJid static map
        (xmpp_src / "probe_static.rs").write_text(
            "static GLOBAL_SESSIONS: DashMap<BareJid, String> = DashMap::new();\n",
            encoding="utf-8",
        )
        # 4. Negative probe: unbounded StoreLimitsBuilder + missing epoch_interruption
        (ext_rt / "bad_limits.rs").write_text(
            "fn probe() { let _ = StoreLimitsBuilder::new(); }\n", encoding="utf-8"
        )
        (ext_rt / "loader.rs").write_text(
            "fn engine() { let _cfg = wasmtime::Config::new(); }\n", encoding="utf-8"
        )

        _, errors = check_repo(root)
        error_blob = "\n".join(errors)
        expected_fragments = (
            f"probe_local.rs: {HARDCODED_LOCAL_WADDLE}:",
            f"probe_ctor.rs: {UNSCOPED_STORE_CTOR}:",
            f"probe_route.rs: {UNSCOPED_STORE_CTOR}:",
            f"probe_static.rs: {GLOBAL_BAREJID_STATIC}:",
            f"bad_limits.rs: {UNBOUNDED_WASM_MEMORIES}:",
            f"loader.rs: {UNBOUNDED_WASM_MEMORIES}:",
        )
        missing = [frag for frag in expected_fragments if frag not in error_blob]
        if missing:
            print(
                f"ERROR: self-test missed expected boundary violations: {missing}\n{error_blob}",
                file=sys.stderr,
            )
            return 1

    print("tenant isolation guard self-test passed (all 4 negative boundary classes rejected)")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "root",
        nargs="?",
        default=".",
        help="Repository root or server workspace root (default: .)",
    )
    parser.add_argument(
        "--server-only",
        action="store_true",
        help="Scope check to server crates (default behavior; accepted for script consistency)",
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="Run negative boundary self-tests in an isolated temporary directory",
    )
    args = parser.parse_args(argv)
    if args.self_test:
        return run_self_test()
    count, errors = check_repo(Path(args.root).resolve())
    if errors:
        for err in errors:
            print(f"ERROR: {err}", file=sys.stderr)
        return 1
    print(f"tenant isolation check passed ({count} files)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
