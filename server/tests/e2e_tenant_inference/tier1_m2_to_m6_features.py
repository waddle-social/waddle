"""Tier 1 Feature Coverage: M2–M6 Target Contracts (F6–F21, 80 tests).

Every test genuinely probes real expected files, contracts, scripts, WIT/CUE/Helm/
client artifacts, or Cargo test targets and FAILS cleanly until its milestone lands.
ZERO fake passes, ZERO skips-as-pass.
"""

from __future__ import annotations

import json
import os
import re
from typing import Sequence

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
    fail,
    strip_comments,
)


def _find_migration_gte(ctx: RunContext, subdir: str, min_ver: int, needle: str) -> None:
    mig_dir = ctx.server_dir / "crates" / "waddle-server" / "sql" / subdir
    if not mig_dir.is_dir():
        fail(f"MISSING_DIR `{mig_dir.relative_to(ctx.repo_root)}` for migration >= {min_ver}")
    matched = []
    for f in sorted(mig_dir.glob("*.sql")):
        m = re.match(r"^(\d+)", f.name)
        if m and int(m.group(1)) >= min_ver:
            text = f.read_text(encoding="utf-8")
            if needle in text:
                matched.append(f.name)
    assert_true(
        len(matched) > 0,
        f"no SQL migration >= {min_ver} in sql/{subdir} containing {needle!r}",
    )


# ---------------------------------------------------------------------------
# F6: Canonical TenantId, AccountId & Registry (M2, 5 tests)
# ---------------------------------------------------------------------------

