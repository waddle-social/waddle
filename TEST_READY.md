# TEST_READY — Waddle Tenant Isolation & Inference Credentials E2E Suite

> **Orchestration Artifact Notice:** `TEST_READY.md` and `TEST_INFRA.md` are orchestration metadata and MUST NOT be committed to git. The standalone opaque-box test suite resides under `server/tests/e2e_tenant_inference/` (zero third-party Python dependencies; outside `server/Cargo.toml` members so parallel milestone work is unaffected; M7 integrates/commits repo-resident test artifacts cleanly).

**Status:** **READY FOR MILESTONE GATING & M7 PHASE 1 VERIFICATION**
**Verified:** Iteration 2 Unanimous 5/5 Gate (`APPROVE` / `APPROVE` / `CONFIRMED` / `CONFIRMED` / `CLEAN`)

---

## 1. Exact Runner Commands

All commands run from repository root `/home/antigravity/waddle-social/waddle` (and are CWD-independent):

```bash
# List all 173 registered tests across Tiers 1–4
python3 server/tests/e2e_tenant_inference/run_e2e.py --list

# Full E2E suite (M7 Phase 1 target: MUST exit 0 with 173/173 PASS)
python3 server/tests/e2e_tenant_inference/run_e2e.py --tier all

# Run by Tier (1, 2, 3, or 4)
python3 server/tests/e2e_tenant_inference/run_e2e.py --tier 1
python3 server/tests/e2e_tenant_inference/run_e2e.py --tier 2
python3 server/tests/e2e_tenant_inference/run_e2e.py --tier 3
python3 server/tests/e2e_tenant_inference/run_e2e.py --tier 4

# Run by Milestone (m1, m2, m3, m4, m5, m6, e2e, all)
python3 server/tests/e2e_tenant_inference/run_e2e.py --milestone m1 --verbose
python3 server/tests/e2e_tenant_inference/run_e2e.py --milestone e2e --verbose

# Machine-readable JSON report & fail-fast mode
python3 server/tests/e2e_tenant_inference/run_e2e.py --tier all --json /tmp/waddle-e2e-report.json
python3 server/tests/e2e_tenant_inference/run_e2e.py --milestone m2 --fail-fast
```

**Operational Notes:**
- Uses zero-dependency Python 3.12 stdlib.
- Automatically configures `RUSTUP_TOOLCHAIN=1.98.1`, `PATH` with `$HOME/.local/bin` (`quint`, `verus`, `p`), and `Cwd: server/` for Cargo invocations.
- Explicitly guards against `cargo test -p waddle-server --lib` (and `cargo test -p waddle-server` without `--test`) to respect the 2-core / 7.8 GiB RAM VM ceiling (`server/AGENTS.md:40`).
- Compiles PObserve into isolated `TemporaryDirectory` paths so `server/specs/pobserve/PGenerated/` is never created in the repo.
- Enforces `assert_cargo_tests_passed` (`ZERO_TESTS_EXECUTED` guard) on all Cargo-backed tests, comment stripping + positive structural anchors on all static/contract checks, and tool-specific failure banner verification on M1 non-vacuity mutation probes.

---

## 2. Tier Coverage Summary (173 Genuine Tests)

| Tier | Module(s) | Methodology | Test Count | Baseline Status (at Publish) |
| --- | --- | --- | --- | --- |
| **Tier 1** | `tier1_m1_formal_and_core.py`, `tier1_m2_to_m6_features.py` | Category-Partition Feature Coverage (`5` tests per feature across all `F1`–`F22`) | **110** | M1 `25/25` PASS, E2E `5/5` PASS, M2 `1/25` PASS (`T1-F08-04` live M2 commit), M3–M6 `0/55` cleanly FAIL |
| **Tier 2** | `tier2_boundaries.py` | Boundary Value Analysis (`TenantId`, `RLIMIT_NOFILE`, `u64::MAX`, SSRF CIDR/IPv4-mapped IPv6, `BootInstant`/clock skew, stream chunks, XEP-0198 nonces, IQ XML/4 bounce paths, SpiceDB `765B`, OpenFGA `contextual_tuples`, ZDR routes, `"private"` room `/ai`, Helm/CUE) | **35** | M1 `10/10` PASS, M2–M6 `0/25` cleanly FAIL |
| **Tier 3** | `tier3_pairwise.py` | Cross-Feature Pairwise Combinations (revocation + authz, lease transfer + stream `Drop` cancel, same-localpart + remote-`BareJid` + MAM + cred isolation, XEP-0198 replay + destruction restore, `unknown_after_send` + budget, room kick + `ReplicaFence`, noisy-neighbor + heartbeat, hostile provider + stderr, tenant delete cascade, M1 spec/kernel/event parity) | **15** | M1 `3/3` PASS, M2–M6 `0/12` cleanly FAIL |
| **Tier 4** | `tier4_scenarios.py` | Real-World Multi-Tenant SaaS Scenarios covering ALL 5 Release Evidence Gates (`T4-S01`..`T4-S12`) + M1 Formal Non-Vacuity Mutation Probes (`T4-M1-01`) | **13** | M1 `1/1` PASS, M2–M6 `0/12` cleanly FAIL |
| **Total** | `server/tests/e2e_tenant_inference/` | Full 4-Tier Opaque-Box Suite | **173** | **45 PASS / 128 FAIL** (exit `1`; zero fake passes) |

