# Project: Waddle Tenant Isolation & Inference Credentials

> **Orchestration Artifact Notice:** `PROJECT.md`, `TEST_INFRA.md`, `TEST_READY.md`, and `.agents/teamwork/` are orchestration metadata and MUST NOT be committed to git. All repository commits go on branch `feat/tenant-inference-isolation` using small, logical Conventional Commits (`AGENTS.md`), and no PR is opened until all milestones and audits pass.

## Architecture

### System Overview
Waddle deploys one independently operated, replicated **Credential & Inference Broker** per trust domain (distinct namespace, `SecretStore`, workload identity, DB permissions, network policy, KMS permissions), shared across a pool of **Cells** (supporting both shared-pool and dedicated-cell tenant placement with identical binaries and placement contracts).

Both storage and authorization are modeled behind backend-agnostic traits in `waddle-broker-core`, supporting two full stacks without changing cell, broker, placement, or guest contracts:
- **Primary Multi-Tenant Stack**: Self-hosted **Turso** (`turso` Rust engine in standard WAL mode, one `<tenant>.db` per tenant behind a bounded LRU FD pool + thin mTLS storage-host service enforcing `(tenant_id, holder_id, placement_generation)`) + **OpenFGA** (one `store_id` per tenant, pinned `authorization_model_id`, Modular Models `schema 1.2` with `module:<store_id>|chat` vs `module:<store_id>|inference`, `HIGHER_CONSISTENCY` reads).
- **Secondary Stack**: **PostgreSQL** (`tenant_id`-scoped schemas/rows, `FOR SHARE`/epoch-guarded `EnteredTransaction`, restricted runtime roles) + **SpiceDB** (tenant-scoped object IDs within `MAX_AUTHZ_ID_BYTES = 765`, `FullyConsistent` reads, `AUTHZ_SCHEMA_VERSION = 3`).

### Crate & Verification Strategy
- Pure-Rust domain types, traits, and security-critical state machines live in `server/crates/waddle-broker-core` (small, fast to compile on 2 cores / 7.8 GiB RAM, directly verifiable by **Verus**).
- Broker custody, envelope encryption, KMS abstraction, admission ledger, `ReplicaFence`, and constrained provider egress live in `server/crates/waddle-broker`.
- Protocol-level distributed invariants are formally modeled and checked with **Quint.SH** (`server/specs/quint/`).
- Runtime execution traces from the native fixture and deterministic barrier tests are verified with **PObserve** (`server/specs/pobserve/`) and a paired JSONL trace verifier.
- Server integration tests live in `server/crates/waddle-server/tests/*.rs` (avoiding the ~6 GB `waddle-server --lib --tests` memory cliff per `server/AGENTS.md:40`).

## Feature Inventory

Every feature from `/home/antigravity/waddle-tenant-inference-plan(1).md` and `ORIGINAL_REQUEST.md` is assigned a milestone (none unassigned):

