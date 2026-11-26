"""Tier 1 Feature Coverage: M1 Formal Verification, ADR/Traits, Turso WAL & E2E Meta (F1–F5, F22).

All 30 tests in this module MUST PASS on the M1 baseline.
"""

from __future__ import annotations

import re

from harness import (
    RunContext,
    TestCase,
    assert_cargo_tests_passed,
    assert_contains_regex,
    assert_eq,
    assert_file_exists,
    assert_in,
    assert_not_contains_regex,
    assert_not_in,
    assert_true,
    extract_brace_block,
    fail,
    strip_comments,
)

QUINT_SPECS = (
    ("three_fence_admission.qnt", 3),
    ("placement_lease.qnt", 4),
    ("credential_lifecycle_restore.qnt", 3),
    ("stream_cancel_and_quarantine.qnt", 2),
)

VERUS_PROOFS = (
    ("aad_codec.rs", 7),
    ("three_fence_guard.rs", 10),
    ("budget_ledger.rs", 8),
    ("ssrf_ip_filter.rs", 11),
    ("lru_fd_pool.rs", 8),
)


# ---------------------------------------------------------------------------
# F1: Formal Verification — Quint.SH (5 tests)
# ---------------------------------------------------------------------------

def _t1_f01_01_quint_typecheck_all(ctx: RunContext) -> None:
    for spec_name, _ in QUINT_SPECS:
        spec = ctx.server_dir / "specs" / "quint" / spec_name
        assert_file_exists(spec)
        res = ctx.run_quint(["typecheck", str(spec)])
        assert_eq(res.returncode, 0, f"quint typecheck failed on {spec_name}: {res.combined[-600:]}")


def _t1_f01_02_quint_test_three_fence(ctx: RunContext) -> None:
    spec = ctx.server_dir / "specs" / "quint" / "three_fence_admission.qnt"
    res = ctx.run_quint(["test", str(spec)])
    assert_eq(res.returncode, 0, f"quint test failed on three_fence_admission.qnt: {res.combined[-600:]}")
    assert_in("3 passing", res.combined, "expected 3 passing Quint tests in three_fence_admission.qnt")


def _t1_f01_03_quint_test_placement_lease(ctx: RunContext) -> None:
    spec = ctx.server_dir / "specs" / "quint" / "placement_lease.qnt"
    res = ctx.run_quint(["test", str(spec)])
    assert_eq(res.returncode, 0, f"quint test failed on placement_lease.qnt: {res.combined[-600:]}")
    assert_in("4 passing", res.combined, "expected 4 passing Quint tests in placement_lease.qnt")


def _t1_f01_04_quint_test_credential_lifecycle(ctx: RunContext) -> None:
    spec = ctx.server_dir / "specs" / "quint" / "credential_lifecycle_restore.qnt"
    res = ctx.run_quint(["test", str(spec)])
    assert_eq(res.returncode, 0, f"quint test failed on credential_lifecycle_restore.qnt: {res.combined[-600:]}")
    assert_in("3 passing", res.combined, "expected 3 passing Quint tests in credential_lifecycle_restore.qnt")


def _t1_f01_05_quint_run_safety_invariants(ctx: RunContext) -> None:
    spec_q = ctx.server_dir / "specs" / "quint" / "stream_cancel_and_quarantine.qnt"
    res_t = ctx.run_quint(["test", str(spec_q)])
    assert_eq(res_t.returncode, 0, f"quint test failed on stream_cancel_and_quarantine.qnt: {res_t.combined[-600:]}")
    assert_in("2 passing", res_t.combined, "expected 2 passing Quint tests in stream_cancel_and_quarantine.qnt")

    for spec_name in ("stream_cancel_and_quarantine.qnt", "three_fence_admission.qnt"):
        spec = ctx.server_dir / "specs" / "quint" / spec_name
        res = ctx.run_quint([
            "run",
            "--seed=0x42",
            "--max-samples=200",
            "--max-steps=20",
            "--invariant=safetyInv",
            str(spec),
        ])
        assert_eq(res.returncode, 0, f"quint run failed on {spec_name}: {res.combined[-600:]}")
        assert_in("[ok] No violation found", res.combined, f"safetyInv check missing ok banner on {spec_name}")


