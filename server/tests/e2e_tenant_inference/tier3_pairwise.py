"""Tier 3 Cross-Feature Pairwise Combinations (15 tests: 3 M1 + 12 M2–M6).

M1 pairwise checks (3) MUST PASS right now; M2–M6 pairwise combinations (12)
MUST FAIL cleanly until their respective milestones land.
"""

from __future__ import annotations

import re

from harness import (
    RunContext,
    TestCase,
    assert_cargo_tests_passed,
    assert_eq,
    assert_in,
    assert_true,
)


# ---------------------------------------------------------------------------
# M1 Pairwise Checks (3 tests — PASS right now)
# ---------------------------------------------------------------------------

def _t3_m1_01_pobserve_trace_verifier_parity(ctx: RunContext) -> None:
    events_p = ctx.read_text("server/specs/pobserve/Events.p")
    tv_rs = ctx.read_text("server/crates/waddle-broker-core/src/trace_verifier.rs")

    p_events = re.findall(r"event\s+e([A-Za-z0-9]+)\s*:", events_p)
    assert_eq(len(p_events), 15, f"expected 15 PObserve events in Events.p, found {len(p_events)}")

    enum_match = re.search(r"pub enum TraceEvent\s*\{([\s\S]*?)\n\}", tv_rs)
    assert_true(enum_match is not None, "could not locate pub enum TraceEvent in trace_verifier.rs")
    rust_variants = re.findall(r"^\s{4}([A-Z][A-Za-z0-9]+)\s*\{", enum_match.group(1), flags=re.MULTILINE)
    assert_eq(len(rust_variants), 15, f"expected 15 TraceEvent variants in trace_verifier.rs, found {len(rust_variants)}")
    assert_eq(
        sorted(p_events),
        sorted(rust_variants),
        "PObserve Events.p declarations and Rust TraceEvent variants must have 1:1 parity",
    )


def _t3_m1_02_verus_broker_core_kernel_parity(ctx: RunContext) -> None:
    verus_stems = sorted(p.stem for p in (ctx.server_dir / "specs" / "verus").glob("*.rs"))
    kernel_stems = sorted(
        p.stem
        for p in (ctx.server_dir / "crates" / "waddle-broker-core" / "src" / "kernels").glob("*.rs")
        if p.stem != "mod"
    )
    assert_eq(len(verus_stems), 5, f"expected 5 Verus proof files, got {verus_stems}")
    assert_eq(verus_stems, kernel_stems, "Verus proofs and waddle-broker-core kernels must match 1:1")
    res = ctx.run_cargo_test(["-p", "waddle-broker-core", "--lib"])
    assert_cargo_tests_passed(
        res,
        "waddle-broker-core kernels tests",
        min_passed=5,
        required_test_prefix="kernels::",
    )


def _t3_m1_03_quint_placement_and_admission_invariants(ctx: RunContext) -> None:
    for spec_name in ("placement_lease.qnt", "three_fence_admission.qnt"):
        spec = ctx.server_dir / "specs" / "quint" / spec_name
        res = ctx.run_quint([
            "run",
            "--seed=0x42",
            "--max-samples=200",
            "--max-steps=50",
            "--invariant=safetyInv",
            str(spec),
        ])
        assert_eq(res.returncode, 0, f"quint safetyInv failed on {spec_name}: {res.combined[-500:]}")
        assert_in("[ok] No violation found", res.combined)


# ---------------------------------------------------------------------------
# M2–M6 Pairwise Combinations (12 tests — FAIL cleanly right now)
# ---------------------------------------------------------------------------