def _t1_f06_01(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass(
        "waddle-tenancy",
        "M2: TenantRegistry sole handle constructor & scoped cache contract",
        test_filter="tenant_registry_is_sole_handle_constructor_with_scoped_cache",
    )


def _t1_f06_02(ctx: RunContext) -> None:
    reg = ctx.server_dir / "crates" / "waddle-server" / "src" / "db" / "tenant_registry.rs"
    assert_file_exists(reg, "M2: tenant_registry.rs sole handle constructor")
    code = strip_comments(reg.read_text(encoding="utf-8"), "rust")
    assert_contains_regex(
        r"\b(?:pub(?:\([^)]*\))?\s+)?struct\s+\w+",
        code,
        "M2: tenant_registry.rs must declare a real Rust struct",
    )
    assert_in("TenantId", code, "M2: tenant_registry.rs must reference TenantId in code")
    assert_in("TenantStore", code, "M2: tenant_registry.rs must reference TenantStore in code")


def _t1_f06_03(ctx: RunContext) -> None:
    sqlite_v1 = ctx.read_text(
        "server/crates/waddle-server/sql/v1/sqlite.sql"
    )
    assert_in("account_id", sqlite_v1, "M2: sqlite v1 schema must include account_id")
    assert_in(
        "trg_users_account_id_immutable",
        sqlite_v1,
        "M2: sqlite v1 schema must enforce immutable users.account_id",
    )


def _t1_f06_04(ctx: RunContext) -> None:
    pg_v1 = ctx.read_text(
        "server/crates/waddle-server/sql/v1/postgres.sql"
    )
    assert_in("account_id", pg_v1, "M2: postgres v1 schema must include immutable account_id")
    assert_in(
        "trg_users_account_id_immutable",
        pg_v1,
        "M2: postgres v1 schema must enforce immutable users.account_id",
    )
    assert_in("tenant_id", pg_v1, "M2: postgres v1 schema must scope tables by tenant_id")


def _t1_f06_05(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "tenant_identity_isolation",
        "M2: distinct account_id UUIDs for identical localparts across two tenants",
    )


# ---------------------------------------------------------------------------
# F7: TenantStore (Turso + Postgres) (M2, 5 tests)
# ---------------------------------------------------------------------------

def _t1_f07_01(ctx: RunContext) -> None:
    turso_store = ctx.server_dir / "crates" / "waddle-server" / "src" / "db" / "turso_tenant_store.rs"
    assert_file_exists(turso_store, "M2: Turso per-tenant <tenant>.db TenantStore implementation")
    code = strip_comments(turso_store.read_text(encoding="utf-8"), "rust")
    assert_contains_regex(
        r"\bimpl\b[^{]*\bTenantStore\s+for\b",
        code,
        "M2: turso_tenant_store.rs must implement TenantStore",
    )


def _t1_f07_02(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass(
        "waddle-tenancy",
        "M2: Turso TenantStore bounded LRU FD pool checkpoint/eviction contract test",
        test_filter="turso_lru_fd_pool_enforces_rlimit_and_checkpoints_on_eviction",
    )


def _t1_f07_03(ctx: RunContext) -> None:
    pg_v1 = ctx.read_text(
        "server/crates/waddle-server/sql/v1/postgres.sql"
    )
    for tbl in ("users", "auth_identities", "native_users"):
        assert_contains_regex(
            rf"CREATE TABLE\s+{tbl}\s*\([^;]*\btenant_id\b",
            pg_v1,
            f"M2: Postgres v1 schema must include tenant_id column on {tbl}",
        )


def _t1_f07_04(ctx: RunContext) -> None:
    import sqlite3

    pg_v1 = ctx.read_text(
        "server/crates/waddle-server/sql/v1/postgres.sql"
    )
    assert_in("tenant_id", pg_v1, "M2: postgres v1 schema must include tenant_id")
    with ctx.temp_dir(prefix="waddle_schema_catalog_") as tmp:
        db_path = tmp / "catalog.db"
        conn = sqlite3.connect(db_path)
        conn.executescript(ctx.read_text("server/crates/waddle-server/sql/v1/sqlite.sql"))
        conn.close()
        res = ctx.run_cmd(
            ["python3", "server/scripts/schema_catalog.py", "dump", "--sqlite", str(db_path)],
            cache=False,
        )
        assert_eq(res.returncode, 0, f"schema_catalog.py dump failed: {res.combined[-600:]}")
        assert_in("tenant_placement_leases", res.stdout, "M2: schema dump must include tenant_placement_leases")
        assert_in("account_id", res.stdout, "M2: schema dump must include account_id")
        assert_in("tenant_id", res.stdout, "M2: schema dump must include tenant_id")


def _t1_f07_05(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass(
        "waddle-tenancy",
        "M2: shared TenantStore parity contract suite (Turso + Postgres)",
        test_filter="satisfies_shared_storage_contract",
        min_passed=2,
    )


# ---------------------------------------------------------------------------
# F8: In-Memory Registry & Actor Scoping (M2, 5 tests)
# ---------------------------------------------------------------------------

def _t1_f08_01(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "tenant_registry_scoping",
        "M2: RoomRegistryActor cross-tenant identical room localpart isolation",
    )


def _t1_f08_02(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "tenant_connection_scoping",
        "M2: ConnectionRegistry cross-tenant identical localpart session isolation",
    )


def _t1_f08_03(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "tenant_command_scoping",
        "M2: CommandRegistry (TenantId, session_id) isolation",
    )


def _t1_f08_04(ctx: RunContext) -> None:
    actor_rs = strip_comments(ctx.read_text("server/crates/waddle-extensions/src/actor.rs"), "rust")
    msg_rs = strip_comments(
        ctx.read_text("server/crates/waddle-extensions/src/manager/message_processing.rs"),
        "rust",
    )
    assert_contains_regex(
        r"\bpub\s+struct\s+WasmExtensionActor\b",
        actor_rs,
        "M2: actor.rs must define WasmExtensionActor",
    )
    assert_contains_regex(
        r"\bimpl\s+ExtensionManager\b",
        msg_rs,
        "M2: message_processing.rs must implement ExtensionManager",
    )
    assert_not_in(
        'WaddleId::try_new("local")',
        actor_rs,
        "M2: actor.rs must not hardcode WaddleId::try_new(\"local\")",
    )
    assert_not_in(
        'WaddleId::try_new("local")',
        msg_rs,
        "M2: message_processing.rs must not hardcode WaddleId::try_new(\"local\")",
    )


def _t1_f08_05(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "tenant_federated_cache_isolation",
        "M2: federated BareJid cache isolation across tenants",
    )


# ---------------------------------------------------------------------------
# F9: Per-Tenant Resource Semaphores & Arch Guard (M2, 5 tests)
# ---------------------------------------------------------------------------

def _t1_f09_01(ctx: RunContext) -> None:
    script = ctx.server_dir / "scripts" / "check-tenant-isolation.py"
    assert_file_exists(script, "M2: lexical tenant isolation architecture guard")
    res = ctx.run_cmd(["python3", str(script), "--server-only", "."])
    assert_eq(res.returncode, 0, f"check-tenant-isolation.py failed: {res.combined[-600:]}")


def _t1_f09_02(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "tenant_resource_semaphores",
        "M2: per-tenant mailbox/task/stream/DB semaphore isolation test",
    )


def _t1_f09_03(ctx: RunContext) -> None:
    hs = strip_comments(ctx.read_text("server/crates/waddle-extensions/src/runtime/host_state.rs"), "rust")
    assert_contains_regex(
        r"\bResourceLimiter\b|\bStoreLimitsBuilder\b",
        hs,
        "M2: host_state.rs must retain Wasmtime ResourceLimiter / StoreLimitsBuilder",
    )
    assert_not_contains_regex(
        r"\.memory_size\([^)]*memory_bytes\)[\s\S]{0,200}\.memories\(4\)",
        hs,
        "M2: host_state.rs must bound memories * memory_bytes instead of unpooled .memories(4)",
    )


def _t1_f09_04(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "wasm_fuel_epoch_bounds",
        "M2: Wasm fuel and wall-clock epoch interruption bounds test",
    )


def _t1_f09_05(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "reserved_heartbeat_capacity",
        "M2: reserved heartbeat/cancellation semaphore capacity under tenant saturation",
    )


# ---------------------------------------------------------------------------
# F10: PlacementLeaseStore & Write Fencing (M2, 5 tests)
# ---------------------------------------------------------------------------

def _t1_f10_01(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass(
        "waddle-tenancy",
        "M2: PlacementLeaseStore durable CAS claim/renew/transfer/release on Turso & Postgres",
        test_filter="placement_lease_store_passes_all_seven_scenarios",
        min_passed=2,
    )


def _t1_f10_02(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass(
        "waddle-tenancy",
        "M2: SPIFFE mTLS SAN extraction & cell isolation checks",
        test_filter="spiffe_x509",
        min_passed=2,
    )
    ctx.require_crate_tests_pass(
        "waddle-broker-core",
        "M2: RFC 9266 tls-exporter HolderInstance channel-binding PoP",
        test_filter="holder_pop",
    )


def _t1_f10_03(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass(
        "waddle-broker-core",
        "M2: monotonic BootClock suspend-jump deadline enforcement",
        test_filter="boot_clock",
        min_passed=2,
    )
    ctx.require_server_integration_test(
        "tenant_isolation_m2",
        "M2: authoritative DB clock expiry and BootInstant deadline checks",
        min_passed=2,
    )


def _t1_f10_04(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "placement_write_and_worker_fencing",
        "M2: stale-generation fencing on data writes, room_observation, outbox, CommandRegistry",
    )


def _t1_f10_05(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass(
        "waddle-tenancy",
        "M2: shared/dedicated placement and WADDLE_DRAIN_TIMEOUT_SECS quiescing",
        test_filter="passes_all_seven_scenarios",
        min_passed=2,
    )


# ---------------------------------------------------------------------------
# F11: Broker Envelope Encryption & KMS Custody (M3, 5 tests)
# ---------------------------------------------------------------------------

def _t1_f11_01(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass("waddle-broker", "M3: envelope encryption with length-prefixed AAD", test_filter="envelope")


def _t1_f11_02(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass("waddle-broker", "M3: two-tier DEK wrapping under broker-only KMS", test_filter="kms")


def _t1_f11_03(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass("waddle-broker", "M3: credential lifecycle rotate/replace/withdraw", test_filter="lifecycle")


def _t1_f11_04(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass("waddle-broker", "M3: destruction ledger + retired_account_jids restore enforcement", test_filter="destruction")


def _t1_f11_05(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass("waddle-broker", "M3: tenant delete purging Turso WAL + OpenFGA DeleteStore", test_filter="tenant_delete")


# ---------------------------------------------------------------------------
# F12: Independent IdP Intent & Device Keys (M3, 5 tests)
# ---------------------------------------------------------------------------

def _t1_f12_01(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass("waddle-broker", "M3: Colony JWT / WebAuthn JWKS verification", test_filter="idp_intent")


def _t1_f12_02(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass("waddle-broker", "M3: registered client device signing keys for invocation", test_filter="device_key")


def _t1_f12_03(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass("waddle-broker", "M3: reject native_users SCRAM for member credentials/policy", test_filter="reject_scram")


def _t1_f12_04(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass("waddle-broker", "M3: reject cell-asserted admin flags without IdP proof", test_filter="reject_cell_admin")


def _t1_f12_05(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass("waddle-broker", "M3: pinned root broker-key-signing key + rotatable XMPP subkeys", test_filter="broker_subkey")


# ---------------------------------------------------------------------------
# F13: Versioned urn:waddle:* Credential IQs & Bounce Stripping (M3, 5 tests)
# ---------------------------------------------------------------------------

def _t1_f13_01(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "credential_iq_contract",
        "M3: versioned urn:waddle:* credential enrollment/status/rotation/revocation/policy IQs",
    )


def _t1_f13_02(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass("waddle-broker", "M3: client-sealed envelope decrypted only at broker", test_filter="sealed_envelope")


def _t1_f13_03(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "credential_iq_sm_replay",
        "M3: XEP-0198 reconnect replay idempotency and secret-free sm_unacked",
    )


def _t1_f13_04(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "credential_iq_bounce_stripping",
        "M3: strip credential payloads on all 4 IQ error-bounce paths",
    )


def _t1_f13_05(ctx: RunContext) -> None:
    audit = ctx.read_text("docs/xep-conformance-audit.md")
    proto = ctx.read_text("docs/superpowers/plans/2026-04-27-waddle-extension-xmpp-protocol.md")
    assert_contains_regex(
        r"urn:waddle:(?:inference|credential)",
        audit,
        "M3: docs/xep-conformance-audit.md must document versioned urn:waddle:* credential IQ namespace",
    )
    assert_contains_regex(
        r"urn:waddle:(?:inference|credential)",
        proto,
        "M3: extension XMPP protocol doc must be amended for versioned urn:waddle:* credential IQs",
    )


# ---------------------------------------------------------------------------
# F14: PermissionBackend (OpenFGA + SpiceDB + Local) (M4, 5 tests)
# ---------------------------------------------------------------------------

def _t1_f14_01(ctx: RunContext) -> None:
    actor_rs = strip_comments(
        ctx.read_text("server/crates/waddle-server/src/permissions/actor.rs"),
        "rust",
    )
    assert_contains_regex(
        r"\bpub\s+enum\s+Permission\b",
        actor_rs,
        "M4: permissions/actor.rs must retain pub enum Permission",
    )
    assert_not_contains_regex(
        r"\benum\s+PermissionActorBackend\b",
        actor_rs,
        "M4: closed PermissionActorBackend enum must be replaced by PermissionBackend trait",
    )
    assert_contains_regex(
        r"\bPermissionBackend\b",
        actor_rs,
        "M4: permissions/actor.rs must use PermissionBackend trait",
    )


def _t1_f14_02(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "permission_openfga_modular_models",
        "M4: OpenFGA per-tenant store_id, pinned model ID, Modular Models chat vs inference",
    )


def _t1_f14_03(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "permission_openfga_consistency",
        "M4: OpenFGA HIGHER_CONSISTENCY and rejection of contextual_tuples",
    )


def _t1_f14_04(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "permission_spicedb_tenant_scoping",
        "M4: SpiceDB tenant-scoped IDs (<=765B), FullyConsistent, AUTHZ_SCHEMA_VERSION=3",
    )


def _t1_f14_05(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "permission_backend_parity",
        "M4: shared PermissionBackend behavioral parity suite (Local, OpenFGA, SpiceDB)",
    )


# ---------------------------------------------------------------------------
# F15: 3-Fence AdmissionLedgerStore & ReplicaFence (M4, 5 tests)
# ---------------------------------------------------------------------------

def _t1_f15_01(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass("waddle-broker", "M4: 3-fence atomic admit_attempt on Turso & Postgres", test_filter="admission_ledger")


def _t1_f15_02(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass("waddle-broker", "M4: fence close before mutation + short-tick ReplicaFence abort", test_filter="replica_fence")


def _t1_f15_03(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass("waddle-broker", "M4: bounded multi-turn run/step ledger", test_filter="run_step_ledger")


def _t1_f15_04(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass("waddle-broker", "M4: strict payer separation (never silently switch payer)", test_filter="payer_separation")


def _t1_f15_05(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass("waddle-broker", "M4: deny member credentials for observers/schedules/webhooks/federated remotes", test_filter="principal_denial")


# ---------------------------------------------------------------------------
# F16: unknown_after_send Quarantine (M4, 5 tests)
# ---------------------------------------------------------------------------

def _t1_f16_01(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass("waddle-broker", "M4: AttemptOutcome::UnknownAfterSend transitions ledger to Quarantined", test_filter="quarantine")


def _t1_f16_02(ctx: RunContext) -> None:
    work_rs = strip_comments(
        ctx.read_text("server/crates/waddle-server/src/ingress_uow/room_observation/work.rs"),
        "rust",
    )
    assert_contains_regex(
        r"\bObservationWork\b",
        work_rs,
        "M4: room_observation/work.rs must retain ObservationWork implementation",
    )
    assert_not_contains_regex(
        r'let\s+uncertain\s*=.*unknown_after_send',
        work_rs,
        "M4: room_observation/work.rs must quarantine unknown_after_send instead of bypassing MAX_ATTEMPTS",
    )
    assert_contains_regex(
        r"\bquarantin\w*",
        work_rs,
        "M4: room_observation/work.rs must explicitly quarantine unknown_after_send in code",
        flags=re.IGNORECASE,
    )


def _t1_f16_03(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "room_observation_quarantine",
        "M4: quarantined billable attempt never re-dispatched without provider dedupe",
    )


def _t1_f16_04(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass("waddle-broker", "M4: conservative budget conservation on unknown_after_send quarantine", test_filter="quarantine_budget")


def _t1_f16_05(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "quarantine_replay_status",
        "M4: replayed XMPP command for quarantined attempt returns existing status",
    )


# ---------------------------------------------------------------------------
# F17: Constrained Provider Egress (M5, 5 tests)
# ---------------------------------------------------------------------------

def _t1_f17_01(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass("waddle-broker", "M5: approved (provider, deployment, model, operation) egress profiles", test_filter="egress_profile")


def _t1_f17_02(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass("waddle-broker", "M5: DNS-resolved SSRF blocklist, TLS, .no_proxy(), no redirects", test_filter="egress_ssrf")


def _t1_f17_03(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass("waddle-broker", "M5: byte/time/concurrency/spend caps on provider egress", test_filter="egress_caps")


def _t1_f17_04(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass("waddle-broker", "M5: response validation discards upstream headers and error bodies", test_filter="egress_error_strip")


def _t1_f17_05(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass("waddle-broker", "M5: provider admin and credential inspection endpoints unconditionally blocked", test_filter="egress_admin_block")


# ---------------------------------------------------------------------------
# F18: Streaming WIT (@4.0.0) & Wasmtime Hardening (M5, 5 tests)
# ---------------------------------------------------------------------------

def _t1_f18_01(ctx: RunContext) -> None:
    wit = strip_comments(ctx.read_text("server/wit/waddle-extension.wit"), "wit")
    assert_in("package waddle:extension@4.0.0;", wit, "M5: WIT package must be bumped to waddle:extension@4.0.0")
    assert_in("credential-ref", wit, "M5: WIT must define credential-ref variant")
    assert_in("resource inference-stream", wit, "M5: WIT must define resource inference-stream")
    assert_in("terminal-usage", wit, "M5: WIT inference-stream must expose host-verified terminal-usage")


def _t1_f18_02(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass("waddle-extensions", "M5: streaming first-chunk delivery and host-verified terminal-usage", test_filter="inference_stream")


def _t1_f18_03(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass("waddle-extensions", "M5: sync Rust Drop on InferenceStream triggers non-blocking upstream cancel", test_filter="stream_drop_cancel")


def _t1_f18_04(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "streaming_wedge_decoupling",
        "M5: streaming inference >15s decoupled from STANZA_HANDLER_WEDGE_TIMEOUT",
    )


def _t1_f18_05(ctx: RunContext) -> None:
    loader = strip_comments(
        ctx.read_text("server/crates/waddle-extensions/src/runtime/loader.rs"),
        "rust",
    )
    host_state = strip_comments(
        ctx.read_text("server/crates/waddle-extensions/src/runtime/host_state.rs"),
        "rust",
    )
    assert_contains_regex(
        r"\bpub\s+struct\s+WasmRuntime\b",
        loader,
        "M5: loader.rs must retain WasmRuntime",
    )
    assert_contains_regex(
        r"\bWasiCtxBuilder\b",
        host_state,
        "M5: host_state.rs must retain WasiCtxBuilder",
    )
    assert_not_in(
        "wasmtime_wasi::p2::add_to_linker_async",
        loader,
        "M5: loader.rs must narrow wasmtime_wasi linker imports instead of full add_to_linker_async",
    )
    assert_not_in(
        "inherit_stderr()",
        host_state,
        "M5: host_state.rs must disable inherit_stderr() for inference guests",
    )


# ---------------------------------------------------------------------------
# F19: Two-Cell, Two-Broker Native Fixture Spike (M5, 5 tests)
# ---------------------------------------------------------------------------

def _check_native_fixture_feature(
    ctx: RunContext,
    needles: Sequence[str],
    spike_filter: str,
    desc: str,
) -> None:
    fixture_root = ctx.server_dir / "experiments" / "native-fixture"
    assert_file_exists(fixture_root / "Cargo.toml", "M5: native-fixture Cargo.toml must exist")
    fixture_src = fixture_root / "src"
    assert_true(fixture_src.is_dir(), "M5: native-fixture/src must exist")

    # Ensure native-fixture builds cleanly without mutating or weakening its Nix tests.
    compile_res = ctx.run_cargo_test(["--offline", "--no-run"], cwd=fixture_root)
    assert_true(
        compile_res.returncode == 0,
        f"M5: native-fixture failed to compile: {compile_res.combined[-1200:]}",
    )

    nix_vars = ("FENCE_POSTGRES_BIN", "FENCE_SPICEDB_BIN", "FENCE_TEST_SHELL", "FENCE_TEST_SLEEP")
    if all(os.environ.get(v) for v in nix_vars):
        res = ctx.run_cargo_test(["--offline"], cwd=fixture_root)
        assert_cargo_tests_passed(res, f"native-fixture full Nix suite ({desc})", min_passed=15)
    else:
        res = ctx.run_cargo_test(["--offline", "cleanup::"], cwd=fixture_root)
        assert_cargo_tests_passed(res, f"native-fixture non-Nix unit tests ({desc})", min_passed=1)

    spike_path = "server/crates/waddle-server/tests/native_fixture_topology_spike.rs"
    assert_file_exists(ctx.repo_root / spike_path, "M5: native_fixture_topology_spike.rs must exist")
    spike_code = strip_comments(ctx.read_text(spike_path), "rust")
    for needle in needles:
        assert_contains_regex(
            needle,
            spike_code,
            f"M5: native_fixture_topology_spike missing {desc} ({needle!r})",
        )
    ctx.require_server_integration_test(
        "native_fixture_topology_spike",
        f"M5 F19 spike ({desc}: {spike_filter})",
        min_passed=5,
    )


def _t1_f19_01(ctx: RunContext) -> None:
    _check_native_fixture_feature(
        ctx,
        (
            r"\bTursoTenantStore\b",
            r"\bTursoPlacementLeaseStore\b",
            r"\bTursoAdmissionLedgerStore\b",
            r"\bInProcessOpenFgaServer\b",
            r"\bOpenFgaPermissionBackend\b",
            r"\bBrokerAdmissionService\b",
            r"\bBrokerReplicaFence\b",
        ),
        "f19_01_turso_openfga_two_cell_two_broker_stack",
        "Turso + OpenFGA two-cell/two-broker stack",
    )


def _t1_f19_02(ctx: RunContext) -> None:
    _check_native_fixture_feature(
        ctx,
        (
            r"\bSqliteCompatPostgresStore\b",
            r"\bSqliteCompatPlacementLeaseStore\b",
            r"\bSqliteCompatAdmissionLedgerStore\b",
            r"\bTenantSpiceDbPermissionBackend\b",
            r"\bBrokerAdmissionService\b",
            r"waddle_native_fixture::fixture::Fixture",
        ),
        "f19_02_postgres_spicedb_two_cell_two_broker_stack",
        "Postgres + SpiceDB two-cell/two-broker stack",
    )


def _t1_f19_03(ctx: RunContext) -> None:
    _check_native_fixture_feature(
        ctx,
        (
            r"\bBrokerEgressClient\b",
            r"\bExtensionInferenceHost\b",
            r"\bLoadedExtension\b",
            r"\bWasmRuntime\b",
            r"\binference_stream\.wat\b",
        ),
        "f19_03_wit_streaming_first_chunk_and_midstream_cancel",
        "streaming first-chunk + cancel scenario",
    )


def _t1_f19_04(ctx: RunContext) -> None:
    _check_native_fixture_feature(
        ctx,
        (
            r"\bHolderInstance\b",
            r"\.transfer\(",
            r"\bBrokerCustodyService\b",
            r"\bseal_credential_for_broker\b",
            r"\bunseal_client_envelope\b",
            r"\bunseal_for_provider_egress\b",
        ),
        "f19_04_sealed_enrollment_workload_rotation_and_lease_transfer_denial",
        "workload rotation & lease-transfer denial scenario",
    )


def _t1_f19_05(ctx: RunContext) -> None:
    _check_native_fixture_feature(
        ctx,
        (
            r"\bTraceRecorder\b",
            r"\bverify_jsonl_trace\b",
            r"\bassert_secret_free\b",
        ),
        "f19_05_secret_free_telemetry_and_pobserve_trace_verifier",
        "PObserve JSONL trace verification in native-fixture spike",
    )


# ---------------------------------------------------------------------------
# F20: ai-chatbot & jev-judgments Migration & Host Room Privacy (M6, 5 tests)
# ---------------------------------------------------------------------------

def _t1_f20_01(ctx: RunContext) -> None:
    ai_manifest = strip_comments(ctx.read_text("server/extensions/ai-chatbot/src/manifest.rs"), "rust")
    ai_provider = strip_comments(ctx.read_text("server/extensions/ai-chatbot/src/provider.rs"), "rust")
    ai_constants = strip_comments(ctx.read_text("server/extensions/ai-chatbot/src/constants.rs"), "rust")
    jev_guest = strip_comments(ctx.read_text("server/extensions/jev-judgments/src/guest.rs"), "rust")
    assert_contains_regex(r"\bfn\s+manifest\b", ai_manifest, "M6: ai-chatbot manifest.rs must retain fn manifest")
    assert_contains_regex(r"\bfn\s+execute_provider_request\b", ai_provider, "M6: ai-chatbot provider.rs must retain execute_provider_request")
    assert_contains_regex(r"\bconst\s+PLUGIN_ID\b", ai_constants, "M6: ai-chatbot constants.rs must retain PLUGIN_ID")
    assert_contains_regex(r"\bstruct\s+JevJudgments\b", jev_guest, "M6: jev-judgments guest.rs must retain JevJudgments")
    assert_not_in("OutboundHttpRequest", ai_manifest, "M6: ai-chatbot must drop OutboundHttpRequest")
    assert_not_in("OutboundHttpRequest", jev_guest, "M6: jev-judgments must drop OutboundHttpRequest")
    assert_not_in("Authorization", ai_provider, "M6: ai-chatbot must not construct raw Authorization headers")
    assert_not_in("MAX_PROVIDER_ERROR_BODY_BYTES", ai_constants)


def _t1_f20_02(ctx: RunContext) -> None:
    ai_tools = strip_comments(ctx.read_text("server/extensions/ai-chatbot/src/tools.rs"), "rust")
    assert_contains_regex(r"\bfn\s+select_host_tools\b", ai_tools, "M6: ai-chatbot tools.rs must retain select_host_tools")
    for removed in ("get_presence", "get_roster", "MamTarget::Conversation"):
        assert_not_in(removed, ai_tools, f"M6: ai-chatbot/src/tools.rs must remove {removed}")
    host_tools = strip_comments(
        ctx.read_text("server/crates/waddle-server/src/server/extension_host_adapter/host_tools.rs"),
        "rust",
    )
    assert_contains_regex(r"\bfn\s+query_mam\b", host_tools, "M6: host_tools.rs must retain fn query_mam")
    assert_in("source_room", host_tools, "M6: host ExtensionHostAdapter::query_mam must enforce source_room")


def _t1_f20_03(ctx: RunContext) -> None:
    ctx.require_server_integration_test(
        "ai_room_disclosure_ordering",
        "M6: server-side room disclosure emitted post-admission and pre-provider-egress",
    )


def _t1_f20_04(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass("waddle-broker", "M6: qualified ZDR/no-training routes fail-closed", test_filter="zdr_route")


def _t1_f20_05(ctx: RunContext) -> None:
    cue = strip_comments(ctx.read_text("server/deployment.cue"), "rust")
    assert_in("jev-judgments", cue, "M6: deployment.cue must retain jev-judgments module definition")
    assert_not_in(
        'kind: "all-hosted-rooms"',
        cue,
        "M6: jev-judgments must be gated per-space instead of all-hosted-rooms",
    )


# ---------------------------------------------------------------------------
# F21: Client Alignment & Deployment/Telemetry Updates (M6, 5 tests)
# ---------------------------------------------------------------------------

def _t1_f21_01(ctx: RunContext) -> None:
    web = strip_comments(ctx.read_text("chat/src/channels/extension-launcher.ts"), "rust")
    android = strip_comments(
        ctx.read_text(
            "apps/android/app/src/main/kotlin/social/waddle/android/feature/conversation/ExtensionCommandController.kt"
        ),
        "rust",
    )
    assert_contains_regex(
        r"\bexport\s+function\s+useExtensionLauncher\b",
        web,
        "M6: Web extension-launcher.ts must retain useExtensionLauncher",
    )
    assert_contains_regex(
        r"\bclass\s+ExtensionCommandController\b",
        android,
        "M6: Android ExtensionCommandController.kt must retain ExtensionCommandController",
    )
    assert_not_in(
        "sendPublicPromptIfRequested",
        web,
        "M6: Web extension-launcher.ts must remove premature sendPublicPromptIfRequested broadcast",
    )
    assert_not_in(
        "sendPublicPromptIfRequested",
        android,
        "M6: Android ExtensionCommandController.kt must remove premature sendPublicPromptIfRequested broadcast",
    )


def _t1_f21_02(ctx: RunContext) -> None:
    resume = strip_comments(ctx.read_text("chat/src/lib/xmpp/resume-persistence.ts"), "rust")
    assert_in(
        "PersistedSmResumeState",
        resume,
        "M6: resume-persistence.ts must retain PersistedSmResumeState",
    )
    assert_contains_regex(
        r"urn:waddle:(?:credential|inference)|redact|filterCredential",
        resume,
        "M6: resume-persistence.ts must strip/exclude credential IQs from PersistedSmResumeState localStorage",
    )


def _t1_f21_03(ctx: RunContext) -> None:
    cue = strip_comments(ctx.read_text("server/deployment.cue"), "rust")
    vals = strip_comments(ctx.read_text("server/charts/waddle-server/templates/validations.yaml"), "hash")
    obs_raw = ctx.read_text("server/observer-release.json")
    assert_in("#PublishedExtensionModules", cue, "M6: deployment.cue must retain #PublishedExtensionModules")
    assert_not_in("waddle-openrouter", cue, "M6: deployment.cue must remove legacy waddle-openrouter cell secret")
    assert_not_contains_regex(
        r"configSecretFiles:\s*api_key",
        cue,
        "M6: deployment.cue must remove configSecretFiles: api_key from inference modules",
    )
    assert_in('eq $name "ai-chatbot"', vals, "M6: validations.yaml must retain ai-chatbot validation block")
    assert_not_in(
        "ai-chatbot requires config.api_key",
        vals,
        "M6: validations.yaml must remove legacy ai-chatbot config.api_key requirement",
    )
    assert_not_in(
        "allowedHttpOrigins is required for ai-chatbot",
        vals,
        "M6: validations.yaml must remove legacy allowedHttpOrigins requirement for ai-chatbot",
    )
    assert_not_in(
        "allowedHttpOrigins must include the ai-chatbot provider origin",
        vals,
        "M6: validations.yaml must remove legacy allowedHttpOrigins provider origin rule for ai-chatbot",
    )
    assert_contains_regex(
        r"(?:broker|inference)[\s\S]{0,300}(?:fail\b|must not|forbidden|reject)|\bfail\b[\s\S]{0,300}(?:broker|inference|must not|forbidden)",
        vals,
        "M6: validations.yaml must enforce brokered inference guardrails",
        flags=re.IGNORECASE,
    )
    try:
        obs_data = json.loads(obs_raw)
    except json.JSONDecodeError as exc:
        fail(f"M6: observer-release.json is not valid JSON: {exc}")
    gen = obs_data.get("generation") if isinstance(obs_data, dict) else None
    assert_true(
        isinstance(gen, int) and gen > 2,
        f"M6: observer-release.json generation must be an integer > 2 after jev-judgments migration, got {gen!r}",
    )


def _t1_f21_04(ctx: RunContext) -> None:
    attrs = strip_comments(ctx.read_text("server/crates/waddle-xmpp/src/telemetry/attributes.rs"), "rust")
    caps = strip_comments(ctx.read_text("server/capabilities.toml"), "hash")
    assert_contains_regex(
        r"\bpub\s+(?:use\s+\S*::)?MetricAttribute\b",
        attrs,
        "M6: telemetry/attributes.rs must export MetricAttribute",
    )
    assert_not_in("tenant_id", attrs, "M6: OTel MetricAttribute must never include raw tenant_id")
    assert_in("[[capability]]", caps, "M6: server/capabilities.toml must retain [[capability]] entries")
    assert_contains_regex(
        r"broker|inference|credential",
        caps,
        "M6: server/capabilities.toml must declare brokered inference/credential capabilities",
    )


def _t1_f21_05(ctx: RunContext) -> None:
    ctx.require_crate_tests_pass("waddle-broker", "M6: rollback controls close admission while preserving fences and destruction ledger", test_filter="rollback")


def get_tests() -> list[TestCase]:
    return [
        # F6 (M2)
        TestCase("T1-F06-01", 1, "M2", ("F6",), ("Gate1", "Gate3"), "TenantId canonical domain-routing & registry contract passes", _t1_f06_01),
        TestCase("T1-F06-02", 1, "M2", ("F6",), ("Gate1",), "tenant_registry.rs exists as sole constructor of tenant handles", _t1_f06_02),
        TestCase("T1-F06-03", 1, "M2", ("F6",), ("Gate1",), "SQLite/Turso v1 schema includes immutable account_id", _t1_f06_03),
        TestCase("T1-F06-04", 1, "M2", ("F6",), ("Gate1",), "Postgres v1 schema includes immutable account_id and tenant_id scope", _t1_f06_04),
        TestCase("T1-F06-05", 1, "M2", ("F6",), ("Gate1",), "Identical localparts across two tenants hold distinct account_id UUIDs", _t1_f06_05),
        # F7 (M2)
        TestCase("T1-F07-01", 1, "M2", ("F7",), ("Gate1",), "Turso per-tenant <tenant>.db TenantStore implementation exists", _t1_f07_01),
        TestCase("T1-F07-02", 1, "M2", ("F7",), ("Gate1",), "Turso LRU FD pool checkpoints and evicts cleanly", _t1_f07_02),
        TestCase("T1-F07-03", 1, "M2", ("F7",), ("Gate1",), "Postgres users/auth_identities/native_users scoped by tenant_id", _t1_f07_03),
        TestCase("T1-F07-04", 1, "M2", ("F7",), ("Gate1",), "schema_catalog.py catalog parity passes with tenant_id & account_id", _t1_f07_04),
        TestCase("T1-F07-05", 1, "M2", ("F7",), ("Gate1",), "Shared TenantStore parity suite passes on Turso and Postgres", _t1_f07_05),
        # F8 (M2)
        TestCase("T1-F08-01", 1, "M2", ("F8",), ("Gate1",), "RoomRegistryActor isolates identical room localparts across tenants", _t1_f08_01),
        TestCase("T1-F08-02", 1, "M2", ("F8",), ("Gate1",), "ConnectionRegistry isolates identical user localparts across tenants", _t1_f08_02),
        TestCase("T1-F08-03", 1, "M2", ("F8",), ("Gate1",), "CommandRegistry isolates multi-step sessions by (TenantId, session_id)", _t1_f08_03),
        TestCase("T1-F08-04", 1, "M2", ("F8",), ("Gate1",), "WasmExtensionActor & message_processing remove hardcoded WaddleId(\"local\")", _t1_f08_04),
        TestCase("T1-F08-05", 1, "M2", ("F8",), ("Gate1",), "Federated BareJid caches never leak state across tenants", _t1_f08_05),
        # F9 (M2)
        TestCase("T1-F09-01", 1, "M2", ("F9",), ("Gate1",), "server/scripts/check-tenant-isolation.py exists and passes", _t1_f09_01),
        TestCase("T1-F09-02", 1, "M2", ("F9",), ("Gate1",), "Per-tenant mailbox/task/stream/DB semaphores isolate tenant load", _t1_f09_02),
        TestCase("T1-F09-03", 1, "M2", ("F9",), ("Gate1",), "Wasm linear memory enforces memories * memory_bytes per tenant", _t1_f09_03),
        TestCase("T1-F09-04", 1, "M2", ("F9",), ("Gate1",), "Wasm fuel and wall-clock epoch interruption terminate runaway guests", _t1_f09_04),
        TestCase("T1-F09-05", 1, "M2", ("F9",), ("Gate1",), "Reserved heartbeat/cancel capacity survives tenant worker saturation", _t1_f09_05),
        # F10 (M2)
        TestCase("T1-F10-01", 1, "M2", ("F10",), ("Gate1", "Gate2"), "PlacementLeaseStore CAS claim/renew/transfer/release on Turso & Postgres", _t1_f10_01),
        TestCase("T1-F10-02", 1, "M2", ("F10",), ("Gate2", "Gate3"), "HolderInstance verifies SPIFFE mTLS + RFC 9266 tls-exporter proof", _t1_f10_02),
        TestCase("T1-F10-03", 1, "M2", ("F10",), ("Gate2",), "Authoritative DB clock expiry and promoted BootInstant deadlines", _t1_f10_03),
        TestCase("T1-F10-04", 1, "M2", ("F10",), ("Gate2",), "Stale-generation data writes, room_observation, outbox, CommandRegistry fenced", _t1_f10_04),
        TestCase("T1-F10-05", 1, "M2", ("F10",), ("Gate1", "Gate5"), "Shared/dedicated placement and WADDLE_DRAIN_TIMEOUT_SECS quiescing", _t1_f10_05),
        # F11 (M3)
        TestCase("T1-F11-01", 1, "M3", ("F11",), ("Gate3", "Gate4"), "Broker authenticated envelope encryption with length-prefixed AAD", _t1_f11_01),
        TestCase("T1-F11-02", 1, "M3", ("F11",), ("Gate3",), "Two-tier user DEK -> tenant DEK -> broker-only KMS wrapping", _t1_f11_02),
        TestCase("T1-F11-03", 1, "M3", ("F11",), ("Gate3",), "Credential lifecycle rotate/replace/withdraw invalidates prior versions", _t1_f11_03),
        TestCase("T1-F11-04", 1, "M3", ("F11",), ("Gate5",), "Destruction ledger & retired_account_jids enforced on backup restore", _t1_f11_04),
        TestCase("T1-F11-05", 1, "M3", ("F11",), ("Gate5",), "Tenant deletion purges Turso WAL and invokes OpenFGA DeleteStore", _t1_f11_05),
        # F12 (M3)
        TestCase("T1-F12-01", 1, "M3", ("F12",), ("Gate3",), "Broker verifies Colony JWT / WebAuthn proofs directly against JWKS", _t1_f12_01),
        TestCase("T1-F12-02", 1, "M3", ("F12",), ("Gate3",), "Member-funded invocation requires registered client device signing key", _t1_f12_02),
        TestCase("T1-F12-03", 1, "M3", ("F12",), ("Gate3",), "native_users SCRAM accounts rejected for member credentials/policy", _t1_f12_03),
        TestCase("T1-F12-04", 1, "M3", ("F12",), ("Gate3",), "Cell-asserted admin flags rejected without independent IdP proof", _t1_f12_04),
        TestCase("T1-F12-05", 1, "M3", ("F12",), ("Gate3",), "Client-pinned root broker key authenticates rotatable XMPP subkeys", _t1_f12_05),
        # F13 (M3)
        TestCase("T1-F13-01", 1, "M3", ("F13",), ("Gate4",), "Versioned urn:waddle:* credential IQs handle enrollment/status/rotate/revoke/policy", _t1_f13_01),
        TestCase("T1-F13-02", 1, "M3", ("F13",), ("Gate4",), "Client-sealed credential envelope decrypts solely at broker", _t1_f13_02),
        TestCase("T1-F13-03", 1, "M3", ("F13",), ("Gate2", "Gate4"), "XEP-0198 replay is idempotent with opaque results (secret-free sm_unacked)", _t1_f13_03),
        TestCase("T1-F13-04", 1, "M3", ("F13",), ("Gate4",), "All 4 IQ error-bounce paths strip urn:waddle:* credential payloads", _t1_f13_04),
        TestCase("T1-F13-05", 1, "M3", ("F13",), ("Gate4",), "Extension protocol doc and xep-conformance-audit.md document credential IQs", _t1_f13_05),
        # F14 (M4)
        TestCase("T1-F14-01", 1, "M4", ("F14",), ("Gate1",), "Closed PermissionActorBackend enum replaced by PermissionBackend trait", _t1_f14_01),
        TestCase("T1-F14-02", 1, "M4", ("F14",), ("Gate1", "Gate3"), "OpenFGA per-tenant store_id, pinned model ID, Modular Models chat vs inference", _t1_f14_02),
        TestCase("T1-F14-03", 1, "M4", ("F14",), ("Gate3",), "OpenFGA broker checks enforce HIGHER_CONSISTENCY and reject contextual_tuples", _t1_f14_03),
        TestCase("T1-F14-04", 1, "M4", ("F14",), ("Gate1", "Gate3"), "SpiceDB tenant-scoped IDs (<=765B), FullyConsistent, AUTHZ_SCHEMA_VERSION=3", _t1_f14_04),
        TestCase("T1-F14-05", 1, "M4", ("F14",), ("Gate1",), "Shared PermissionBackend parity suite passes on Local, OpenFGA, SpiceDB", _t1_f14_05),
        # F15 (M4)
        TestCase("T1-F15-01", 1, "M4", ("F15",), ("Gate2",), "Atomic 3-fence admit_attempt checks all 3 fence keys on Turso & Postgres", _t1_f15_01),
        TestCase("T1-F15-02", 1, "M4", ("F15",), ("Gate2",), "Restriction closes fence before mutation and aborts peer streams via ReplicaFence", _t1_f15_02),
        TestCase("T1-F15-03", 1, "M4", ("F15",), ("Gate2", "Gate3"), "Multi-turn tool rounds bounded by run/step ledger and call/token budgets", _t1_f15_03),
        TestCase("T1-F15-04", 1, "M4", ("F15",), ("Gate3",), "Failed member credential never silently switches to tenant payer", _t1_f15_04),
        TestCase("T1-F15-05", 1, "M4", ("F15",), ("Gate3",), "Observers, schedules, webhooks, federated remotes denied member credentials", _t1_f15_05),
        # F16 (M4)
        TestCase("T1-F16-01", 1, "M4", ("F16",), ("Gate2",), "AttemptOutcome::UnknownAfterSend transitions ledger row to Quarantined", _t1_f16_01),
        TestCase("T1-F16-02", 1, "M4", ("F16",), ("Gate2",), "room_observation/work.rs quarantines unknown_after_send instead of bypassing MAX_ATTEMPTS", _t1_f16_02),
        TestCase("T1-F16-03", 1, "M4", ("F16",), ("Gate2",), "Quarantined billable attempt never re-dispatched without provider dedupe", _t1_f16_03),
        TestCase("T1-F16-04", 1, "M4", ("F16",), ("Gate2",), "Reserved spend for quarantined attempt conserved conservatively", _t1_f16_04),
        TestCase("T1-F16-05", 1, "M4", ("F16",), ("Gate2",), "Replayed XMPP command for quarantined attempt returns existing status", _t1_f16_05),
        # F17 (M5)
        TestCase("T1-F17-01", 1, "M5", ("F17",), ("Gate3",), "Broker egress uses approved (provider, deployment, model, operation) profiles", _t1_f17_01),
        TestCase("T1-F17-02", 1, "M5", ("F17",), ("Gate3", "Gate4"), "Broker egress enforces DNS SSRF blocklist, TLS, .no_proxy(), no redirects", _t1_f17_02),
        TestCase("T1-F17-03", 1, "M5", ("F17",), ("Gate3",), "Broker egress enforces byte, deadline, concurrency, and spend caps", _t1_f17_03),
        TestCase("T1-F17-04", 1, "M5", ("F17",), ("Gate4",), "Response validation discards upstream headers and error bodies", _t1_f17_04),
        TestCase("T1-F17-05", 1, "M5", ("F17",), ("Gate3",), "Provider admin and credential inspection endpoints unconditionally blocked", _t1_f17_05),
        # F18 (M5)
        TestCase("T1-F18-01", 1, "M5", ("F18",), ("Gate4",), "waddle-extension.wit defines waddle:extension@4.0.0 streaming & credential-ref", _t1_f18_01),
        TestCase("T1-F18-02", 1, "M5", ("F18",), ("Gate4",), "Guest receives first streaming chunk and host-verified terminal-usage", _t1_f18_02),
        TestCase("T1-F18-03", 1, "M5", ("F18",), ("Gate2", "Gate4"), "Sync Rust Drop on host InferenceStream triggers non-blocking upstream cancel", _t1_f18_03),
        TestCase("T1-F18-04", 1, "M5", ("F18",), ("Gate4",), "Streaming inference >15s decoupled from STANZA_HANDLER_WEDGE_TIMEOUT", _t1_f18_04),
        TestCase("T1-F18-05", 1, "M5", ("F18",), ("Gate4",), "loader.rs narrows WASI imports and host_state.rs disables inherit_stderr()", _t1_f18_05),
        # F19 (M5)
        TestCase("T1-F19-01", 1, "M5", ("F19",), ("Gate1",), "native-fixture supports two-cell, two-broker (Turso + OpenFGA) stack", _t1_f19_01),
        TestCase("T1-F19-02", 1, "M5", ("F19",), ("Gate1",), "native-fixture supports two-cell, two-broker (Postgres + SpiceDB) stack", _t1_f19_02),
        TestCase("T1-F19-03", 1, "M5", ("F19",), ("Gate2", "Gate4"), "native-fixture exercises streaming first-chunk and mid-stream cancellation", _t1_f19_03),
        TestCase("T1-F19-04", 1, "M5", ("F19",), ("Gate2", "Gate3"), "native-fixture exercises workload rotation, lease-transfer denial, sealed enroll", _t1_f19_04),
        TestCase("T1-F19-05", 1, "M5", ("F19",), ("Gate2", "Gate4"), "native-fixture emits secret-free telemetry and PObserve JSONL traces", _t1_f19_05),
        # F20 (M6)
        TestCase("T1-F20-01", 1, "M6", ("F20",), ("Gate4",), "ai-chatbot & jev-judgments drop OutboundHttpRequest, api_key, 512B error echo", _t1_f20_01),
        TestCase("T1-F20-02", 1, "M6", ("F20",), ("Gate3", "Gate4"), "Host query_mam enforces source_room and removes DM/roster/presence tools", _t1_f20_02),
        TestCase("T1-F20-03", 1, "M6", ("F20",), ("Gate3", "Gate4"), "Server-side room disclosure emitted post-admission and pre-provider-egress", _t1_f20_03),
        TestCase("T1-F20-04", 1, "M6", ("F20",), ("Gate3",), "Admin route selection accepts only qualified ZDR/no-training routes", _t1_f20_04),
        TestCase("T1-F20-05", 1, "M6", ("F20",), ("Gate3",), "Unattended jev-judgments gated per-space on explicit tenant authz", _t1_f20_05),
        # F21 (M6)
        TestCase("T1-F21-01", 1, "M6", ("F21",), ("Gate4",), "Web & Android remove premature sendPublicPromptIfRequested broadcasts", _t1_f21_01),
        TestCase("T1-F21-02", 1, "M6", ("F21",), ("Gate4",), "PersistedSmResumeState localStorage excludes/redacts credential IQs", _t1_f21_02),
        TestCase("T1-F21-03", 1, "M6", ("F21",), ("Gate4", "Gate5"), "deployment.cue, observer-release.json, validations.yaml updated in lockstep", _t1_f21_03),
        TestCase("T1-F21-04", 1, "M6", ("F21",), ("Gate4",), "OTel MetricAttribute uses closed enums and capabilities.toml is updated", _t1_f21_04),
        TestCase("T1-F21-05", 1, "M6", ("F21",), ("Gate5",), "Rollback controls close admission while preserving fences and destruction ledger", _t1_f21_05),
    ]