# ---------------------------------------------------------------------------
# F2: Formal Verification — Verus (5 tests)
# ---------------------------------------------------------------------------

def _make_verus_test(filename: str, min_verified: int):
    def _fn(ctx: RunContext) -> None:
        proof = ctx.server_dir / "specs" / "verus" / filename
        assert_file_exists(proof)
        res = ctx.run_verus(proof)
        assert_eq(res.returncode, 0, f"verus failed on {filename}: {res.combined[-800:]}")
        m = re.search(r"verification results::\s*(\d+)\s+verified,\s*(\d+)\s+errors", res.combined)
        assert_true(m is not None, f"missing verus summary line in output for {filename}")
        verified, errors = int(m.group(1)), int(m.group(2))
        assert_eq(errors, 0, f"verus reported errors in {filename}")
        assert_true(
            verified >= min_verified,
            f"verus verified {verified} < expected {min_verified} in {filename}",
        )
    return _fn


# ---------------------------------------------------------------------------
# F3: Formal Verification — PObserve & trace_verifier (5 tests)
# ---------------------------------------------------------------------------

def _t1_f03_01_pobserve_compile(ctx: RunContext) -> None:
    pobserve_dir = ctx.server_dir / "specs" / "pobserve"
    for fname in (
        "WaddleMonitors.pproj",
        "Events.p",
        "AdmissionFenceMonitor.p",
        "PlacementLeaseMonitor.p",
        "StreamCancellationMonitor.p",
    ):
        assert_file_exists(pobserve_dir / fname)
    res = ctx.run_pobserve_compile()
    assert_eq(res.returncode, 0, f"p compile --mode pobserve failed: {res.combined[-600:]}")
    assert_in("Compilation succeeded.", res.combined, "PObserve compiler did not report success")
    assert_true(
        not (pobserve_dir / "PGenerated").exists(),
        "PObserve compilation leaked PGenerated/ into repository",
    )


def _t1_f03_02_trace_verifier_unit_suite(ctx: RunContext) -> None:
    res = ctx.run_cargo_test(["-p", "waddle-broker-core", "--lib"])
    assert_cargo_tests_passed(
        res,
        "waddle-broker-core trace_verifier unit suite",
        min_passed=4,
        required_test_prefix="trace_verifier::",
    )


def _t1_f03_03_trace_verifier_admission_safety(ctx: RunContext) -> None:
    src = strip_comments(ctx.read_text("server/crates/waddle-broker-core/src/trace_verifier.rs"), "rust")
    for sym in (
        "AdmissionFenceMonitor",
        "DuplicateAttemptDispatch",
        "PostRevokeDispatch",
        "SilentPayerFallback",
    ):
        assert_in(sym, src, f"trace_verifier missing admission safety symbol {sym}")
    res = ctx.run_cargo_test(["-p", "waddle-broker-core", "--lib"])
    assert_cargo_tests_passed(
        res,
        "waddle-broker-core trace_verifier admission safety",
        min_passed=4,
        required_test_prefix="trace_verifier::",
    )


def _t1_f03_04_trace_verifier_lease_safety(ctx: RunContext) -> None:
    src = strip_comments(ctx.read_text("server/crates/waddle-broker-core/src/trace_verifier.rs"), "rust")
    for sym in (
        "PlacementLeaseMonitor",
        "NonMonotonicLeaseGen",
        "LeaseTransferMismatch",
        "StaleTenantWrite",
    ):
        assert_in(sym, src, f"trace_verifier missing lease safety symbol {sym}")
    res = ctx.run_cargo_test(["-p", "waddle-broker-core", "--lib"])
    assert_cargo_tests_passed(
        res,
        "waddle-broker-core trace_verifier lease safety",
        min_passed=4,
        required_test_prefix="trace_verifier::",
    )