---

## 3. Feature & Release Evidence Checklist (22/22 Features + 5/5 Gates)

| # | Feature (`PROJECT.md § Feature Inventory`) | Milestone | Tier 1 | Tier 2–4 Coverage |
| --- | --- | --- | --- | --- |
| F1 | Formal Verification — Quint.SH | M1 | `T1-F01-01..05` (PASS) | `T2-M1-08`, `T3-M1-03`, `T4-M1-01` (PASS) |
| F2 | Formal Verification — Verus | M1 | `T1-F02-01..05` (PASS) | `T2-M1-02..07`, `T3-M1-02`, `T4-M1-01` (PASS) |
| F3 | Formal Verification — PObserve | M1 | `T1-F03-01..05` (PASS) | `T2-M1-09..10`, `T3-M1-01`, `T4-M1-01` (PASS) |
| F4 | ADR & Trait Contracts | M1 | `T1-F04-01..05` (PASS) | `T2-M1-01` (PASS) |
| F5 | Turso WAL Baseline Verification | M1 | `T1-F05-01..05` (PASS) | `T2-M1-06` (PASS) |
| F6 | Canonical `TenantId`, `AccountId` & Registry | M2 | `T1-F06-01..05` | `T2-M2-01`, `T3-03`, `T4-S02` |
| F7 | `TenantStore` (Turso + Postgres) | M2 | `T1-F07-01..05` | `T2-M2-02`, `T3-03`, `T4-S01` (Gate 1) |
| F8 | In-Memory Registry & Actor Scoping | M2 | `T1-F08-01..05` | `T3-03`, `T4-S02` (Gate 1) |
| F9 | Per-Tenant Resource Semaphores & Arch Guard | M2 | `T1-F09-01..05` | `T2-M2-05`, `T3-07`, `T4-S03` (Gate 1) |
| F10 | `PlacementLeaseStore` & Write Fencing | M2 | `T1-F10-01..05` | `T2-M2-03..04`, `T3-02`, `T3-11`, `T4-S04`, `T4-S12` (Gates 2 & 5) |
| F11 | Broker Envelope Encryption & KMS Custody | M3 | `T1-F11-01..05` | `T2-M3-01`, `T2-M3-05`, `T3-04`, `T3-12`, `T4-S10..S11` (Gate 5) |
| F12 | Independent IdP Intent & Device Keys | M3 | `T1-F12-01..05` | `T2-M3-05`, `T4-S06` (Gate 3) |
| F13 | Versioned `urn:waddle:*` Credential IQs & Bounce Stripping | M3 | `T1-F13-01..05` | `T2-M3-02..04`, `T3-04`, `T4-S09` (Gate 4) |
| F14 | `PermissionBackend` (OpenFGA + SpiceDB + Local) | M4 | `T1-F14-01..05` | `T2-M4-01..02`, `T3-01`, `T3-12`, `T4-S01`, `T4-S06`, `T4-S11` (Gates 1, 3, 5) |
| F15 | 3-Fence `AdmissionLedgerStore` & `ReplicaFence` | M4 | `T1-F15-01..05` | `T2-M4-03..04`, `T3-01..02`, `T3-05..06`, `T4-S05` (Gate 2) |
| F16 | `unknown_after_send` Quarantine | M4 | `T1-F16-01..05` | `T2-M4-05`, `T3-05`, `T3-09`, `T4-S05` (Gate 2) |
| F17 | Constrained Provider Egress | M5 | `T1-F17-01..05` | `T2-M5-01`, `T2-M5-03..04`, `T3-08`, `T4-S06`, `T4-S09` (Gates 3 & 4) |
| F18 | Streaming WIT Contract (`waddle:extension@4.0.0`) & Wasmtime Hardening | M5 | `T1-F18-01..05` | `T2-M5-02`, `T2-M5-05`, `T3-02`, `T3-08`, `T4-S07` (Gate 4) |
| F19 | Two-Cell, Two-Broker Native Fixture Spike | M5 | `T1-F19-01..05` | `T4-S01`, `T4-S04`, `T4-S05` (Gates 1 & 2) |
| F20 | `ai-chatbot` & `jev-judgments` Migration & Host Room Privacy | M6 | `T1-F20-01..05` | `T2-M6-01..03`, `T3-06`, `T3-09`, `T4-S07..S09` (Gate 4) |
| F21 | Client Alignment & Deployment/Telemetry Updates | M6 | `T1-F21-01..05` | `T2-M6-04..05`, `T3-10..11`, `T4-S07`, `T4-S09`, `T4-S12` (Gates 4 & 5) |
| F22 | E2E Testing Track (Tiers 1–4) & Tier 5 Adversarial Hardening | E2E / M7 | `T1-F22-01..05` (PASS) | Full Suite (`TEST_INFRA.md`, `TEST_READY.md`) |

See `/home/antigravity/waddle-social/waddle/TEST_INFRA.md` for full traceability, methodology, and M1–M7 milestone progression criteria.
