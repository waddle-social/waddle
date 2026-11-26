"""Tier 4 Real-World Application Scenarios (13 scenarios: 1 M1 + 12 Release Evidence Gates).

T4-M1-01 runs isolated negative-mutation non-vacuity probes on Verus, Quint, and
PObserve in a TemporaryDirectory and MUST PASS right now.
T4-S01..T4-S12 cover all 5 Release Evidence Gates and MUST FAIL cleanly until
their respective milestones land.
"""

from __future__ import annotations

import re
import shutil

from harness import (
    RunContext,
    TestCase,
    assert_contains_regex,
    assert_not_in,
    assert_true,
    fail,
)


# ---------------------------------------------------------------------------
# M1 Real-World Formal Verification Non-Vacuity Scenario (PASS right now)
# ---------------------------------------------------------------------------

def _assert_real_probe_failure(res, tool_name: str) -> None:
    assert_true(
        not res.timed_out,
        f"{tool_name} non-vacuity probe timed out (returncode={res.returncode})",
    )
    assert_true(
        res.returncode not in (0, 124, 127),
        f"{tool_name} non-vacuity probe returned invalid exit code {res.returncode} "
        f"(must be a real verification/compilation failure, not 0/124/127):\n{res.combined[-600:]}",
    )
    assert_not_in(
        "Executable not found",
        res.combined,
        f"{tool_name} non-vacuity probe failed because executable was missing",
    )


def _t4_m1_01_formal_non_vacuity_probes(ctx: RunContext) -> None:
    """Prove Verus, Quint, and PObserve genuinely reject injected security violations.

    All mutations occur exclusively inside an isolated TemporaryDirectory that is
    always cleaned up on exit; repository files are NEVER modified.
    """
    with ctx.temp_dir(prefix="waddle_non_vacuity_") as tmp:
        # 1. Verus negative mutation: weaken `now_ms < lease_expiry_ms` to `<=`
        verus_src = ctx.read_text("server/specs/verus/three_fence_guard.rs")
        target = "&& input.now_ms < input.lease_expiry_ms"
        if verus_src.count(target) < 2:
            fail("unexpected three_fence_guard.rs structure for Verus non-vacuity probe")
        # Mutate only the implementation in evaluate_admission (second occurrence), keeping spec strict
        parts = verus_src.rsplit(target, 1)
        mutated_verus = (target.replace("<", "<=")).join(parts)
        verus_copy = tmp / "three_fence_guard_mutated.rs"
        verus_copy.write_text(mutated_verus, encoding="utf-8")
        v_res = ctx.run_verus(verus_copy, cwd=tmp, cache=False)
        _assert_real_probe_failure(v_res, "Verus")
        assert_contains_regex(
            r"verification results::\s*\d+\s+verified,\s*[1-9]\d*\s+errors",
            v_res.combined,
            "Verus non-vacuity probe must report >= 1 verification error",
        )

        # 2. Quint negative mutation: weaken renew guard from incarnationKey to spiffeRole
        quint_src = ctx.read_text("server/specs/quint/placement_lease.qnt")
        q_target = "activeHolder == h.incarnationKey,"
        if q_target not in quint_src:
            fail("unexpected placement_lease.qnt structure for Quint non-vacuity probe")
        mutated_quint = quint_src.replace(q_target, "activeRole == h.spiffeRole,", 1)
        quint_copy = tmp / "placement_lease_mutated.qnt"
        quint_copy.write_text(mutated_quint, encoding="utf-8")
        q_res = ctx.run_quint(["test", str(quint_copy)], cwd=tmp, cache=False)
        _assert_real_probe_failure(q_res, "Quint")
        assert_contains_regex(
            r"(failed|failing|violated|Violation|Invariant|QNT\d+)",
            q_res.combined,
            "Quint non-vacuity probe must report a failing test or invariant violation",
            flags=re.IGNORECASE,
        )

        # 3. PObserve negative mutation: drop credGen field from eProviderDispatch in Events.p
        p_copy_dir = tmp / "pobserve"
        shutil.copytree(ctx.server_dir / "specs" / "pobserve", p_copy_dir)
        events_copy = p_copy_dir / "Events.p"
        e_text = events_copy.read_text(encoding="utf-8")
        e_target = "event eProviderDispatch: (tenant: string, attemptId: string, credGen: int);"
        if e_target not in e_text:
            fail("unexpected Events.p structure for PObserve non-vacuity probe")
        events_copy.write_text(
            e_text.replace(e_target, "event eProviderDispatch: (tenant: string, attemptId: string);"),
            encoding="utf-8",
        )
        p_res = ctx.run_pobserve_compile(p_copy_dir / "WaddleMonitors.pproj", cache=False)
        _assert_real_probe_failure(p_res, "PObserve")
        assert_contains_regex(
            r"(Compilation failed|error|syntax|mismatched|extraneous|no viable)",
            p_res.combined,
            "PObserve non-vacuity probe must report a real P compilation error",
            flags=re.IGNORECASE,
        )