def _t1_f03_05_trace_verifier_stream_and_secret_free(ctx: RunContext) -> None:
    src = strip_comments(ctx.read_text("server/crates/waddle-broker-core/src/trace_verifier.rs"), "rust")
    assert_in("StreamCancellationMonitor", src)
    assert_in("assert_secret_free", src)
    assert_in("deny_unknown_fields", src)
    # Extract full brace-balanced TraceRecord and TraceEvent blocks (no magic byte slice)
    # and verify no forbidden secret/PII field name appears (with or without `pub`).
    record_block = extract_brace_block(src, r"\bpub\s+struct\s+TraceRecord\b")
    event_block = extract_brace_block(src, r"\bpub\s+enum\s+TraceEvent\b")
    schema_blocks = f"{record_block}\n{event_block}"
    for forbidden in (
        "api_key",
        "secret",
        "plaintext",
        "prompt",
        "raw_jid",
        "bearer",
        "token",
        "password",
        "authorization",
        "wrapped_dek",
        "ciphertext",
    ):
        assert_not_contains_regex(
            rf"\b{forbidden}\s*:",
            schema_blocks,
            f"TraceRecord/TraceEvent must not declare forbidden field {forbidden}",
        )
    res = ctx.run_cargo_test(["-p", "waddle-broker-core", "--lib"])
    assert_cargo_tests_passed(
        res,
        "waddle-broker-core trace_verifier stream & secret-free",
        min_passed=4,
        required_test_prefix="trace_verifier::",
    )


# ---------------------------------------------------------------------------
# F4: ADR-0021 & waddle-broker-core Trait Contracts (5 tests)
# ---------------------------------------------------------------------------

def _t1_f04_01_adr0021_supersession(ctx: RunContext) -> None:
    adr = ctx.read_text("server/docs/adrs/0021-tenant-isolation-and-brokered-inference.md")
    assert_in("RFC-0007", adr, "ADR-0021 must reference RFC-0007")
    assert_in("RFC-0014", adr, "ADR-0021 must reference RFC-0014")
    assert_in("waddle:extension@4.0.0", adr, "ADR-0021 must scope supersession to waddle:extension@4.0.0")
    for d in range(1, 11):
        assert_in(f"| D{d} |", adr, f"ADR-0021 missing decision D{d}")


def _t1_f04_02_adr0021_trust_boundaries_and_bounds(ctx: RunContext) -> None:
    adr = ctx.read_text("server/docs/adrs/0021-tenant-isolation-and-brokered-inference.md")
    for section in ("## Trust-Boundary Inventory", "## Ownership Matrix", "Release Evidence"):
        assert_in(section, adr, f"ADR-0021 missing section {section}")
    for boundary in ("Colony", "OpenFGA", "SpiceDB", "Turso", "PostgreSQL", "KMS"):
        assert_in(boundary, adr, f"ADR-0021 missing trust boundary {boundary}")


def _t1_f04_03_broker_core_traits_compile(ctx: RunContext) -> None:
    for rel, trait_name in (
        ("server/crates/waddle-broker-core/src/store.rs", "pub trait TenantStore"),
        ("server/crates/waddle-broker-core/src/placement.rs", "pub trait PlacementLeaseStore"),
        ("server/crates/waddle-broker-core/src/admission.rs", "pub trait AdmissionLedgerStore"),
        ("server/crates/waddle-broker-core/src/permission.rs", "pub trait PermissionBackend"),
    ):
        assert_in(trait_name, ctx.read_text(rel), f"missing {trait_name} in {rel}")
    res = ctx.run_cargo_test(["-p", "waddle-broker-core", "--lib"])
    assert_cargo_tests_passed(res, "waddle-broker-core --lib", min_passed=22)


def _t1_f04_04_broker_core_domain_types(ctx: RunContext) -> None:
    lib_src = ctx.read_text("server/crates/waddle-broker-core/src/lib.rs")
    for mod_name in ("identity", "store", "placement", "permission", "admission", "kernels", "trace_verifier"):
        assert_in(f"pub mod {mod_name};", lib_src, f"waddle-broker-core missing pub mod {mod_name}")
    id_src = ctx.read_text("server/crates/waddle-broker-core/src/identity.rs")
    for ty in ("pub struct TenantId", "pub struct AccountId", "pub struct HolderInstance"):
        assert_in(ty, id_src)
    adm_src = ctx.read_text("server/crates/waddle-broker-core/src/admission.rs")
    for ty in ("pub struct ThreeFenceSnapshot", "pub struct AdmitAttemptRequest", "pub enum AttemptOutcome"):
        assert_in(ty, adm_src)


