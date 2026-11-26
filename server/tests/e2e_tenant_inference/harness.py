"""Deterministic, zero-dependency Python 3.12 stdlib E2E test harness.

Provides structured TestCase / TestResult / SuiteRunner abstractions, memoized
subprocess helpers for cargo/quint/verus/pobserve, isolated TemporaryDirectory
helpers, and strict fail-closed contract assertions (zero fake passes, zero
skips-as-pass).
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import tempfile
import time
from contextlib import contextmanager
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Callable, Iterator, Sequence

E2E_DIR = Path(__file__).resolve().parent
SERVER_DIR = E2E_DIR.parent.parent
REPO_ROOT = SERVER_DIR.parent
HOME_LOCAL_BIN = Path.home() / ".local" / "bin"


class AssertionFailure(Exception):
    """Raised when an E2E test assertion fails."""


@dataclass(frozen=True)
class CmdResult:
    cmd: tuple[str, ...]
    cwd: str
    returncode: int
    stdout: str
    stderr: str
    duration_ms: int
    timed_out: bool = False

    @property
    def combined(self) -> str:
        return f"{self.stdout}\n{self.stderr}"


@dataclass(frozen=True)
class TestCase:
    test_id: str
    tier: int
    milestone: str
    feature_ids: tuple[str, ...]
    gate_ids: tuple[str, ...]
    description: str
    fn: Callable[["RunContext"], None]


@dataclass
class TestResult:
    test_id: str
    tier: int
    milestone: str
    feature_ids: list[str]
    gate_ids: list[str]
    description: str
    passed: bool
    duration_ms: int
    error: str | None = None


def fail(msg: str) -> None:
    raise AssertionFailure(msg)


def assert_true(cond: bool, msg: str) -> None:
    if not cond:
        raise AssertionFailure(msg)


def assert_eq(actual: object, expected: object, msg: str = "") -> None:
    if actual != expected:
        prefix = f"{msg}: " if msg else ""
        raise AssertionFailure(f"{prefix}expected {expected!r}, got {actual!r}")


def assert_in(needle: str, haystack: str, msg: str = "") -> None:
    if needle not in haystack:
        prefix = f"{msg}: " if msg else ""
        raise AssertionFailure(f"{prefix}expected substring {needle!r} not found")


def assert_not_in(needle: str, haystack: str, msg: str = "") -> None:
    if needle in haystack:
        prefix = f"{msg}: " if msg else ""
        raise AssertionFailure(f"{prefix}forbidden substring {needle!r} was found")


def assert_file_exists(path: Path, msg: str = "") -> Path:
    if not path.is_file():
        prefix = f"{msg}: " if msg else ""
        rel = _rel(path)
        raise AssertionFailure(f"{prefix}missing required file {rel}")
    return path


def assert_dir_exists(path: Path, msg: str = "") -> Path:
    if not path.is_dir():
        prefix = f"{msg}: " if msg else ""
        rel = _rel(path)
        raise AssertionFailure(f"{prefix}missing required directory {rel}")
    return path


def assert_contains_regex(pattern: str, text: str, msg: str = "", flags: int = 0) -> None:
    if re.search(pattern, text, flags) is None:
        prefix = f"{msg}: " if msg else ""
        raise AssertionFailure(f"{prefix}required regex {pattern!r} did not match")


def assert_not_contains_regex(pattern: str, text: str, msg: str = "", flags: int = 0) -> None:
    match = re.search(pattern, text, flags)
    if match is not None:
        prefix = f"{msg}: " if msg else ""
        raise AssertionFailure(
            f"{prefix}forbidden regex {pattern!r} matched {match.group(0)!r}"
        )


_C_STYLE_TOKEN_RE = re.compile(r'"(?:\\.|[^"\\])*"|//[^\n]*|/\*[\s\S]*?\*/')
_HASH_STYLE_TOKEN_RE = re.compile(r'"(?:\\.|[^"\\])*"|\'(?:\\.|[^\'\\])*\'|#[^\n]*')
_HELM_COMMENT_RE = re.compile(r"\{\{-?\s*/\*[\s\S]*?\*/\s*-?\}\}")


def strip_comments(text: str, style: str = "rust") -> str:
    """Strip comments while preserving quoted string literals.

    Supported styles:
      - ``"rust"`` / ``"wit"``: ``// ...`` and ``/* ... */``
      - ``"hash"``: ``# ...`` (plus Helm ``{{/* ... */}}`` template comments)
    """
    if style in ("rust", "wit"):
        def _repl_c(m: re.Match[str]) -> str:
            tok = m.group(0)
            if tok.startswith('"'):
                return tok
            return " " if tok.startswith("/*") else ""

        return _C_STYLE_TOKEN_RE.sub(_repl_c, text)
    if style == "hash":
        no_helm = _HELM_COMMENT_RE.sub(" ", text)

        def _repl_h(m: re.Match[str]) -> str:
            tok = m.group(0)
            if tok.startswith(('"', "'")):
                return tok
            return ""

        return _HASH_STYLE_TOKEN_RE.sub(_repl_h, no_helm)
    raise ValueError(f"unsupported comment style: {style!r}")


def extract_brace_block(text: str, header_pattern: str) -> str:
    """Extract a full brace-balanced block starting from ``header_pattern``."""
    match = re.search(header_pattern, text)
    if match is None:
        fail(f"could not locate block matching {header_pattern!r}")
    open_idx = text.find("{", match.start())
    if open_idx == -1:
        fail(f"missing opening '{{' for block matching {header_pattern!r}")
    depth = 0
    in_string = False
    escape = False
    for idx in range(open_idx, len(text)):
        ch = text[idx]
        if in_string:
            if escape:
                escape = False
            elif ch == "\\":
                escape = True
            elif ch == '"':
                in_string = False
            continue
        if ch == '"':
            in_string = True
        elif ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
            if depth == 0:
                return text[match.start() : idx + 1]
    fail(f"unbalanced braces in block matching {header_pattern!r}")
    return ""


def assert_cargo_tests_passed(
    res: CmdResult,
    label: str,
    min_passed: int = 1,
    required_test_prefix: str | None = None,
) -> int:
    """Verify a cargo test invocation succeeded and executed >= min_passed tests."""
    if res.timed_out:
        fail(f"TIMEOUT for {label}:\n{res.combined[-1200:]}")
    if res.returncode != 0:
        fail(f"{label} failed (exit {res.returncode}):\n{res.combined[-1200:]}")
    passed_counts = [
        int(m.group(1))
        for m in re.finditer(r"test result: ok\.\s*(\d+)\s+passed;\s*0 failed", res.combined)
    ]
    total_passed = sum(passed_counts)
    if total_passed < min_passed:
        fail(
            f"ZERO_TESTS_EXECUTED for {label}: expected >= {min_passed} passed, "
            f"got {total_passed} (refusing 0-passed fake pass)"
        )
    if required_test_prefix is not None:
        prefix_matches = re.findall(
            rf"^test\s+{re.escape(required_test_prefix)}\S*\s+\.\.\.\s+ok$",
            res.combined,
            re.MULTILINE,
        )
        if len(prefix_matches) < min_passed:
            fail(
                f"ZERO_TESTS_EXECUTED for {label}: expected >= {min_passed} passing tests "
                f"matching prefix {required_test_prefix!r}, got {len(prefix_matches)}"
            )
        return len(prefix_matches)
    return total_passed


def _rel(path: Path) -> str:
    try:
        return str(path.relative_to(REPO_ROOT))
    except ValueError:
        return str(path)


class RunContext:
    """Shared per-run execution context with memoized subprocess cache."""

    def __init__(self, all_cases: Sequence[TestCase] = ()) -> None:
        self.repo_root = REPO_ROOT
        self.server_dir = SERVER_DIR
        self.e2e_dir = E2E_DIR
        self.all_cases: tuple[TestCase, ...] = tuple(all_cases)
        self._cmd_cache: dict[tuple[tuple[str, ...], str, tuple[tuple[str, str], ...]], CmdResult] = {}
        self._pobserve_cache: CmdResult | None = None

    def _base_env(self, overrides: dict[str, str] | None = None) -> dict[str, str]:
        env = os.environ.copy()
        local_bin = str(HOME_LOCAL_BIN)
        path = env.get("PATH", "")
        if local_bin not in path.split(os.pathsep):
            env["PATH"] = f"{local_bin}{os.pathsep}{path}" if path else local_bin
        if overrides:
            env.update(overrides)
        return env

    def run_cmd(
        self,
        cmd: Sequence[str],
        *,
        cwd: Path | None = None,
        timeout: int = 120,
        env_overrides: dict[str, str] | None = None,
        cache: bool = True,
    ) -> CmdResult:
        cmd_tuple = tuple(str(c) for c in cmd)
        cwd_path = cwd or self.repo_root
        env_key = tuple(sorted((env_overrides or {}).items()))
        cache_key = (cmd_tuple, str(cwd_path), env_key)
        if cache and cache_key in self._cmd_cache:
            return self._cmd_cache[cache_key]

        env = self._base_env(env_overrides)
        t0 = time.monotonic()
        try:
            proc = subprocess.run(
                cmd_tuple,
                cwd=str(cwd_path),
                env=env,
                capture_output=True,
                text=True,
                timeout=timeout,
            )
            res = CmdResult(
                cmd=cmd_tuple,
                cwd=str(cwd_path),
                returncode=proc.returncode,
                stdout=proc.stdout,
                stderr=proc.stderr,
                duration_ms=int((time.monotonic() - t0) * 1000),
                timed_out=False,
            )
        except subprocess.TimeoutExpired as exc:
            stdout = exc.stdout.decode("utf-8", errors="replace") if isinstance(exc.stdout, bytes) else (exc.stdout or "")
            stderr = exc.stderr.decode("utf-8", errors="replace") if isinstance(exc.stderr, bytes) else (exc.stderr or "")
            res = CmdResult(
                cmd=cmd_tuple,
                cwd=str(cwd_path),
                returncode=124,
                stdout=stdout,
                stderr=f"TIMEOUT after {timeout}s\n{stderr}",
                duration_ms=int((time.monotonic() - t0) * 1000),
                timed_out=True,
            )
        except FileNotFoundError as exc:
            res = CmdResult(
                cmd=cmd_tuple,
                cwd=str(cwd_path),
                returncode=127,
                stdout="",
                stderr=f"Executable not found: {exc}",
                duration_ms=int((time.monotonic() - t0) * 1000),
                timed_out=False,
            )

        if cache and not res.timed_out:
            self._cmd_cache[cache_key] = res
        return res

    def run_cargo_test(
        self,
        args: Sequence[str],
        *,
        cwd: Path | None = None,
        timeout: int = 360,
        cache: bool = True,
    ) -> CmdResult:
        # Guardrail: never run `cargo test --lib` (or unfiltered `cargo test`) on waddle-server
        # (6 GB RAM cliff on 7.8 GiB VM).
        args_list = [str(a) for a in args]
        if "waddle-server" in args_list and ("--lib" in args_list or "--test" not in args_list):
            fail(
                "FORBIDDEN: `cargo test -p waddle-server` without `--test <binary>` (or with `--lib`) "
                "exceeds the 7.8 GiB VM memory cliff. Use targeted `--test <binary>` instead."
            )
        return self.run_cmd(
            ["cargo", "test", *args_list],
            cwd=cwd or self.server_dir,
            timeout=timeout,
            env_overrides={
                "RUSTUP_TOOLCHAIN": "1.98.1",
                "CARGO_BUILD_JOBS": "2",
            },
            cache=cache,
        )

    def run_quint(
        self,
        args: Sequence[str],
        *,
        cwd: Path | None = None,
        timeout: int = 180,
        cache: bool = True,
    ) -> CmdResult:
        quint_bin = str(HOME_LOCAL_BIN / "quint") if (HOME_LOCAL_BIN / "quint").exists() else "quint"
        return self.run_cmd(
            [quint_bin, *[str(a) for a in args]],
            cwd=cwd or self.repo_root,
            timeout=timeout,
            cache=cache,
        )

    def run_verus(
        self,
        proof_path: Path,
        *,
        cwd: Path | None = None,
        timeout: int = 180,
        cache: bool = True,
    ) -> CmdResult:
        # Note: $HOME/.local/bin/verus wrapper sets RUSTUP_TOOLCHAIN=1.100.0 internally.
        verus_bin = str(HOME_LOCAL_BIN / "verus") if (HOME_LOCAL_BIN / "verus").exists() else "verus"
        return self.run_cmd(
            [verus_bin, str(proof_path)],
            cwd=cwd or self.repo_root,
            timeout=timeout,
            cache=cache,
        )

    def run_pobserve_compile(
        self,
        pproj_path: Path | None = None,
        *,
        cache: bool = True,
        timeout: int = 180,
    ) -> CmdResult:
        if cache and pproj_path is None and self._pobserve_cache is not None:
            return self._pobserve_cache
        proj = pproj_path or (self.server_dir / "specs" / "pobserve" / "WaddleMonitors.pproj")
        p_bin = str(HOME_LOCAL_BIN / "p") if (HOME_LOCAL_BIN / "p").exists() else "p"
        with self.temp_dir(prefix="waddle_pobserve_") as out_dir:
            res = self.run_cmd(
                [p_bin, "compile", "--mode", "pobserve", "-pp", str(proj), "-o", str(out_dir)],
                cwd=self.repo_root,
                timeout=timeout,
                cache=False,
            )
            generated_count = sum(1 for _ in out_dir.rglob("*") if _.is_file())
            if res.returncode == 0 and generated_count == 0:
                res = CmdResult(
                    cmd=res.cmd,
                    cwd=res.cwd,
                    returncode=1,
                    stdout=res.stdout,
                    stderr=f"{res.stderr}\nPObserve produced 0 files in {out_dir}",
                    duration_ms=res.duration_ms,
                )
        if cache and pproj_path is None:
            self._pobserve_cache = res
        return res

    @contextmanager
    def temp_dir(self, prefix: str = "waddle_e2e_") -> Iterator[Path]:
        with tempfile.TemporaryDirectory(prefix=prefix) as d:
            yield Path(d)

    def read_text(self, rel_path: str | Path) -> str:
        p = rel_path if isinstance(rel_path, Path) else (self.repo_root / rel_path)
        assert_file_exists(p)
        return p.read_text(encoding="utf-8")

    def require_crate_exists(self, crate_name: str, reason: str) -> Path:
        crate_dir = self.server_dir / "crates" / crate_name
        cargo_toml = crate_dir / "Cargo.toml"
        if not cargo_toml.is_file():
            fail(f"MISSING_CRATE `{crate_name}` ({_rel(cargo_toml)}): {reason}")
        return crate_dir

    def require_server_integration_test(
        self,
        test_stem: str,
        reason: str,
        *,
        min_passed: int = 1,
    ) -> Path:
        test_file = self.server_dir / "crates" / "waddle-server" / "tests" / f"{test_stem}.rs"
        if not test_file.is_file():
            fail(f"MISSING_TARGET `{_rel(test_file)}`: {reason}")
        res = self.run_cargo_test(["-p", "waddle-server", "--test", test_stem])
        assert_cargo_tests_passed(
            res,
            f"cargo test -p waddle-server --test {test_stem} ({reason})",
            min_passed=min_passed,
        )
        return test_file

    def require_crate_tests_pass(
        self,
        crate_name: str,
        reason: str,
        *,
        test_filter: str | None = None,
        min_passed: int = 1,
    ) -> CmdResult:
        crate_dir = self.require_crate_exists(crate_name, reason)
        if test_filter:
            has_match = any(
                test_filter in p.read_text(encoding="utf-8", errors="ignore")
                for p in crate_dir.rglob("*.rs")
            )
            if not has_match:
                fail(
                    f"MISSING_TEST_FILTER `{test_filter}` in crate `{crate_name}`: {reason}"
                )
        args = ["-p", crate_name]
        if test_filter:
            args.append(test_filter)
        res = self.run_cargo_test(args)
        assert_cargo_tests_passed(
            res,
            f"cargo test {' '.join(args)} ({reason})",
            min_passed=min_passed,
        )
        return res


class SuiteRunner:
    """Filters, executes, and summarizes TestCase collections."""

    def __init__(self, cases: Sequence[TestCase]) -> None:
        self.cases = list(cases)

    def filter_cases(
        self,
        *,
        tier: str = "all",
        milestone: str = "all",
    ) -> list[TestCase]:
        selected: list[TestCase] = []
        tier_norm = tier.lower()
        ms_norm = milestone.lower()
        for tc in self.cases:
            if tier_norm != "all" and str(tc.tier) != tier_norm:
                continue
            if ms_norm != "all" and tc.milestone.lower() != ms_norm:
                continue
            selected.append(tc)
        return selected

    def run(
        self,
        selected: Sequence[TestCase],
        *,
        fail_fast: bool = False,
        verbose: bool = False,
    ) -> list[TestResult]:
        ctx = RunContext(all_cases=self.cases)
        results: list[TestResult] = []
        for tc in selected:
            t0 = time.monotonic()
            passed = True
            err_msg: str | None = None
            try:
                tc.fn(ctx)
            except AssertionFailure as exc:
                passed = False
                err_msg = str(exc)
            except Exception as exc:  # noqa: BLE001
                passed = False
                err_msg = f"Unexpected {type(exc).__name__}: {exc}"
            dur_ms = int((time.monotonic() - t0) * 1000)
            res = TestResult(
                test_id=tc.test_id,
                tier=tc.tier,
                milestone=tc.milestone,
                feature_ids=list(tc.feature_ids),
                gate_ids=list(tc.gate_ids),
                description=tc.description,
                passed=passed,
                duration_ms=dur_ms,
                error=err_msg,
            )
            results.append(res)
            if verbose:
                status = "PASS" if passed else "FAIL"
                print(
                    f"[{status}] {tc.test_id} (T{tc.tier}/{tc.milestone}/"
                    f"{','.join(tc.feature_ids)}) {dur_ms}ms — {tc.description}",
                    flush=True,
                )
                if not passed and err_msg:
                    for line in err_msg.splitlines()[:6]:
                        print(f"       {line}", flush=True)
            if fail_fast and not passed:
                break
        return results

    @staticmethod
    def write_json(results: Sequence[TestResult], output_path: Path) -> None:
        passed = sum(1 for r in results if r.passed)
        failed = len(results) - passed
        payload = {
            "total": len(results),
            "passed": passed,
            "failed": failed,
            "results": [asdict(r) for r in results],
        }
        output_path.parent.mkdir(parents=True, exist_ok=True)
        output_path.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")

    @staticmethod
    def print_summary(results: Sequence[TestResult], *, verbose: bool = False) -> None:
        total = len(results)
        passed = sum(1 for r in results if r.passed)
        failed = total - passed
        total_ms = sum(r.duration_ms for r in results)

        print("\n=== E2E Summary by Tier ===")
        print(f"{'Tier':<8} {'Total':>6} {'PASS':>6} {'FAIL':>6}")
        for t in (1, 2, 3, 4):
            tr = [r for r in results if r.tier == t]
            if not tr:
                continue
            tp = sum(1 for r in tr if r.passed)
            tf = len(tr) - tp
            print(f"Tier {t:<3} {len(tr):>6} {tp:>6} {tf:>6}")

        print("\n=== E2E Summary by Milestone ===")
        print(f"{'Milestone':<10} {'Total':>6} {'PASS':>6} {'FAIL':>6}")
        order = ["M1", "M2", "M3", "M4", "M5", "M6", "E2E"]
        seen = {r.milestone for r in results}
        for ms in order + sorted(seen - set(order)):
            mr = [r for r in results if r.milestone == ms]
            if not mr:
                continue
            mp = sum(1 for r in mr if r.passed)
            mf = len(mr) - mp
            print(f"{ms:<10} {len(mr):>6} {mp:>6} {mf:>6}")

        if failed > 0 and not verbose:
            print("\n=== Failure Diagnostics ===")
            for r in results:
                if not r.passed:
                    first_line = (r.error or "unknown error").splitlines()[0]
                    print(f"  FAIL {r.test_id} [{r.milestone}/T{r.tier}]: {first_line}")

        print(
            f"\nTOTAL: {total} tests | PASS: {passed} | FAIL: {failed} | "
            f"Duration: {total_ms / 1000:.2f}s"
        )