# ---------------------------------------------------------------------------
# M2–M6 Release Evidence Gate Scenarios (12 scenarios — FAIL cleanly right now)
# ---------------------------------------------------------------------------

def _t4_s01_backend_parity_stacks(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "e2e_gate1_backend_parity",
        "Gate 1 (M5): identical multi-tenant SaaS workload on (Turso+OpenFGA) vs (Postgres+SpiceDB)",
    )


def _t4_s02_identity_collision_isolation_drill(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "e2e_gate1_identity_collision_drill",
        "Gate 1 (M4): acme.example & globex.example with identical alice & bob@remote.example",
    )


def _t4_s03_noisy_neighbor_rlimit_memory_drill(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "e2e_gate1_noisy_neighbor_saturation",
        "Gate 1 (M2): noisy-neighbor saturation under RLIMIT_NOFILE & Wasm memory caps",
    )


def _t4_s04_dual_broker_fencing_race(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass(
        "waddle-broker",
        "Gate 2 (M4): deterministic dual-broker fencing race yielding at most 1 provider dispatch",
        test_filter="e2e_gate2_dual_broker_race",
    )


def _t4_s05_deterministic_eight_barrier_matrix(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "e2e_gate2_eight_barrier_matrix",
        "Gate 2 (M5): 8-barrier race matrix verified against synthetic provider & PObserve monitors",
    )


def _t4_s06_authority_boundary_denial_matrix(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "e2e_gate3_authority_denial_matrix",
        "Gate 3 (M5): all 8 mandatory authority/boundary denials + positive control",
    )


def _t4_s07_member_funded_ai_chatbot_e2e(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "e2e_gate4_member_ai_chatbot_lifecycle",
        "Gate 3/4 (M6): member-funded /ai full E2E lifecycle with synthetic streaming provider",
    )


def _t4_s08_unattended_jev_judgments_e2e(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "e2e_gate4_jev_judgments_lifecycle",
        "Gate 3/4 (M6): unattended tenant-funded jev-judgments with ZDR route & per-space authz",
    )


def _t4_s09_zero_leak_sentinel_scan(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "e2e_gate4_zero_leak_sentinel_scan",
        "Gate 4 (M6): zero-leak sentinel scan across cells, guests, sm_unacked, bounce, logs, stderr, OTel",
    )


def _t4_s10_backup_wal_restore_drill(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass(
        "waddle-broker",
        "Gate 5 (M3): Turso WAL / Postgres backup restore drill enforcing destruction ledger",
        test_filter="e2e_gate5_backup_restore_drill",
    )


def _t4_s11_dependency_outage_fail_closed_drill(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "e2e_gate5_dependency_outage_drill",
        "Gate 5 (M5): KMS, OpenFGA/SpiceDB, PlacementLeaseStore, and broker outage fail-closed drill",
    )


def _t4_s12_drain_timeout_and_dedicated_cell_handoff(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "e2e_gate5_drain_and_dedicated_handoff",
        "Gate 1/5 (M6): WADDLE_DRAIN_TIMEOUT_SECS pod drain & shared-to-dedicated cell handoff",
    )


def get_tests() -> list[TestCase]:
    return [
        # M1 (1)
        TestCase(
            "T4-M1-01",
            4,
            "M1",
            ("F1", "F2", "F3"),
            ("Gate2", "Gate3"),
            "Real-World Formal Non-Vacuity: isolated temp-copy mutations rejected by Verus, Quint & PObserve",
            _t4_m1_01_formal_non_vacuity_probes,
        ),
        # Release Evidence Gates 1–5 (12)
        TestCase("T4-S01", 4, "M5", ("F7", "F14", "F19"), ("Gate1",), "Gate 1: Multi-tenant backend parity (Turso+OpenFGA vs Postgres+SpiceDB)", _t4_s01_backend_parity_stacks),
        TestCase("T4-S02", 4, "M4", ("F6", "F7", "F8", "F11", "F14"), ("Gate1",), "Gate 1: Identity collision & cross-tenant isolation drill (alice + bob@remote.example)", _t4_s02_identity_collision_isolation_drill),
        TestCase("T4-S03", 4, "M2", ("F7", "F9", "F10"), ("Gate1",), "Gate 1: Noisy-neighbor saturation under RLIMIT_NOFILE & Wasm memory caps", _t4_s03_noisy_neighbor_rlimit_memory_drill),
        TestCase("T4-S04", 4, "M4", ("F10", "F15"), ("Gate2",), "Gate 2: Deterministic dual-broker fencing race (single provider dispatch)", _t4_s04_dual_broker_fencing_race),
        TestCase("T4-S05", 4, "M5", ("F3", "F10", "F13", "F15", "F16", "F19"), ("Gate2",), "Gate 2: Deterministic 8-barrier race matrix + PObserve monitors", _t4_s05_deterministic_eight_barrier_matrix),
        TestCase("T4-S06", 4, "M5", ("F10", "F11", "F12", "F14", "F15", "F17"), ("Gate3",), "Gate 3: Complete 8-case authority & boundary denial matrix + positive control", _t4_s06_authority_boundary_denial_matrix),
        TestCase("T4-S07", 4, "M6", ("F12", "F13", "F18", "F20", "F21"), ("Gate3", "Gate4"), "Gate 3/4: Member-funded /ai full E2E lifecycle (ai-chatbot + synthetic streaming provider)", _t4_s07_member_funded_ai_chatbot_e2e),
        TestCase("T4-S08", 4, "M6", ("F15", "F17", "F18", "F20"), ("Gate3", "Gate4"), "Gate 3/4: Unattended tenant-funded jev-judgments E2E lifecycle (ZDR + per-space authz)", _t4_s08_unattended_jev_judgments_e2e),
        TestCase("T4-S09", 4, "M6", ("F11", "F13", "F17", "F18", "F20", "F21"), ("Gate4",), "Gate 4: Zero-leak sentinel scan across cells, guests, sm_unacked, bounces, stderr, OTel", _t4_s09_zero_leak_sentinel_scan),
        TestCase("T4-S10", 4, "M3", ("F5", "F11"), ("Gate5",), "Gate 5: Backup & WAL restore drill (destruction ledger enforcement before egress)", _t4_s10_backup_wal_restore_drill),
        TestCase("T4-S11", 4, "M5", ("F10", "F11", "F14", "F15"), ("Gate5",), "Gate 5: Dependency outage & fail-closed resilience drill (KMS/OpenFGA/SpiceDB/Lease)", _t4_s11_dependency_outage_fail_closed_drill),
        TestCase("T4-S12", 4, "M6", ("F10", "F18", "F21"), ("Gate1", "Gate5"), "Gate 1/5: WADDLE_DRAIN_TIMEOUT_SECS pod drain & dedicated-cell handoff drill", _t4_s12_drain_timeout_and_dedicated_cell_handoff),
    ]