def _t1_f04_05_workspace_and_crate_hygiene(ctx: RunContext) -> None:
    ws_toml = ctx.read_text("server/Cargo.toml")
    assert_in("crates/waddle-broker-core", ws_toml, "waddle-broker-core must be in server workspace members")
    crate_toml = ctx.read_text("server/crates/waddle-broker-core/Cargo.toml")
    assert_in("[lints]", crate_toml)
    assert_in("workspace = true", crate_toml)
    assert_contains_regex(r"turso\s*=\s*\{[^}]*default-features\s*=\s*false", crate_toml, "turso must disable default mimalloc")
    res = ctx.run_cmd(["python3", "server/scripts/check-rust-structure.py", "--server-only", "."])
    assert_eq(res.returncode, 0, f"check-rust-structure.py failed: {res.combined[-600:]}")


# ---------------------------------------------------------------------------
# F5: Turso WAL Baseline Verification (5 tests)
# ---------------------------------------------------------------------------

def _run_turso_spike(ctx: RunContext) -> str:
    res = ctx.run_cargo_test(["-p", "waddle-broker-core", "--test", "turso_baseline_spike"])
    assert_cargo_tests_passed(res, "turso_baseline_spike", min_passed=4)
    return res.combined


def _t1_f05_01_turso_wal_mode(ctx: RunContext) -> None:
    out = _run_turso_spike(ctx)
    assert_in("test_wal_mode_baseline_schema_and_checkpoint_probe ... ok", out)
    spike_src = ctx.read_text("server/crates/waddle-broker-core/tests/turso_baseline_spike.rs")
    assert_not_in("journal_mode = mvcc", spike_src.lower(), "must not use pre-1.0 Turso mvcc mode")


def _t1_f05_02_turso_eight_triggers(ctx: RunContext) -> None:
    out = _run_turso_spike(ctx)
    assert_in("test_all_eight_baseline_triggers_enforced ... ok", out)


def _t1_f05_03_turso_fts_probe(ctx: RunContext) -> None:
    out = _run_turso_spike(ctx)
    assert_in("test_mam_fulltext_glob_and_fts5_vs_turso_fts_probe ... ok", out)


def _t1_f05_04_turso_unixepoch_subsec(ctx: RunContext) -> None:
    out = _run_turso_spike(ctx)
    assert_in("test_unixepoch_subsec_millis_monotonicity ... ok", out)


def _t1_f05_05_turso_multipod_topology_adr(ctx: RunContext) -> None:
    adr = ctx.read_text("server/docs/adrs/0021-tenant-isolation-and-brokered-inference.md")
    assert_in("Option (a)", adr, "ADR-0021 must record Option (a) Turso multi-pod topology")
    assert_in("storage-host", adr, "ADR-0021 must document mTLS Turso storage-host")
    assert_in("(tenant_id, holder_id, placement_generation)", adr)


# ---------------------------------------------------------------------------
# F22: E2E Meta / Runner Self-Checks (5 tests, milestone="E2E")
# ---------------------------------------------------------------------------

def _t1_f22_01_test_infra_doc_complete(ctx: RunContext) -> None:
    doc = ctx.read_text("TEST_INFRA.md")
    for f_num in range(1, 23):
        assert_in(f"F{f_num}", doc, f"TEST_INFRA.md missing feature F{f_num}")
    for gate in ("Gate 1", "Gate 2", "Gate 3", "Gate 4", "Gate 5"):
        assert_in(gate, doc, f"TEST_INFRA.md missing Release Evidence {gate}")


def _t1_f22_02_unique_test_ids_and_metadata(ctx: RunContext) -> None:
    assert_true(len(ctx.all_cases) >= 120, f"expected >= 120 registered E2E cases, got {len(ctx.all_cases)}")
    seen: set[str] = set()
    for tc in ctx.all_cases:
        assert_true(tc.test_id not in seen, f"duplicate test_id {tc.test_id}")
        seen.add(tc.test_id)
        assert_true(tc.tier in (1, 2, 3, 4), f"invalid tier {tc.tier} on {tc.test_id}")
        assert_true(len(tc.feature_ids) >= 1, f"missing feature_ids on {tc.test_id}")
        assert_true(bool(tc.description.strip()), f"empty description on {tc.test_id}")