def _t3_01_concurrent_revocation_and_inflight_authz(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass(
        "waddle-broker",
        "M4 Pairwise (F14+F15): concurrent revocation barrier vs in-flight OpenFGA/SpiceDB check",
        test_filter="pairwise_revoke_inflight_authz",
    )


def _t3_02_lease_transfer_and_active_stream_drop(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "pairwise_lease_transfer_stream_cancel",
        "M5 Pairwise (F10+F15+F18): lease transfer closes placement fence -> ReplicaFence + sync Drop cancel",
    )


def _t3_03_same_localpart_remote_barejid_mam_cred_isolation(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "pairwise_cross_tenant_identity_mam_cred",
        "M4 Pairwise (F6+F7+F8+F11+F14): same-localpart + same-remote-BareJid + MAM + credential isolation",
    )


def _t3_04_xep0198_replay_and_destruction_ledger_restore(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass(
        "waddle-broker",
        "M3 Pairwise (F11+F13): XEP-0198 reconnect replay + destruction ledger restore enforcement",
        test_filter="pairwise_sm_replay_destruction_restore",
    )


def _t3_05_unknown_after_send_and_budget_conservation(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass(
        "waddle-broker",
        "M4 Pairwise (F15+F16): unknown_after_send quarantine conserves reserved budget without double spend",
        test_filter="pairwise_quarantine_budget_conservation",
    )


def _t3_06_room_kick_replica_fence_and_archive_reauthz(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "pairwise_room_kick_stream_and_mam_abort",
        "M6 Pairwise (F14+F15+F20): room kick mid-tool-round aborts stream via ReplicaFence & denies MAM",
    )


def _t3_07_sealed_enrollment_and_four_bounce_paths(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "pairwise_sealed_enroll_bounce_strip",
        "M3 Pairwise (F11+F12+F13): IdP proof + pinned root subkey + sealed IQ + all 4 bounce strip paths",
    )


def _t3_08_noisy_neighbor_saturation_and_reserved_heartbeat(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "pairwise_noisy_neighbor_heartbeat",
        "M2 Pairwise (F7+F9+F10): noisy-neighbor saturation + Turso LRU FD cap + reserved heartbeat capacity",
    )


def _t3_09_hostile_provider_and_stderr_suppression(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "pairwise_hostile_provider_zero_leak",
        "M6 Pairwise (F17+F18+F20): hostile provider (502 sentinel + 302 metadata) + guest stderr suppression",
    )


def _t3_10_unattended_observer_member_denial_and_quarantine(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "pairwise_observer_member_denial_quarantine",
        "M6 Pairwise (F15+F16+F20): jev-judgments member-cred denial + tenant ref + unknown_after_send quarantine",
    )


def _t3_11_graceful_drain_stream_cancel_and_rollback(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "pairwise_drain_cancel_rollback",
        "M6 Pairwise (F10+F18+F21): WADDLE_DRAIN_TIMEOUT_SECS drain + active stream sync Drop + rollback",
    )


def _t3_12_tenant_deletion_cascade_turso_openfga_kms(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass(
        "waddle-broker",
        "M4 Pairwise (F7+F11+F14): tenant deletion purges Turso WAL, OpenFGA DeleteStore, KMS destruction ledger",
        test_filter="pairwise_tenant_deletion_cascade",
    )


def get_tests() -> list[TestCase]:
    return [
        # M1 (3)
        TestCase("T3-M1-01", 3, "M1", ("F3", "F4"), ("Gate2", "Gate4"), "Pairwise: PObserve Events.p <-> trace_verifier.rs 15-event schema parity", _t3_m1_01_pobserve_trace_verifier_parity),
        TestCase("T3-M1-02", 3, "M1", ("F2", "F4"), ("Gate1", "Gate2"), "Pairwise: Verus proofs <-> waddle-broker-core 5-kernel module parity", _t3_m1_02_verus_broker_core_kernel_parity),
        TestCase("T3-M1-03", 3, "M1", ("F1",), ("Gate2",), "Pairwise: Quint placement_lease + three_fence_admission combined safetyInv", _t3_m1_03_quint_placement_and_admission_invariants),
        # M2–M6 (12)
        TestCase("T3-01", 3, "M4", ("F14", "F15"), ("Gate2",), "Pairwise: concurrent revocation + in-flight OpenFGA/SpiceDB authz check", _t3_01_concurrent_revocation_and_inflight_authz),
        TestCase("T3-02", 3, "M5", ("F10", "F15", "F18"), ("Gate2", "Gate4"), "Pairwise: lease transfer + active stream sync Drop cancellation", _t3_02_lease_transfer_and_active_stream_drop),
        TestCase("T3-03", 3, "M4", ("F6", "F7", "F8", "F11", "F14"), ("Gate1", "Gate3"), "Pairwise: same-localpart + same-remote-BareJid + cross-tenant MAM & cred isolation", _t3_03_same_localpart_remote_barejid_mam_cred_isolation),
        TestCase("T3-04", 3, "M3", ("F11", "F13"), ("Gate2", "Gate5"), "Pairwise: XEP-0198 reconnect replay + destruction ledger restore", _t3_04_xep0198_replay_and_destruction_ledger_restore),
        TestCase("T3-05", 3, "M4", ("F15", "F16"), ("Gate2",), "Pairwise: unknown_after_send quarantine + conservative budget conservation", _t3_05_unknown_after_send_and_budget_conservation),
        TestCase("T3-06", 3, "M6", ("F14", "F15", "F20"), ("Gate2", "Gate3"), "Pairwise: room kick mid-tool-round + ReplicaFence abort + archive_still_authorized", _t3_06_room_kick_replica_fence_and_archive_reauthz),
        TestCase("T3-07", 3, "M3", ("F11", "F12", "F13"), ("Gate3", "Gate4"), "Pairwise: sealed enrollment (IdP + pinned root subkey) + all 4 bounce paths", _t3_07_sealed_enrollment_and_four_bounce_paths),
        TestCase("T3-08", 3, "M2", ("F7", "F9", "F10"), ("Gate1",), "Pairwise: noisy-neighbor saturation + Turso LRU FD cap + reserved heartbeat", _t3_08_noisy_neighbor_saturation_and_reserved_heartbeat),
        TestCase("T3-09", 3, "M6", ("F17", "F18", "F20"), ("Gate4",), "Pairwise: hostile upstream provider (502 + 302 metadata) + stderr suppression", _t3_09_hostile_provider_and_stderr_suppression),
        TestCase("T3-10", 3, "M6", ("F15", "F16", "F20"), ("Gate2", "Gate3"), "Pairwise: jev-judgments observer + member-cred denial + quarantine", _t3_10_unattended_observer_member_denial_and_quarantine),
        TestCase("T3-11", 3, "M6", ("F10", "F18", "F21"), ("Gate5",), "Pairwise: WADDLE_DRAIN_TIMEOUT_SECS drain + sync Drop cancel + rollback", _t3_11_graceful_drain_stream_cancel_and_rollback),
        TestCase("T3-12", 3, "M4", ("F7", "F11", "F14"), ("Gate3", "Gate5"), "Pairwise: tenant deletion cascade across Turso WAL, OpenFGA, KMS ledger", _t3_12_tenant_deletion_cascade_turso_openfga_kms),
    ]