| # | Feature | Description | Milestone | Source |
|---|---------|-------------|-----------|--------|
| 1 | Formal Verification — Quint.SH | 4 Quint specs (`three_fence_admission.qnt`, `placement_lease.qnt`, `credential_lifecycle_restore.qnt`, `stream_cancel_and_quarantine.qnt`) passing `typecheck`, `test`, `run` | M1 | ORIGINAL_REQUEST, Plan §1-6 |
| 2 | Formal Verification — Verus | Pure-Rust Verus proofs (AAD injectivity, 3-fence snapshot/gen guard, budget arithmetic non-overflow, SSRF IP blocklist completeness, Turso LRU FD pool bound) | M1 | ORIGINAL_REQUEST, Plan §2-6 |
| 3 | Formal Verification — PObserve | 3 PObserve runtime trace monitors (`AdmissionFenceMonitor.p`, `PlacementLeaseMonitor.p`, `StreamCancellationMonitor.p`) + secret-free JSONL trace schema | M1 | ORIGINAL_REQUEST, Plan §3,5,6 |
| 4 | ADR & Trait Contracts | ADR superseding RFC-0007/0014 host-AI prohibition; trust-boundary ownership & measurable bounds; `TenantStore`, `PlacementLeaseStore`, `AdmissionLedgerStore`, `PermissionBackend` traits | M1 | Plan §Release, §1 |
| 5 | Turso WAL Baseline Verification | Resolve Turso multi-pod topology (mTLS storage-host enforcing `(tenant_id, holder_id, placement_generation)`); verify `sqlite.sql` (8 triggers, FTS5, `unixepoch('subsec') * 1000`) on `turso` crate in WAL mode | M1 | Plan §1 |
| 6 | Canonical `TenantId`, `AccountId` & Registry | Canonical `TenantId` from verified local domain routing; tenant registry as sole handle constructor; immutable `account_id` UUID across `users` and `native_users` | M2 | Plan §2 |
| 7 | `TenantStore` (Turso + Postgres) | Turso per-tenant `<tenant>.db` behind bounded LRU FD pool (`RLIMIT_NOFILE`); Postgres `tenant_id`-scoped keys, unique constraints (`users`, `auth_identities`, `spaces_metadata`), `baseline.rs` catalog parity | M2 | Plan §2 |
| 8 | In-Memory Registry & Actor Scoping | Scope `RoomRegistryActor`, `ConnectionRegistry`, `CommandRegistry`, `ExtensionManager`, federated `BareJid` caches; fix `WasmExtensionActor` (`waddle_id = "local"`) | M2 | Plan §2 |
| 9 | Per-Tenant Resource Semaphores & Arch Guard | Bound mailboxes, tasks, streams, Wasm linear memory (`memories * memory_bytes`), fuel, epoch interruption, DB concurrency, reserved heartbeat/cancel capacity; Python lexical arch guard (`check-tenant-isolation.py`) | M2 | Plan §2 |
| 10 | `PlacementLeaseStore` & Write Fencing | Durable CAS claim/renew/transfer/release on Turso & Postgres; SPIFFE mTLS + RFC 9266 `tls-exporter` incarnation key proof; promote `BootInstant`; fence data writes, `room_observation`, outbox workers, `CommandRegistry`; shared & dedicated placement; `WADDLE_DRAIN_TIMEOUT_SECS` quiescing | M2 | Plan §3 |
| 11 | Broker Envelope Encryption & KMS Custody | Authenticated envelope encryption (AES-256-GCM / XChaCha20-Poly1305) with length-prefixed AAD; user DEK wrapped under tenant DEK under broker-only KMS; destruction ledger enforced on restore; lifecycle (rotate, replace, withdraw, `retired_account_jids`, tenant delete + Turso WAL purge + OpenFGA `DeleteStore`) | M3 | Plan §4 |
| 12 | Independent IdP Intent & Device Keys | `Colony` JWT / WebAuthn proof verified by broker against IdP JWKS; registered client device signing keys; reject `native_users` SCRAM & cell-asserted admin flags; client-pinned root broker-key-signing public key + rotatable subkeys over XMPP | M3 | Plan §Security, §4 |
| 13 | Versioned `urn:waddle:*` Credential IQs & Bounce Stripping | Client-sealed credential enrollment/status/rotation/revocation/policy `<iq/>` stanzas; idempotent nonces across XEP-0198 reconnect replay; opaque IQ results/errors for `sm_unacked`; strip credential payloads on all IQ error-bounce paths (`route_to_connection`, `destination`, `full_jid_forward`, `muc_admin`); amend protocol doc & XEP audit | M3 | Plan §4 |
| 14 | `PermissionBackend` (OpenFGA + SpiceDB + Local) | Replace `PermissionActorBackend` enum with `PermissionBackend` trait; OpenFGA per-tenant `store_id`, pinned `authorization_model_id`, Modular Models (`chat` vs `inference`), `HIGHER_CONSISTENCY`, reject `contextual_tuples`; SpiceDB tenant-scoped IDs (`<=765B`), `FullyConsistent`, `AUTHZ_SCHEMA_VERSION=3` | M4 | Plan §5 |
| 15 | 3-Fence `AdmissionLedgerStore` & `ReplicaFence` | Validate device-signed invocation & bounded multi-turn run/step ledger; 3 durable fence keys `(tenant, credential)`, `(tenant, room_or_space)`, `(tenant, placement)` on Turso & Postgres; short-tick broker `ReplicaFence` aborting peer in-flight streams; never silently switch payer; reject member creds for observers/schedules/webhooks/federated remotes | M4 | Plan §5 |
| 16 | `unknown_after_send` Quarantine | Update `ingress_uow/room_observation/work.rs` so `unknown_after_send` quarantines work instead of bypassing `MAX_ATTEMPTS` | M4 | Plan §5 |
| 17 | Constrained Provider Egress | Broker outbound adapters with approved provider/deployment/model/operation profiles, DNS-resolved SSRF IP blocklist, TLS identity, `.no_proxy()`, no redirects, byte/time/concurrency/spend caps, response validation, discard upstream headers/error bodies, no provider admin endpoints | M5 | Plan §6 |
| 18 | Streaming WIT Contract (`waddle:extension@4.0.0`) & Wasmtime Hardening | Typed inference WIT ops (`tenant(name)` / `invoking-user(provider)`), bounded stream resource with host-verified terminal usage, sync `Drop` -> non-blocking upstream cancel, decouple streaming from 15s `STANZA_HANDLER_WEDGE_TIMEOUT`, narrow `wasmtime_wasi` linker imports, disable `inherit_stderr()` & guest log forwarding for inference, remove `outbound.http.request` from inference guests | M5 | Plan §6 |
| 19 | Two-Cell, Two-Broker Native Fixture Spike | Extend `server/experiments/native-fixture` (no containers) exercising `(Turso+OpenFGA)` & `(Postgres+SpiceDB)` with streaming first-chunk + cancel, workload rotation, lease-transfer denial, sealed enrollment, secret-free telemetry, and PObserve trace checks | M5 | Plan §1, §6 |
| 20 | `ai-chatbot` & `jev-judgments` Migration & Host Room Privacy | Migrate both guests off `Authorization`/raw `api_key`/`configSecretFiles`; qualified ZDR/no-training routes only; remove 512B error-body echo; host-enforce `source_room` in `ExtensionHostAdapter::query_mam` + post-read `archive_still_authorized`; remove cross-room MAM/DM/roster/presence tools; disallow `"private"` output for member-funded room `/ai`; server-side post-admission room disclosure; gate `jev-judgments` per-space | M6 | Plan §7 |
| 21 | Client Alignment & Deployment/Telemetry Updates | Remove premature client prompt broadcasts in Web (`extension-launcher.ts`) & Android (`ExtensionCommandController.kt`); protect `PersistedSmResumeState` `localStorage`; align Apple/Linux `/ai`; update `deployment.cue`, `observer-release.json`, `validations.yaml`; closed `MetricAttribute` enums (no raw `tenant_id`/JID); rollback controls | M6 | Plan §7, §8 |
| 22 | E2E Testing Track (Tiers 1–4) & Tier 5 Adversarial Hardening | Opaque-box E2E suite (`TEST_INFRA.md`, Tiers 1–4, `TEST_READY.md`) covering all 5 Release Evidence gates (backend parity, deterministic fencing, authority denials, guest migration/zero-leak scans, restore/outage drills) + Final Milestone Phase 1 (100% pass) & Phase 2 (Tier 5 white-box adversarial hardening) | M7 + E2E Track | Plan §Release Evidence |