def _t1_f22_03_all_22_features_covered_in_tier1(ctx: RunContext) -> None:
    covered: set[str] = set()
    for tc in ctx.all_cases:
        if tc.tier == 1:
            covered.update(tc.feature_ids)
    missing = [f"F{i}" for i in range(1, 23) if f"F{i}" not in covered]
    assert_eq(missing, [], f"Tier 1 missing feature coverage for {missing}")


def _t1_f22_04_all_5_gates_covered_in_tier4(ctx: RunContext) -> None:
    covered: set[str] = set()
    for tc in ctx.all_cases:
        if tc.tier == 4:
            covered.update(tc.gate_ids)
    missing = [f"Gate{i}" for i in range(1, 6) if f"Gate{i}" not in covered]
    assert_eq(missing, [], f"Tier 4 missing Release Evidence gate coverage for {missing}")


def _t1_f22_05_runner_cli_list_works(ctx: RunContext) -> None:
    runner = ctx.e2e_dir / "run_e2e.py"
    assert_file_exists(runner)
    res = ctx.run_cmd(["python3", str(runner), "--list"])
    assert_eq(res.returncode, 0, f"run_e2e.py --list failed: {res.combined[-400:]}")
    assert_in("T1-F01-01", res.stdout)
    assert_in("T4-S12", res.stdout)


