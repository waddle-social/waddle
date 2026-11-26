# Waddle Tenant Isolation & Inference Credentials — E2E Test Infrastructure (`TEST_INFRA.md`)

> **Orchestration Artifact Notice:** `TEST_INFRA.md`, `PROJECT.md`, `TEST_READY.md`, and `.agents/teamwork/` are orchestration metadata and MUST NOT be committed to git.

## 1. Overview & Testing Methodology

This document describes the opaque-box, requirement-driven End-to-End (E2E) test suite located under `server/tests/e2e_tenant_inference/`. It verifies all **22 features** (`F1`–`F22` from `PROJECT.md § Feature Inventory`) and all **5 Release Evidence Gates** from `/home/antigravity/waddle-tenant-inference-plan(1).md` across **4 structured tiers**:

| Tier | Methodology | Purpose | Module(s) | Count |
| --- | --- | --- | --- | --- |
| **Tier 1** | Category-Partition (progressive testability) | Feature coverage (`>= 5` independent cases per feature area across `F1`–`F22`) | `tier1_m1_formal_and_core.py`, `tier1_m2_to_m6_features.py` | 110 |
| **Tier 2** | Boundary Value Analysis (BVA) | Exact min/max/overflow/empty/malformed/timing/SSRF/CIDR/protocol edges across M1–M6 | `tier2_boundaries.py` | 35 |
| **Tier 3** | Pairwise Combinatorial Testing | High-risk cross-feature interactions (schema parity, concurrent revocation + authz, lease transfer + sync `Drop`, noisy-neighbor + heartbeats, hostile provider + stderr suppression, etc.) | `tier3_pairwise.py` | 15 |
| **Tier 4** | Real-World Application Scenarios | Full multi-tenant SaaS scenarios covering all 5 Release Evidence Gates + M1 formal non-vacuity fault-injection probes | `tier4_scenarios.py` | 13 |
| **Total** | — | — | `server/tests/e2e_tenant_inference/` | **173** |

### Zero-Fake-Pass & Progressive Testability Policy
- **Zero-dependency Python 3.12 stdlib harness** (`harness.py`): no third-party Python packages, no mocks of target contracts, and no silent skips.
- **M1 & E2E meta tests PASS today** by genuinely executing `quint`, `verus`, `p compile --mode pobserve -o <tempdir>`, `cargo test -p waddle-broker-core`, `check-rust-structure.py`, and isolated `TemporaryDirectory` negative-mutation probes.
- **M2–M6 tests FAIL cleanly today** by probing real expected files, scripts (`check-tenant-isolation.py`), WIT (`waddle:extension@4.0.0`), CUE (`deployment.cue`), Helm (`validations.yaml`), client files (`extension-launcher.ts`, `ExtensionCommandController.kt`, `resume-persistence.ts`), crates (`waddle-broker`), and targeted `waddle-server` integration test binaries. As each milestone lands, those exact tests transition from `FAIL` to `PASS`.

---

## 2. Complete Traceability Matrix

### 2.1 Features (`F1`–`F22`) → Test IDs