## Milestones

| # | Name | Scope | Dependencies | Status |
|---|------|-------|-------------|--------|
| M1 | Bootstrap, Formal Specs & Trait Contracts | Branch setup, tool bootstrap (Rust 1.98.1, Quint, Verus, P/PObserve), ADR, `waddle-broker-core` traits & Verus-verified kernels, 4 Quint specs, 3 PObserve monitors, Turso WAL baseline verification | none | DONE (`fdce5d3`, 6 commits, 5/5 PASS) |
| M2 | Tenant Scope, `TenantStore` & `PlacementLeaseStore` | `TenantId`/`AccountId`, tenant registry, Turso LRU + Postgres `TenantStore`, scoped registries/caches, per-tenant semaphores, arch guard, `BootInstant`, `PlacementLeaseStore` + write/worker fencing | M1 | DONE (`eb51142`, 14 commits, 5/5 PASS) |
| M3 | Broker Custody, Envelope Crypto & `urn:waddle:*` IQs | `waddle-broker` envelope encryption + KMS + destruction ledger + restore drill, IdP/device-key intent, pinned root key + subkey distribution, `urn:waddle:*` credential IQs, IQ error-bounce stripping, doc amendments | M1 | DONE (`ba0f03b`, 8 commits, 5/5 PASS, M3 E2E 23/23) |
| M4 | `PermissionBackend` & 3-Fence `AdmissionLedgerStore` | `PermissionBackend` trait (OpenFGA + SpiceDB + Local), device-signed invocation & run ledger, 3-fence `AdmissionLedgerStore` (Turso + Postgres), broker `ReplicaFence`, `unknown_after_send` quarantine, deterministic barrier + PObserve tests | M2, M3 | DONE (`bb9026a`, 10 commits, 5/5 PASS, M4 E2E 26/26) |
| M5 | Provider Egress, WIT `@4.0.0` Streaming & Native Fixture | Constrained broker provider egress, `waddle:extension@4.0.0` streaming WIT + sync `Drop` cancel, decouple from 15s wedge backstop, Wasmtime WASI/stderr/logging hardening, two-cell/two-broker native fixture spike | M4 | IN_PROGRESS |
| M6 | Extension Migration, Host Privacy, Clients & Deploy | Migrate `ai-chatbot` & `jev-judgments`, host `query_mam` `source_room` enforcement, server-side post-admission disclosure, Web/Android/Apple/Linux alignment, `deployment.cue`/`observer-release.json`/`validations.yaml`, closed `MetricAttribute` OTel, rollback controls | M5 | PLANNED |
| M7 | Final Milestone (E2E Tiers 1–4 + Tier 5 Hardening) | Phase 1: 100% pass of E2E Tiers 1–4 (`TEST_READY.md`); Phase 2: white-box Tier 5 Challenger-driven adversarial hardening + full verification & clean git commit check | M6, E2E Track | PLANNED |
| E2E | E2E Testing Track (Parallel) | Opaque-box requirement-driven E2E test infra (`TEST_INFRA.md`), Tiers 1–4 test suites across all 22 features, publish `TEST_READY.md` | M1 | DONE (`TEST_READY.md` published, 173 tests, 5/5 PASS) |

