"""Tier 2 Boundary & Corner Cases (BVA across M1–M6, 35 tests).

M1 boundary tests (10) MUST PASS right now; M2–M6 boundary tests (25) MUST FAIL
cleanly until their respective milestones land.
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
    strip_comments,
)


# ---------------------------------------------------------------------------
# M1 Boundaries (10 tests — PASS right now)
# ---------------------------------------------------------------------------

def _t2_m1_01_identity_boundaries(ctx: RunContext) -> None:
    src = ctx.read_text("server/crates/waddle-broker-core/src/identity.rs")
    assert_in("pub const MAX_TENANT_ID_BYTES: usize = 253;", src)
    assert_in("pub const MAX_TLS_EXPORTER_PROOF_BYTES: usize = 128;", src)
    assert_in("pub const MAX_CELL_ROLE_BYTES: usize = 63;", src)
    res = ctx.run_cargo_test(["-p", "waddle-broker-core", "--lib"])
    assert_cargo_tests_passed(
        res,
        "waddle-broker-core identity boundary tests",
        min_passed=1,
        required_test_prefix="identity::",
    )


def _t2_m1_02_aad_codec_boundaries(ctx: RunContext) -> None:
    res = ctx.run_cargo_test(["-p", "waddle-broker-core", "--lib"])
    assert_cargo_tests_passed(
        res,
        "waddle-broker-core aad_codec boundary tests",
        min_passed=1,
        required_test_prefix="kernels::aad_codec::",
    )


def _t2_m1_03_budget_u64_max_boundaries(ctx: RunContext) -> None:
    src = ctx.read_text("server/crates/waddle-broker-core/src/kernels/budget_ledger.rs")
    assert_in("checked_add", src, "budget_ledger must use checked_add to prevent u64::MAX wrap")
    res = ctx.run_cargo_test(["-p", "waddle-broker-core", "--lib"])
    assert_cargo_tests_passed(
        res,
        "waddle-broker-core budget_ledger boundary tests",
        min_passed=1,
        required_test_prefix="kernels::budget_ledger::",
    )


def _t2_m1_04_three_fence_guard_boundaries(ctx: RunContext) -> None:
    src = ctx.read_text("server/crates/waddle-broker-core/src/kernels/three_fence_guard.rs")
    assert_in("u64::MAX", src, "three_fence_guard must guard against u64::MAX generation overflow")
    res = ctx.run_cargo_test(["-p", "waddle-broker-core", "--lib"])
    assert_cargo_tests_passed(
        res,
        "waddle-broker-core three_fence_guard boundary tests",
        min_passed=1,
        required_test_prefix="kernels::three_fence_guard::",
    )


def _t2_m1_05_ssrf_ip_cidr_boundaries(ctx: RunContext) -> None:
    src = ctx.read_text("server/crates/waddle-broker-core/src/kernels/ssrf_ip_filter.rs")
    for edge in ("169", "100", "172", "ipv4_mapped_blocked"):
        assert_in(edge, src, f"ssrf_ip_filter missing CIDR/IPv4-mapped edge {edge}")
    res = ctx.run_cargo_test(["-p", "waddle-broker-core", "--lib"])
    assert_cargo_tests_passed(
        res,
        "waddle-broker-core ssrf_ip_filter boundary tests",
        min_passed=1,
        required_test_prefix="kernels::ssrf_ip_filter::",
    )


def _t2_m1_06_lru_fd_pool_boundaries(ctx: RunContext) -> None:
    src = ctx.read_text("server/crates/waddle-broker-core/src/kernels/lru_fd_pool.rs")
    assert_in("ZeroCapacity", src, "lru_fd_pool must reject ZeroCapacity")
    res = ctx.run_cargo_test(["-p", "waddle-broker-core", "--lib"])
    assert_cargo_tests_passed(
        res,
        "waddle-broker-core lru_fd_pool boundary tests",
        min_passed=1,
        required_test_prefix="kernels::lru_fd_pool::",
    )


def _t2_m1_07_trace_verifier_boundaries(ctx: RunContext) -> None:
    src = ctx.read_text("server/crates/waddle-broker-core/src/trace_verifier.rs")
    assert_eq(src.count("deny_unknown_fields"), 2, "both TraceRecord and TraceEvent must set deny_unknown_fields")
    assert_in("UncanceledStreamAtEof", src, "StreamCancellationMonitor must flag uncanceled streams at EOF")
    assert_in("LeaseTransferMismatch", src, "PlacementLeaseMonitor must reject ghost/mismatched transfers")
    res = ctx.run_cargo_test(["-p", "waddle-broker-core", "--lib"])
    assert_cargo_tests_passed(
        res,
        "waddle-broker-core trace_verifier boundary tests",
        min_passed=4,
        required_test_prefix="trace_verifier::",
    )


def _t2_m1_08_verus_zero_bypass_boundary(ctx: RunContext) -> None:
    verus_dir = ctx.server_dir / "specs" / "verus"
    proofs = sorted(verus_dir.glob("*.rs"))
    assert_eq(len(proofs), 5, f"expected 5 Verus proof files, found {len(proofs)}")
    for p in proofs:
        text = p.read_text(encoding="utf-8")
        assert_in("verus! {", text, f"{p.name} missing verus! block")
        assert_not_contains_regex(
            r"\b(?:assume|admit)\s*\(|external_body",
            text,
            f"Verus proof {p.name} must contain zero assume/admit/external_body bypasses",
        )


def _t2_m1_09_quint_zero_step_boundary(ctx: RunContext) -> None:
    for spec_name in (
        "three_fence_admission.qnt",
        "placement_lease.qnt",
        "credential_lifecycle_restore.qnt",
        "stream_cancel_and_quarantine.qnt",
    ):
        spec = ctx.server_dir / "specs" / "quint" / spec_name
        res = ctx.run_quint([
            "run",
            "--seed=0x42",
            "--max-samples=10",
            "--max-steps=0",
            "--invariant=safetyInv",
            str(spec),
        ])
        assert_eq(res.returncode, 0, f"quint zero-step boundary failed on {spec_name}: {res.combined[-500:]}")
        assert_in("[ok] No violation found", res.combined)


def _t2_m1_10_quint_deep_steps_boundary(ctx: RunContext) -> None:
    for spec_name in ("placement_lease.qnt", "credential_lifecycle_restore.qnt"):
        spec = ctx.server_dir / "specs" / "quint" / spec_name
        res = ctx.run_quint([
            "run",
            "--seed=0x42",
            "--max-samples=200",
            "--max-steps=50",
            "--invariant=safetyInv",
            str(spec),
        ])
        assert_eq(res.returncode, 0, f"quint 50-step boundary failed on {spec_name}: {res.combined[-500:]}")
        assert_in("[ok] No violation found", res.combined)


# ---------------------------------------------------------------------------
# M2 Boundaries (6 tests — FAIL until M2)
# ---------------------------------------------------------------------------

def _t2_m2_01_tenant_id_routing_boundaries(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass(
        "waddle-broker-core",
        "M2 BVA: empty/63/64/253/254B/uppercase/traversal TenantId rejected at domain-routing entry",
        test_filter="tenant_id_validation",
    )


def _t2_m2_02_turso_rlimit_nofile_boundary(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass(
        "waddle-broker-core",
        "M2 BVA: FdPoolAccounting enforces RLIMIT_NOFILE cap and rejects ZeroCapacity",
        test_filter="enforces_rlimit_and_never_evicts_pinned",
    )
    ctx.require_crate_tests_pass(
        "waddle-tenancy",
        "M2 BVA: Turso LRU FD pool enforces RLIMIT_NOFILE cap",
        test_filter="turso_lru_fd_pool_enforces_rlimit_and_checkpoints_on_eviction",
    )


def _t2_m2_03_lease_gen_u64_max_boundary(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass(
        "waddle-broker-core",
        "M2 BVA: three_fence_guard rejects u64::MAX fence generation wraparound",
        test_filter="three_fence_guard",
    )
    ctx.require_crate_tests_pass(
        "waddle-tenancy",
        "M2 BVA: PlacementLeaseStore monotone generation fencing",
        test_filter="passes_all_seven_scenarios",
        min_passed=2,
    )


def _t2_m2_04_clock_skew_boot_instant_boundary(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass(
        "waddle-broker-core",
        "M2 BVA: BootClock suspend-jump deadline enforcement",
        test_filter="boot_clock",
        min_passed=2,
    )
    ctx.require_crate_tests_pass(
        "waddle-tenancy",
        "M2 BVA: PlacementLeaseStore TTL=0 and expiry boundary checks",
        test_filter="passes_all_seven_scenarios",
        min_passed=2,
    )


def _t2_m2_05_wasm_memory_and_fuel_boundaries(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "wasm_memory_fuel_boundaries",
        "M2 BVA: Wasm memories * memory_bytes + 1 page fails cleanly; fuel=0 traps deterministically",
    )


def _t2_m2_06_arch_guard_negative_boundaries(ctx: RunContext) -> None:
    script = ctx.server_dir / "scripts" / "check-tenant-isolation.py"
    assert_file_exists(script, "M2 BVA: check-tenant-isolation.py negative self-test")
    res = ctx.run_cmd(["python3", str(script), "--self-test"])
    assert_eq(res.returncode, 0, f"check-tenant-isolation.py --self-test failed: {res.combined[-500:]}")


# ---------------------------------------------------------------------------
# M3 Boundaries (5 tests — FAIL until M3)
# ---------------------------------------------------------------------------

def _t2_m3_01_aead_bitflip_and_secret_size(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass("waddle-broker", "M3 BVA: 1-bit AEAD/AAD flip and 0-byte/oversized secret rejection", test_filter="aead_boundary")


def _t2_m3_02_xep0198_duplicate_vs_mutated_nonce(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "credential_iq_nonce_boundaries",
        "M3 BVA: XEP-0198 duplicate nonce idempotent; same nonce + mutated ciphertext rejected",
    )


def _t2_m3_03_malformed_credential_iq_xml(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "credential_iq_malformed_xml",
        "M3 BVA: malformed IQ XML, unknown urn:waddle version, and oversized stanza return opaque error",
    )


def _t2_m3_04_all_four_bounce_paths_nested(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "credential_iq_four_bounce_paths",
        "M3 BVA: route_to_connection, destination, full_jid_forward, muc_admin strip nested credential payloads",
    )


def _t2_m3_05_idp_jwt_and_subkey_expiry(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass("waddle-broker", "M3 BVA: expired/wrong-aud/alg-none IdP JWT and expired broker subkey rejected", test_filter="jwt_boundary")


# ---------------------------------------------------------------------------
# M4 Boundaries (5 tests — FAIL until M4)
# ---------------------------------------------------------------------------

def _t2_m4_01_spicedb_765_766_byte_limit(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "permission_spicedb_id_limit",
        "M4 BVA: SpiceDB object ID 765B accepted; 766B rejected before RPC",
    )


def _t2_m4_02_openfga_contextual_tuples_rejected(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "permission_openfga_contextual_tuples",
        "M4 BVA: OpenFGA rejects any non-empty contextual_tuples and prohibits MINIMIZE_LATENCY",
    )


def _t2_m4_03_three_fence_stale_minus_one(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass("waddle-broker", "M4 BVA: stale generation by -1 on any of the 3 fences denies admission", test_filter="stale_fence_boundary")


def _t2_m4_04_budget_cap_u64_max_overflow(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass("waddle-broker", "M4 BVA: u64::MAX budget cap and spend reservation overflow fail closed", test_filter="budget_overflow")


def _t2_m4_05_unknown_after_send_attempt_one(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "room_observation_quarantine_attempt_one",
        "M4 BVA: first unknown_after_send quarantines immediately even at attempt 1 < MAX_ATTEMPTS",
    )


# ---------------------------------------------------------------------------
# M5 Boundaries (5 tests — FAIL until M5)
# ---------------------------------------------------------------------------

def _t2_m5_01_ssrf_ipv4_mapped_and_redirect(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass("waddle-broker", "M5 BVA: SSRF IPv4-mapped IPv6/loopback/metadata/ULA/CGNAT and 302 redirect blocked", test_filter="ssrf_boundary")


def _t2_m5_02_stream_chunk_zero_and_max_bytes(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass("waddle-extensions", "M5 BVA: 0-byte, max-byte, and max+1 stream chunk boundaries", test_filter="chunk_boundary")


def _t2_m5_03_sync_drop_lifecycle_edges(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass("waddle-extensions", "M5 BVA: sync Drop before first chunk, mid-stream, and post-EOF is idempotent", test_filter="drop_boundary")


def _t2_m5_04_undeclared_wasi_import_rejected(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass("waddle-extensions", "M5 BVA: guest importing undeclared wasi:filesystem/sockets/cli fails instantiation", test_filter="wasi_narrow_boundary")


def _t2_m5_05_guest_stderr_sentinel_suppressed(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass("waddle-extensions", "M5 BVA: guest stderr/log sentinel produces 0 occurrences in host logs", test_filter="stderr_suppression")


# ---------------------------------------------------------------------------
# M6 Boundaries (4 tests — FAIL until M6)
# ---------------------------------------------------------------------------

def _t2_m6_01_private_output_rejected_member_room_ai(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "ai_private_output_rejection",
        "M6 BVA: 'private' output mode unconditionally rejected for member-funded room /ai",
    )


def _t2_m6_02_unqualified_expired_zdr_route_rejected(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass("waddle-broker", "M6 BVA: zdr=false, data_collection=allow, or expired ZDR route fails closed", test_filter="zdr_boundary")


def _t2_m6_03_host_query_mam_cross_room_dm_rejected(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "host_query_mam_source_room_boundary",
        "M6 BVA: host query_mam rejects room != source_room and 1:1 DM MamTarget::Conversation",
    )


def _t2_m6_04_helm_validations_reject_legacy_api_key(ctx: RunContext) -> None:
    vals = strip_comments(
        ctx.read_text("server/charts/waddle-server/templates/validations.yaml"),
        "hash",
    )
    assert_in(
        'eq $name "ai-chatbot"',
        vals,
        "M6 BVA: validations.yaml must retain ai-chatbot validation block",
    )
    assert_not_in(
        "ai-chatbot requires config.api_key",
        vals,
        "M6 BVA: validations.yaml must reject (not require) legacy api_key on ai-chatbot",
    )
    assert_not_in(
        "allowedHttpOrigins is required for ai-chatbot",
        vals,
        "M6 BVA: validations.yaml must remove legacy allowedHttpOrigins requirement for ai-chatbot",
    )
    assert_not_in(
        "allowedHttpOrigins must include the ai-chatbot provider origin",
        vals,
        "M6 BVA: validations.yaml must remove legacy allowedHttpOrigins provider origin rule for ai-chatbot",
    )
    assert_contains_regex(
        r"(?:broker|inference)[\s\S]{0,300}(?:fail\b|must not|forbidden|reject)|\bfail\b[\s\S]{0,300}(?:broker|inference|must not|forbidden)",
        vals,
        "M6 BVA: validations.yaml must enforce brokered inference guardrails",
        flags=re.IGNORECASE,
    )


def get_tests() -> list[TestCase]:
    return [
        # M1 (10)
        TestCase("T2-M1-01", 2, "M1", ("F4",), ("Gate1",), "Identity BVA: TenantId 253/254B, CellRole 63/64B, TlsExporterProof 128/129B", _t2_m1_01_identity_boundaries),
        TestCase("T2-M1-02", 2, "M1", ("F2",), ("Gate3",), "AAD codec BVA: length-prefixed framing prevents boundary shift collisions", _t2_m1_02_aad_codec_boundaries),
        TestCase("T2-M1-03", 2, "M1", ("F2",), ("Gate2",), "Budget ledger BVA: u64::MAX overflow, 0-cost reserve, actual > reserved underflow", _t2_m1_03_budget_u64_max_boundaries),
        TestCase("T2-M1-04", 2, "M1", ("F2",), ("Gate2",), "Three-fence guard BVA: u64::MAX generation overflow & now == lease_expiry_ms", _t2_m1_04_three_fence_guard_boundaries),
        TestCase("T2-M1-05", 2, "M1", ("F2",), ("Gate3", "Gate4"), "SSRF IP filter BVA: IPv4-mapped IPv6, loopback, metadata, ULA, CGNAT CIDR edges", _t2_m1_05_ssrf_ip_cidr_boundaries),
        TestCase("T2-M1-06", 2, "M1", ("F2", "F5"), ("Gate1",), "LRU FD pool BVA: ZeroCapacity rejection, eviction at cap+1, pinned-handle guard", _t2_m1_06_lru_fd_pool_boundaries),
        TestCase("T2-M1-07", 2, "M1", ("F3",), ("Gate2", "Gate4"), "trace_verifier BVA: deny_unknown_fields, GhostTransfer, UncanceledStreamAtEof", _t2_m1_07_trace_verifier_boundaries),
        TestCase("T2-M1-08", 2, "M1", ("F2",), ("Gate1",), "Verus BVA: zero assume()/admit()/external_body across all 5 proof files", _t2_m1_08_verus_zero_bypass_boundary),
        TestCase("T2-M1-09", 2, "M1", ("F1",), ("Gate2",), "Quint BVA: zero-step (--max-steps=0) initial-state safetyInv on all 4 specs", _t2_m1_09_quint_zero_step_boundary),
        TestCase("T2-M1-10", 2, "M1", ("F1",), ("Gate2", "Gate5"), "Quint BVA: deep 50-step exploration on placement_lease & credential_lifecycle_restore", _t2_m1_10_quint_deep_steps_boundary),
        # M2 (6)
        TestCase("T2-M2-01", 2, "M2", ("F6",), ("Gate1",), "TenantId BVA: empty/63/64/253/254B/uppercase/traversal rejected at routing entry", _t2_m2_01_tenant_id_routing_boundaries),
        TestCase("T2-M2-02", 2, "M2", ("F7",), ("Gate1",), "Turso LRU FD pool BVA: RLIMIT_NOFILE cap & ZeroCapacity rejection", _t2_m2_02_turso_rlimit_nofile_boundary),
        TestCase("T2-M2-03", 2, "M2", ("F10",), ("Gate2",), "PlacementLeaseStore BVA: u64::MAX generation overflow fails closed", _t2_m2_03_lease_gen_u64_max_boundary),
        TestCase("T2-M2-04", 2, "M2", ("F10",), ("Gate2",), "Lease expiry BVA: ttl=0, now == expires_at +/- 1, clock skew & BootInstant jump", _t2_m2_04_clock_skew_boot_instant_boundary),
        TestCase("T2-M2-05", 2, "M2", ("F9",), ("Gate1",), "Wasm bounds BVA: memories * memory_bytes + 1 page fails; fuel=0 traps", _t2_m2_05_wasm_memory_and_fuel_boundaries),
        TestCase("T2-M2-06", 2, "M2", ("F9",), ("Gate1",), "Arch guard BVA: check-tenant-isolation.py --self-test catches edge-case bypasses", _t2_m2_06_arch_guard_negative_boundaries),
        # M3 (5)
        TestCase("T2-M3-01", 2, "M3", ("F11",), ("Gate3",), "AEAD BVA: 1-bit tag/AAD flip and 0-byte/oversized secret rejected", _t2_m3_01_aead_bitflip_and_secret_size),
        TestCase("T2-M3-02", 2, "M3", ("F13",), ("Gate2",), "XEP-0198 BVA: duplicate nonce idempotent; mutated ciphertext with same nonce rejected", _t2_m3_02_xep0198_duplicate_vs_mutated_nonce),
        TestCase("T2-M3-03", 2, "M3", ("F13",), ("Gate4",), "Credential IQ BVA: malformed XML & unknown version return opaque error", _t2_m3_03_malformed_credential_iq_xml),
        TestCase("T2-M3-04", 2, "M3", ("F13",), ("Gate4",), "Error bounce BVA: all 4 bounce paths strip nested credential payloads", _t2_m3_04_all_four_bounce_paths_nested),
        TestCase("T2-M3-05", 2, "M3", ("F12",), ("Gate3",), "IdP/subkey BVA: expired/wrong-aud/alg-none JWT & expired broker subkey rejected", _t2_m3_05_idp_jwt_and_subkey_expiry),
        # M4 (5)
        TestCase("T2-M4-01", 2, "M4", ("F14",), ("Gate1",), "SpiceDB BVA: 765B object ID accepted; 766B rejected before RPC", _t2_m4_01_spicedb_765_766_byte_limit),
        TestCase("T2-M4-02", 2, "M4", ("F14",), ("Gate3",), "OpenFGA BVA: contextual_tuples rejected & MINIMIZE_LATENCY prohibited", _t2_m4_02_openfga_contextual_tuples_rejected),
        TestCase("T2-M4-03", 2, "M4", ("F15",), ("Gate2",), "3-Fence BVA: stale generation by -1 on any of the 3 fences denies admission", _t2_m4_03_three_fence_stale_minus_one),
        TestCase("T2-M4-04", 2, "M4", ("F15",), ("Gate2",), "Budget BVA: u64::MAX cap and spend reservation overflow fail closed", _t2_m4_04_budget_cap_u64_max_overflow),
        TestCase("T2-M4-05", 2, "M4", ("F16",), ("Gate2",), "Quarantine BVA: first unknown_after_send quarantines at attempt 1 < MAX_ATTEMPTS", _t2_m4_05_unknown_after_send_attempt_one),
        # M5 (5)
        TestCase("T2-M5-01", 2, "M5", ("F17",), ("Gate3", "Gate4"), "SSRF BVA: IPv4-mapped IPv6/loopback/metadata/ULA/CGNAT & 302 redirect blocked", _t2_m5_01_ssrf_ipv4_mapped_and_redirect),
        TestCase("T2-M5-02", 2, "M5", ("F18",), ("Gate4",), "Stream chunk BVA: 0-byte, max-byte, and max+1 byte chunks handled safely", _t2_m5_02_stream_chunk_zero_and_max_bytes),
        TestCase("T2-M5-03", 2, "M5", ("F18",), ("Gate2",), "Sync Drop BVA: InferenceStream Drop before 1st chunk/mid-stream/post-EOF idempotent", _t2_m5_03_sync_drop_lifecycle_edges),
        TestCase("T2-M5-04", 2, "M5", ("F18",), ("Gate4",), "WASI linker BVA: undeclared wasi:filesystem/sockets/cli import fails instantiation", _t2_m5_04_undeclared_wasi_import_rejected),
        TestCase("T2-M5-05", 2, "M5", ("F18",), ("Gate4",), "Stderr BVA: guest stderr/log sentinel produces 0 occurrences in host logs", _t2_m5_05_guest_stderr_sentinel_suppressed),
        # M6 (4)
        TestCase("T2-M6-01", 2, "M6", ("F20",), ("Gate3",), "Room /ai BVA: 'private' output rejected for member-funded room /ai", _t2_m6_01_private_output_rejected_member_room_ai),
        TestCase("T2-M6-02", 2, "M6", ("F20",), ("Gate3",), "ZDR route BVA: unqualified or expired ZDR route fails closed", _t2_m6_02_unqualified_expired_zdr_route_rejected),
        TestCase("T2-M6-03", 2, "M6", ("F20",), ("Gate3", "Gate4"), "Host query_mam BVA: room != source_room and 1:1 DM target rejected", _t2_m6_03_host_query_mam_cross_room_dm_rejected),
        TestCase("T2-M6-04", 2, "M6", ("F21",), ("Gate4",), "Helm validations BVA: rejects legacy api_key & allowedHttpOrigins on ai-chatbot", _t2_m6_04_helm_validations_reject_legacy_api_key),
    ]