| Feature | Milestone | Tier 1 (Feature Coverage) | Tier 2 (Boundaries) | Tier 3 (Pairwise) | Tier 4 (Scenarios) |
| --- | --- | --- | --- | --- | --- |
| **F1** Formal Verification — Quint.SH | M1 | `T1-F01-01`..`T1-F01-05` | `T2-M1-09`, `T2-M1-10` | `T3-M1-03` | `T4-M1-01` |
| **F2** Formal Verification — Verus | M1 | `T1-F02-01`..`T1-F02-05` | `T2-M1-02`..`T2-M1-06`, `T2-M1-08` | `T3-M1-02` | `T4-M1-01` |
| **F3** Formal Verification — PObserve | M1 | `T1-F03-01`..`T1-F03-05` | `T2-M1-07` | `T3-M1-01` | `T4-M1-01`, `T4-S05` |
| **F4** ADR-0021 & Trait Contracts | M1 | `T1-F04-01`..`T1-F04-05` | `T2-M1-01` | `T3-M1-01`, `T3-M1-02` | — |
| **F5** Turso WAL Baseline Verification | M1 | `T1-F05-01`..`T1-F05-05` | `T2-M1-06` | — | `T4-S10` |
| **F6** Canonical `TenantId`, `AccountId` & Registry | M2 | `T1-F06-01`..`T1-F06-05` | `T2-M2-01` | `T3-03` | `T4-S02` |
| **F7** `TenantStore` (Turso + Postgres) | M2 | `T1-F07-01`..`T1-F07-05` | `T2-M2-02` | `T3-03`, `T3-08`, `T3-12` | `T4-S01`, `T4-S02`, `T4-S03` |
| **F8** In-Memory Registry & Actor Scoping | M2 | `T1-F08-01`..`T1-F08-05` | — | `T3-03` | `T4-S02` |
| **F9** Per-Tenant Semaphores & Arch Guard | M2 | `T1-F09-01`..`T1-F09-05` | `T2-M2-05`, `T2-M2-06` | `T3-08` | `T4-S03` |
| **F10** `PlacementLeaseStore` & Write Fencing | M2 | `T1-F10-01`..`T1-F10-05` | `T2-M2-03`, `T2-M2-04` | `T3-02`, `T3-08`, `T3-11` | `T4-S03`..`T4-S06`, `T4-S11`, `T4-S12` |
| **F11** Broker Envelope Encryption & KMS Custody | M3 | `T1-F11-01`..`T1-F11-05` | `T2-M3-01` | `T3-03`, `T3-04`, `T3-07`, `T3-12` | `T4-S02`, `T4-S06`, `T4-S09`..`T4-S11` |
| **F12** Independent IdP Intent & Device Keys | M3 | `T1-F12-01`..`T1-F12-05` | `T2-M3-05` | `T3-07` | `T4-S06`, `T4-S07` |
| **F13** Versioned `urn:waddle:*` IQs & Bounce Stripping | M3 | `T1-F13-01`..`T1-F13-05` | `T2-M3-02`, `T2-M3-03`, `T2-M3-04` | `T3-04`, `T3-07` | `T4-S05`, `T4-S07`, `T4-S09` |
| **F14** `PermissionBackend` (OpenFGA + SpiceDB + Local) | M4 | `T1-F14-01`..`T1-F14-05` | `T2-M4-01`, `T2-M4-02` | `T3-01`, `T3-03`, `T3-06`, `T3-12` | `T4-S01`, `T4-S02`, `T4-S06`, `T4-S11` |
| **F15** 3-Fence `AdmissionLedgerStore` & `ReplicaFence` | M4 | `T1-F15-01`..`T1-F15-05` | `T2-M4-03`, `T2-M4-04` | `T3-01`, `T3-02`, `T3-05`, `T3-06`, `T3-10` | `T4-S04`..`T4-S06`, `T4-S08`, `T4-S11` |
| **F16** `unknown_after_send` Quarantine | M4 | `T1-F16-01`..`T1-F16-05` | `T2-M4-05` | `T3-05`, `T3-10` | `T4-S05` |
| **F17** Constrained Provider Egress | M5 | `T1-F17-01`..`T1-F17-05` | `T2-M5-01` | `T3-09` | `T4-S06`, `T4-S08`, `T4-S09` |
| **F18** Streaming WIT (`@4.0.0`) & Wasmtime Hardening | M5 | `T1-F18-01`..`T1-F18-05` | `T2-M5-02`..`T2-M5-05` | `T3-02`, `T3-09`, `T3-11` | `T4-S07`..`T4-S09`, `T4-S12` |
| **F19** Two-Cell, Two-Broker Native Fixture Spike | M5 | `T1-F19-01`..`T1-F19-05` | — | — | `T4-S01`, `T4-S05` |
| **F20** `ai-chatbot` & `jev-judgments` Migration & Host Privacy | M6 | `T1-F20-01`..`T1-F20-05` | `T2-M6-01`..`T2-M6-03` | `T3-06`, `T3-09`, `T3-10` | `T4-S07`, `T4-S08`, `T4-S09` |
| **F21** Client Alignment & Deployment/Telemetry Updates | M6 | `T1-F21-01`..`T1-F21-05` | `T2-M6-04` | `T3-11` | `T4-S07`, `T4-S09`, `T4-S12` |
| **F22** E2E Testing Track & Tier 5 Hardening | E2E / M7 | `T1-F22-01`..`T1-F22-05` | — | — | All Gates 1–5 |

### 2.2 Release Evidence Gates (Plan § Release Evidence) → Test IDs