## Interface Contracts

### `waddle-broker-core` ↔ `waddle-server` / `waddle-broker`
- **`TenantId`**: Canonical validated newtype derived from verified local domain routing (non-empty lowercase DNS domain, max 253 bytes).
- **`AccountId`**: Immutable UUIDv4/v7 newtype (`uuid::Uuid`) decoupled from mutable XMPP localparts; used in AAD and authz subjects.
- **`HolderInstance`**: `{ spiffe_id: String, incarnation_pubkey: [u8; 32], tls_exporter_proof: Vec<u8> }`.
- **`TenantStore` trait**: Tenant-scoped CRUD/query handle factory; Turso opens `<data_dir>/<tenant>.db` via bounded LRU FD pool; Postgres binds `tenant_id` scope.
- **`PlacementLeaseStore` trait**:
  - `claim(tenant: &TenantId, role: &CellRole, holder: &HolderInstance, ttl_ms: u64) -> Result<PlacementLease, LeaseError>`
  - `renew(tenant: &TenantId, holder: &HolderInstance, generation: u64, ttl_ms: u64) -> Result<PlacementLease, LeaseError>`
  - `transfer(tenant: &TenantId, from_holder: &HolderInstance, to_holder: &HolderInstance, expected_gen: u64, ttl_ms: u64) -> Result<PlacementLease, LeaseError>`
  - `release(tenant: &TenantId, holder: &HolderInstance, expected_gen: u64) -> Result<(), LeaseError>`
  - `current(tenant: &TenantId) -> Result<Option<PlacementLease>, LeaseError>`