def get_tests() -> list[TestCase]:
    return [
        # F1
        TestCase("T1-F01-01", 1, "M1", ("F1",), ("Gate2", "Gate5"), "Quint typecheck succeeds on all 4 protocol specs", _t1_f01_01_quint_typecheck_all),
        TestCase("T1-F01-02", 1, "M1", ("F1",), ("Gate2",), "Quint test passes 3 deterministic tests in three_fence_admission.qnt", _t1_f01_02_quint_test_three_fence),
        TestCase("T1-F01-03", 1, "M1", ("F1",), ("Gate2",), "Quint test passes 4 deterministic tests in placement_lease.qnt", _t1_f01_03_quint_test_placement_lease),
        TestCase("T1-F01-04", 1, "M1", ("F1",), ("Gate5",), "Quint test passes 3 deterministic tests in credential_lifecycle_restore.qnt", _t1_f01_04_quint_test_credential_lifecycle),
        TestCase("T1-F01-05", 1, "M1", ("F1",), ("Gate2",), "Quint test + bounded run safetyInv hold on stream_cancel_and_quarantine & three_fence_admission", _t1_f01_05_quint_run_safety_invariants),
        # F2
        TestCase("T1-F02-01", 1, "M1", ("F2",), ("Gate3", "Gate4"), "Verus verifies aad_codec.rs (>=7 verified, 0 errors)", _make_verus_test("aad_codec.rs", 7)),
        TestCase("T1-F02-02", 1, "M1", ("F2",), ("Gate2",), "Verus verifies three_fence_guard.rs (>=10 verified, 0 errors)", _make_verus_test("three_fence_guard.rs", 10)),
        TestCase("T1-F02-03", 1, "M1", ("F2",), ("Gate2",), "Verus verifies budget_ledger.rs (>=8 verified, 0 errors)", _make_verus_test("budget_ledger.rs", 8)),
        TestCase("T1-F02-04", 1, "M1", ("F2",), ("Gate3", "Gate4"), "Verus verifies ssrf_ip_filter.rs (>=11 verified, 0 errors)", _make_verus_test("ssrf_ip_filter.rs", 11)),
        TestCase("T1-F02-05", 1, "M1", ("F2",), ("Gate1",), "Verus verifies lru_fd_pool.rs (>=8 verified, 0 errors)", _make_verus_test("lru_fd_pool.rs", 8)),
        # F3
        TestCase("T1-F03-01", 1, "M1", ("F3",), ("Gate2",), "PObserve compiles WaddleMonitors.pproj cleanly into isolated tempdir", _t1_f03_01_pobserve_compile),
        TestCase("T1-F03-02", 1, "M1", ("F3",), ("Gate2",), "waddle-broker-core trace_verifier unit tests pass", _t1_f03_02_trace_verifier_unit_suite),
        TestCase("T1-F03-03", 1, "M1", ("F3",), ("Gate2",), "trace_verifier enforces AdmissionFenceMonitor double-dispatch/post-revoke/payer safety", _t1_f03_03_trace_verifier_admission_safety),
        TestCase("T1-F03-04", 1, "M1", ("F3",), ("Gate2",), "trace_verifier enforces PlacementLeaseMonitor generation/holder safety", _t1_f03_04_trace_verifier_lease_safety),
        TestCase("T1-F03-05", 1, "M1", ("F3",), ("Gate2", "Gate4"), "trace_verifier enforces StreamCancellationMonitor and secret-free deny_unknown_fields schema", _t1_f03_05_trace_verifier_stream_and_secret_free),
        # F4
        TestCase("T1-F04-01", 1, "M1", ("F4",), ("Gate1", "Gate3"), "ADR-0021 exists, defines D1-D10, and scopes RFC-0007/RFC-0014 supersession", _t1_f04_01_adr0021_supersession),
        TestCase("T1-F04-02", 1, "M1", ("F4",), ("Gate1", "Gate3"), "ADR-0021 documents 9 trust boundaries, ownership matrix, and Release Evidence gates", _t1_f04_02_adr0021_trust_boundaries_and_bounds),
        TestCase("T1-F04-03", 1, "M1", ("F4",), ("Gate1",), "waddle-broker-core exposes 4 backend-agnostic traits and passes >=22 lib tests", _t1_f04_03_broker_core_traits_compile),
        TestCase("T1-F04-04", 1, "M1", ("F4",), ("Gate1",), "waddle-broker-core exports canonical identity, placement, and admission domain types", _t1_f04_04_broker_core_domain_types),
        TestCase("T1-F04-05", 1, "M1", ("F4",), ("Gate1",), "waddle-broker-core satisfies workspace lints, no-mimalloc turso, and check-rust-structure.py", _t1_f04_05_workspace_and_crate_hygiene),
        # F5
        TestCase("T1-F05-01", 1, "M1", ("F5",), ("Gate1", "Gate5"), "Turso baseline spike passes in standard WAL mode (never mvcc)", _t1_f05_01_turso_wal_mode),
        TestCase("T1-F05-02", 1, "M1", ("F5",), ("Gate1",), "Turso baseline spike enforces all 8 sqlite.sql triggers", _t1_f05_02_turso_eight_triggers),
        TestCase("T1-F05-03", 1, "M1", ("F5",), ("Gate1",), "Turso baseline spike verifies MAM GLOB and FTS probe", _t1_f05_03_turso_fts_probe),
        TestCase("T1-F05-04", 1, "M1", ("F5",), ("Gate1",), "Turso baseline spike verifies unixepoch('subsec') * 1000 monotonicity", _t1_f05_04_turso_unixepoch_subsec),
        TestCase("T1-F05-05", 1, "M1", ("F5",), ("Gate1", "Gate5"), "ADR-0021 records Option (a) Turso mTLS storage-host topology", _t1_f05_05_turso_multipod_topology_adr),
        # F22 (E2E meta)
        TestCase("T1-F22-01", 1, "E2E", ("F22",), ("Gate1", "Gate2", "Gate3", "Gate4", "Gate5"), "TEST_INFRA.md maps all 22 features and all 5 Release Evidence gates", _t1_f22_01_test_infra_doc_complete),
        TestCase("T1-F22-02", 1, "E2E", ("F22",), ("Gate1",), "All registered E2E tests have unique IDs and valid tier/feature metadata", _t1_f22_02_unique_test_ids_and_metadata),
        TestCase("T1-F22-03", 1, "E2E", ("F22",), ("Gate1",), "Tier 1 covers all 22 features (F1-F22) with zero omissions", _t1_f22_03_all_22_features_covered_in_tier1),
        TestCase("T1-F22-04", 1, "E2E", ("F22",), ("Gate1", "Gate2", "Gate3", "Gate4", "Gate5"), "Tier 4 scenarios cover all 5 Release Evidence gates", _t1_f22_04_all_5_gates_covered_in_tier4),
        TestCase("T1-F22-05", 1, "E2E", ("F22",), ("Gate1",), "run_e2e.py --list executes cleanly and lists Tier 1-4 tests", _t1_f22_05_runner_cli_list_works),
    ]