| Release Evidence Gate | Description | Primary Tier 4 Scenarios | Supporting Tiers 1–3 |
| --- | --- | --- | --- |
| **Gate 1** | Backend trait parity (`Turso+OpenFGA` vs `Postgres+SpiceDB`), identity collision isolation, noisy-neighbor saturation | `T4-S01`, `T4-S02`, `T4-S03`, `T4-S12` | `T1-F04..F10`, `T1-F14`, `T1-F19`, `T2-M2-*`, `T3-03`, `T3-08` |
| **Gate 2** | Deterministic dual-broker fencing race, 8-barrier race matrix, `unknown_after_send` quarantine, PObserve monitors | `T4-M1-01`, `T4-S04`, `T4-S05` | `T1-F01..F03`, `T1-F10`, `T1-F15`, `T1-F16`, `T3-01`, `T3-02`, `T3-05` |
| **Gate 3** | Full 8-case authority & boundary denial matrix + positive control | `T4-M1-01`, `T4-S06`, `T4-S07`, `T4-S08` | `T1-F11`, `T1-F12`, `T1-F14`, `T1-F15`, `T1-F17`, `T1-F20`, `T2-M4-02`, `T2-M6-*` |
| **Gate 4** | End-to-end `ai-chatbot` & `jev-judgments` migration against synthetic streaming provider + zero-leak sentinel scan | `T4-S07`, `T4-S08`, `T4-S09` | `T1-F13`, `T1-F17`, `T1-F18`, `T1-F20`, `T1-F21`, `T2-M5-*`, `T3-07`, `T3-09` |
| **Gate 5** | Backup & WAL restore drill (destruction ledger), KMS/authz/lease outage fail-closed drill, `WADDLE_DRAIN_TIMEOUT_SECS` drain & rollback | `T4-S10`, `T4-S11`, `T4-S12` | `T1-F01-04`, `T1-F05`, `T1-F10-05`, `T1-F11-04`, `T1-F11-05`, `T1-F21-05`, `T3-04`, `T3-11`, `T3-12` |

---

## 3. Execution Instructions & VM Resource Guardrails

### 3.1 CLI Commands
Run from the repository root (`/home/antigravity/waddle-social/waddle`):

```bash
# List all registered tests with tier, milestone, feature, and gate metadata
python3 server/tests/e2e_tenant_inference/run_e2e.py --list

# Run M1 formal verification, trait contracts, Turso WAL spike, and non-vacuity probes (MUST exit 0 today)
python3 server/tests/e2e_tenant_inference/run_e2e.py --milestone m1 --verbose

# Run E2E meta self-checks (MUST exit 0 today)
python3 server/tests/e2e_tenant_inference/run_e2e.py --milestone e2e --verbose

# Run a specific tier (1, 2, 3, 4, or all)
python3 server/tests/e2e_tenant_inference/run_e2e.py --tier 1
python3 server/tests/e2e_tenant_inference/run_e2e.py --tier all

# Filter by milestone (m1..m6, e2e, all) and write JSON report
python3 server/tests/e2e_tenant_inference/run_e2e.py --milestone m2 --json /tmp/m2_e2e.json
```

Exit code contract:
- **Exit `0`**: ALL selected tests passed (and `> 0` tests executed).
- **Exit `1`**: ANY selected test failed (or `0` tests matched).

### 3.2 VM Environment & Resource Notes
- **Hardware:** 2 CPU cores / 7.8 GiB RAM Linux VM.
- **Rust Toolchain:** Cargo invocations automatically run with `cwd=server/`, `RUSTUP_TOOLCHAIN=1.98.1`, and `CARGO_BUILD_JOBS=2` (required for `turso_core 0.8.4` compatibility).
- **Memory Cliff Guard:** `harness.py` explicitly forbids `cargo test -p waddle-server --lib` (which hits the ~6 GB RAM cliff on this VM). All server tests target `-p waddle-broker-core`, `-p waddle-broker`, `-p waddle-extensions`, or individual `waddle-server --test <binary>` targets.
- **Formal Tools (`$HOME/.local/bin`):**
  - `quint` (`$HOME/.local/bin/quint`)
  - `verus` (`$HOME/.local/bin/verus`, wrapper selects `RUSTUP_TOOLCHAIN=1.100.0` internally)
  - `p` (`$HOME/.local/bin/p`, always invoked with `-o <tempdir>` so `server/specs/pobserve/PGenerated/` is never created in the repo)
- **Memoization:** Shared heavy commands (`cargo test -p waddle-broker-core`, `quint`, `verus`, `p compile`) are memoized per runner invocation so each tool runs genuinely once per CLI invocation without redundant rebuilds.

---

## 4. Expected Pass / Fail Progression Across M1–M7

| Milestone Completed | Expected Passing Milestones | Expected Remaining Failures | Full Suite (`--tier all`) Exit Code |
| --- | --- | --- | --- |
| **M1 (Current Baseline)** | `M1` (39/39), `E2E` (5/5) = **44 PASS** | `M2`–`M6` (129 FAIL) | `1` (proves zero fake passes) |
| **M2** | `M1`, `M2`, `E2E` | `M3`–`M6` | `1` |
| **M3** | `M1`–`M3`, `E2E` | `M4`–`M6` | `1` |
| **M4** | `M1`–`M4`, `E2E` | `M5`–`M6` | `1` |
| **M5** | `M1`–`M5`, `E2E` | `M6` | `1` |
| **M6** | `M1`–`M6`, `E2E` = **173/173 PASS** | 0 FAIL | `0` |
| **M7 (Final + Tier 5)** | 173/173 PASS + Tier 5 adversarial hardening | 0 FAIL | `0` |