- **`PermissionBackend` trait** (replaces closed `PermissionActorBackend`):
  - `check(&self, req: CheckRequest) -> Result<CheckDecision, PermissionError>` (enforces `HIGHER_CONSISTENCY` on OpenFGA / `FullyConsistent` on SpiceDB; rejects `contextual_tuples` on broker path)
  - `write_tuples` / `delete_tuples` / `read_tuples` / `swap_exclusive_relation` / `replace_exclusive_relation`
  - `provision_tenant_store(&self, tenant: &TenantId) -> Result<TenantAuthzStoreRef, PermissionError>`
  - `delete_tenant_store(&self, tenant: &TenantId) -> Result<(), PermissionError>`
- **`AdmissionLedgerStore` trait**:
  - `snapshot_fences(&self, tenant: &TenantId, cred: &CredentialKey, room: &RoomScopeKey) -> Result<ThreeFenceSnapshot, AdmissionError>`
  - `close_and_advance_fence(&self, key: &FenceKey) -> Result<u64, AdmissionError>`
  - `reopen_fence(&self, key: &FenceKey, expected_gen: u64) -> Result<(), AdmissionError>`
  - `admit_attempt(&self, req: AdmitAttemptRequest) -> Result<AdmitReceipt, AdmissionError>` (single atomic transaction checking all 3 fence generations + open state + holder/expiry/budgets + spend reservation + unique `attempt_id`)
  - `reconcile_attempt(&self, attempt_id: &AttemptId, outcome: AttemptOutcome) -> Result<(), AdmissionError>` (including `UnknownAfterSend -> Quarantined`)
- **WIT `waddle:extension@4.0.0`**:
  - `variant credential-ref { tenant(string), invoking-user(string) }`
  - `resource inference-stream { next-chunk: func() -> result<option<inference-chunk>, inference-error>; terminal-usage: func() -> option<verified-usage>; }`
  - Synchronous Rust `Drop` on host `InferenceStream` table entry triggers non-blocking upstream cancellation channel.

## Code Layout

- `server/crates/waddle-broker-core/` — Pure-Rust types, traits, Verus-verified kernels (AAD, 3-fence guard, budget math, SSRF IP classifier, LRU FD pool)
- `server/crates/waddle-broker/` — Broker custody, envelope encryption, KMS, OpenFGA/SpiceDB authz client, Turso/Postgres `PlacementLeaseStore` & `AdmissionLedgerStore`, `ReplicaFence`, constrained provider egress
- `server/crates/waddle-server/src/db/` — `TenantStore` (Turso + Postgres), `account_id`/`tenant_id` migrations (`>= 2001`), tenant registry
- `server/crates/waddle-server/src/permissions/` — `PermissionBackend` trait, OpenFGA & SpiceDB & Local implementations, promoted `BootInstant`
- `server/crates/waddle-extensions/` — WIT `@4.0.0` bindings, streaming host resource + sync `Drop` cancel, narrowed WASI linker, per-tenant semaphores, stderr/log suppression
- `server/wit/waddle-extension.wit` — `waddle:extension@4.0.0`
- `server/extensions/{ai-chatbot,jev-judgments}/` — Migrated inference guests (no `outbound.http.request`, no `api_key` config)
- `server/specs/quint/` — Quint.SH protocol specs (`.qnt`)
- `server/specs/verus/` — Verus proof harness / specs (`.rs`)
- `server/specs/pobserve/` — PObserve runtime monitors (`.p`) + JSONL trace verifier
- `server/scripts/check-tenant-isolation.py` — Lexical/type architecture guard
- `server/docs/adrs/` — Superseding ADR for brokered typed inference
- `server/experiments/native-fixture/` — Extended two-cell, two-broker native fixture
- `chat/src/`, `apps/android/`, `apps/apple/`, `apps/linux/` — Client `/ai` disclosure & sealed credential IQ alignment
- `server/deployment.cue`, `server/observer-release.json`, `server/charts/waddle-server/templates/validations.yaml` — Deployment & Helm validation updates
