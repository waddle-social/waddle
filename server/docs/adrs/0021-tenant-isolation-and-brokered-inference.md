# ADR-0021: Tenant Isolation and Brokered Typed Inference

## Status

Accepted on 2026-11-25 for design decisions `D1`–`D10`.

Accepting these decisions selects the architecture, trait contracts, and formal
verification invariants for multi-tenant isolation and brokered typed inference.
Implementation and production activation remain gated on the
[Release Evidence & Rollback](#release-evidence--rollback) gates below.

- **Supersedes (scoped)**: Supersedes the host-AI prohibition in
  [RFC-0007](../rfcs/0007-ai-integrations.md) (lines 3–6, 25–27) and
  [RFC-0014](../rfcs/0014-bot-framework.md) (lines 231–235, 520–522, 619–621)
  **solely** for brokered typed inference exposed through the
  `waddle:extension@4.0.0` WIT contract. Retains RFC-0014's prohibition on
  direct cell/guest secret custody and unbrokered provider HTTP egress.
- **Updates**: Updates [ADR-0004](0004-turso-libsql-database.md) by introducing
  backend-agnostic storage (`TenantStore`, `PlacementLeaseStore`,
  `AdmissionLedgerStore`) and authorization (`PermissionBackend`) traits in
  `waddle-broker-core`, supporting both self-hosted Turso (per-tenant WAL `.db`)
  and PostgreSQL (`tenant_id`-scoped), paired with OpenFGA and SpiceDB.

## Context

Waddle must isolate multiple tenants cleanly across both a shared cell pool and
dedicated single-tenant cells using identical binaries and placement contracts,
while allowing tenants and members to invoke external LLM inference from Wasm
extensions (`ai-chatbot`, `jev-judgments`, and future extensions) without
exposing provider credentials or raw prompts to untrusted boundaries.

Previous design constraints (`RFC-0007`, `RFC-0014`) forbade host-level AI
abstractions and placed provider keys inside extension configuration
(`configSecretFiles` / raw `api_key`) with direct `outbound.http.request`
access. Under multi-tenant hosting, that model has unacceptable blast radius:
1. A compromised cell or extension could exfiltrate plaintext provider keys,
   bypass spend/rate/model policies, or pivot via SSRF (`169.254.169.254`,
   internal cluster services).
2. Unfenced concurrent cell replicas sharing a SPIFFE workload role could issue
   overlapping writes or post-revocation provider dispatches.
3. Member-funded credentials require independent IdP user intent and client-side
   sealing so cells cannot mint, steal, or silently substitute a payer.

To avoid lock-in and verify backend parity continuously, storage and
authorization sit behind backend-agnostic traits in `waddle-broker-core`:
- **Primary multi-tenant stack**: Self-hosted **Turso** (`turso` Rust engine in
  standard WAL mode, per-tenant `<tenant>.db` behind a bounded LRU FD pool +
  mTLS storage-host) + **OpenFGA** (one `store_id` per tenant, pinned
  `authorization_model_id`, Modular Models `schema 1.2`, `HIGHER_CONSISTENCY`).
- **Secondary parity stack**: **PostgreSQL** (`tenant_id`-scoped schemas/rows,
  `FOR SHARE`/epoch-guarded `EnteredTransaction`, restricted runtime roles) +
  **SpiceDB** (tenant-scoped object IDs within `MAX_AUTHZ_ID_BYTES = 765`,
  `FullyConsistent` reads, `AUTHZ_SCHEMA_VERSION = 3`).

## Decisions

| ID | Decision |
| --- | --- |
| D1 | Deploy one independently operated, replicated **Credential & Inference Broker** (`waddle-broker`) per trust domain with its own namespace, `SecretStore` (never `ClusterSecretStore`), distinct SPIFFE workload identity, DB permissions, network policy, and KMS grants. |
| D2 | Supersede RFC-0007/0014's host-AI prohibition **only** for brokered typed inference via `waddle:extension@4.0.0` (`tenant(name)` / `invoking-user(provider)`, bounded streaming resource with synchronous Rust `Drop` -> non-blocking upstream cancel). Remove `outbound.http.request`, `allowedHttpOrigins`, raw `api_key`, and `configSecretFiles` from inference guests. |
| D3 | Define backend-agnostic async traits and canonical newtypes in `server/crates/waddle-broker-core`: `TenantId`, `AccountId`, `HolderInstance`, `TenantStore`, `PlacementLeaseStore`, `AdmissionLedgerStore`, and `PermissionBackend` (replacing the closed `PermissionActorBackend` enum). |
| D4 | Adopt **Option (a)** for self-hosted multi-pod Turso: a thin mTLS-authenticated Turso storage-host service enforcing `(tenant_id, holder_id, placement_generation)` on every query/batch over standard WAL-mode (`PRAGMA journal_mode = WAL`; never pre-1.0 `mvcc`) per-tenant `<tenant>.db` files behind a bounded LRU FD pool. Apply the same fencing contract to the replicated broker ledger (tenant-sharded Turso `.db` via storage-host or HA PostgreSQL alongside OpenFGA). |
| D5 | Enforce single-writer tenant placement via `PlacementLeaseStore` (composing with / replacing `ClaimStore` in `clustering/claims.rs` and `EPOCH_GUARDED_TABLES`, distinct from `clustering/lease.rs` swarm keypair-slot leases), bound to SPIFFE mTLS + RFC 9266 `tls-exporter` ephemeral incarnation keys, authoritative DB clock (`CAST(unixepoch('subsec') * 1000 AS INTEGER)` on Turso; `now()` on Postgres), and promoted `BootInstant` monotonic deadlines. |
| D6 | Require independent user/admin intent for credential and inference policy mutations: `Colony` JWT / WebAuthn proofs verified directly by the broker against IdP JWKS, registered client device signing keys, and a client-pinned root broker-key-signing public key authenticating rotatable XMPP-delivered encryption subkeys. Reject `native_users` SCRAM and cell-asserted admin flags for member-funded credentials and policy mutations. |
| D7 | Protect credentials in the broker using authenticated envelope encryption (AES-256-GCM / XChaCha20-Poly1305) with length-prefixed AAD binding `(trust_domain, tenant_id, account_id, provider, credential_id, version)`, wrapping per-user DEKs under per-tenant DEKs under broker-only KMS keys, with a durable destruction ledger enforced on every restore before reopening egress. |
| D8 | Commit every inference dispatch through a single atomic 3-fence admission transaction (`AdmissionLedgerStore::admit_attempt`) checking `(tenant, credential)`, `(tenant, room_or_space)`, and `(tenant, placement)` fences alongside `HIGHER_CONSISTENCY` (OpenFGA) / `FullyConsistent` (SpiceDB) checks (rejecting `contextual_tuples`), short-tick broker `ReplicaFence` revocation abort, strict payer separation (never silently falling back from member to tenant), and `unknown_after_send` quarantine (never retrying or bypassing `MAX_ATTEMPTS`). |
| D9 | Enforce room privacy in the host (`ExtensionHostAdapter::query_mam` restricted to `source_room` + post-read `archive_still_authorized`), emit room disclosure server-side only after broker admission, and allow only qualified zero-data-retention / no-training provider routes (`data_collection: "deny"`, `zdr: true`). |
| D10 | Ship tenant-funded inference first; gate member-funded inference on independent IdP/device-key intent, pinned root key, provider qualification, and restore/revocation drills. Emit only closed `MetricAttribute` enums (never raw `tenant_id`, JIDs, prompts, or secrets). Rollback disables features without weakening fences or restoring plaintext keys to cells. |

## Trust-Boundary Inventory

| # | Boundary | Untrusted For | Trusted / Enforced Responsibility |
| --- | --- | --- | --- |
| 1 | **Cell (`waddle-server`)** | Plaintext provider keys, member/admin intent, `module:<store_id>\|inference` authz writes, unbrokered provider HTTP | XMPP routing, `module:<store_id>\|chat` within leased tenants, host MAM `source_room` enforcement, Wasmtime sandboxing & per-tenant semaphores |
| 2 | **Ingress Authentication** | Unauthenticated or replayed credential mutations | WebSocket C2S / S2S transport termination, stripping `urn:waddle:*` credential payloads on all IQ error bounces, keeping unsealed secrets out of `sm_unacked` |
| 3 | **Identity Provider (`Colony`)** | Cell runtime routing | Authoritative issuer of user/admin intent proofs (JWT / WebAuthn) verified directly by the broker via JWKS |
| 4 | **Storage (`Turso` storage-host & `PostgreSQL`)** | Client-provided wall clocks or unfenced writes | Enforcing `(tenant_id, holder_id, placement_generation)` (Turso) or `FOR SHARE`/epoch-guarded `EnteredTransaction` + restricted roles (PostgreSQL) |
| 5 | **Authorization (`OpenFGA` & `SpiceDB`)** | Untrusted `contextual_tuples` on broker admission | OpenFGA per-tenant `store_id`, pinned `authorization_model_id`, Modular Models (`chat` vs `inference`), `HIGHER_CONSISTENCY`; SpiceDB tenant-scoped IDs (`<= 765` B), `FullyConsistent`, `AUTHZ_SCHEMA_VERSION = 3` |
| 6 | **Credential & Inference Broker (`waddle-broker`)** | Cell-asserted admin flags or unverified payer claims | Isolated namespace/`SecretStore`/SPIFFE/KMS/NetworkPolicy; sole holder of decrypted provider keys, envelope encryption, destruction ledger, 3-fence admission, `ReplicaFence`, and constrained provider egress |
| 7 | **KMS (Scaleway Key Manager / isolated Vault Transit)** | Cell access | Wrapping per-tenant DEKs and destruction authority; accessible only to broker workload identity |
| 8 | **Clients (`chat/`, Apple, Android, Linux)** | Server-side policy enforcement | Holding pinned root broker-key-signing public key and device signing key; sealing secrets to rotatable broker subkeys; never persisting unsealed secrets in `PersistedSmResumeState` `localStorage` or broadcasting premature `/ai` prompts before admission |
| 9 | **External Inference Providers** | Internal cluster access, training/retention | Reached solely from broker egress adapters via approved `(provider, deployment, model, operation)` profiles, DNS-resolved SSRF IP blocklist, TLS identity pinning, `.no_proxy()`, no redirects, ZDR/no-training qualification |

## Ownership Matrix

| Domain | Authoritative Owner | Enforcement Mechanism | Explicitly Forbidden Writers |
| --- | --- | --- | --- |
| **Placement writes** | Placement controller / claiming cell via `PlacementLeaseStore` | Durable CAS + SPIFFE mTLS + RFC 9266 `tls-exporter` ephemeral incarnation key proof + DB-clock TTL | Stale leaseholders, same-role replicas with different incarnation keys, unauthenticated request-field spoofing |
| **Consent & inference policy writes** | Credential & Inference Broker (`waddle-broker`) | OpenFGA `module:<store_id>\|inference` (or SpiceDB broker namespace) gated on broker-verified IdP proof | Cells (`waddle-server`), Wasm extensions, `native_users` SCRAM sessions |
| **Device key enrollment** | User client + Broker | IdP proof (`Colony` JWT / WebAuthn binding challenge, audience, client pubkey) verified and persisted by broker | Cells, `native_users` SCRAM, unverified XMPP stanzas |
| **Key destruction & restore enforcement** | Broker + KMS | Durable broker-owned destruction ledger + KMS wrapping key destruction; broker startup/restore gate keeps egress closed until ledger is applied | Cells, backup restore scripts bypassing the broker restore gate |
| **Provider & route qualification** | Trust-domain operator / Broker qualification registry | Static + signed `(provider, tier, endpoint, model, downstream_routing)` allowlist requiring `zdr: true` and `data_collection: "deny"` | Cells, extensions, runtime user-supplied base URLs or headers |

## Turso Multi-Pod Topology & Baseline Compatibility Spike

### Multi-Pod Topology (Option a)
In multi-pod deployments using the primary Turso stack:
1. Each tenant owns an isolated SQLite-format database file `<data_dir>/<tenant_id>.db`
   opened in standard WAL mode (`PRAGMA journal_mode = WAL`; never pre-1.0 `mvcc`).
2. A thin mTLS-authenticated **Turso storage-host** service fronts the local NVMe
   volume behind a bounded LRU handle/FD pool (accounting proven in
   `server/specs/verus/lru_fd_pool.rs`).
3. Every query and batch sent from a cell or broker replica presents
   `(tenant_id, holder_id, placement_generation)` over SPIFFE mTLS bound to the
   incarnation key (`tls-exporter`). The storage-host verifies the fence against
   `PlacementLeaseStore` before executing any write, denying stale-generation or
   same-role stale-incarnation writers.
4. The replicated broker admission and placement ledgers use the exact same
   `PlacementLeaseStore` and `AdmissionLedgerStore` traits over either
   tenant-sharded Turso `.db` files via the storage-host or HA PostgreSQL
   alongside OpenFGA.

### Verified Turso `0.8.4` WAL Baseline Spike Results
Executed in `server/crates/waddle-broker-core/tests/turso_baseline_spike.rs`
against `turso = { version = "0.8.4", default-features = false, features = ["fts"] }`
(disabling default `mimalloc` to avoid process-wide `#[global_allocator]`
collisions) on Rust `1.98.1`:

| Capability Tested | Observed Result on `turso 0.8.4` | Architectural Impact |
| --- | --- | --- |
| `PRAGMA journal_mode = WAL` | **PASS** — returns `"wal"` on temp file DB | Standard WAL mode confirmed; `mvcc` remains forbidden. |
| Full `server/crates/waddle-server/sql/baseline/sqlite.sql` (1,437 lines, 228 DDL statements, 2 seed rows) | **PASS** — `execute_batch` applies cleanly; `sqlite_master` registers all **8** triggers | No baseline schema rewrites required for Turso WAL mode. |
| All 8 `CREATE TRIGGER` statements (`ingress_messages_*`, `ingress_owner_rejections_*`, `pending_notification_recovery_*`) | **PASS** — all 8 triggers enforce both valid transitions and `RAISE(ABORT, ...)` rejections (including correlated `EXISTS` subqueries and `AFTER INSERT` self-table update) | Ingress source-binding immutability and recovery-ordinal monotonicity hold natively on Turso. |
| Waddle runtime MAM full-text search (`body GLOB ?` with Unicode case-fold & escaped `*`/`[`) | **PASS** — exact match on ASCII, Unicode (`Überraschung`), and literal `*star*` | Existing XEP-0431 MAM search works unchanged on Turso. |
| Standard SQLite `FTS5` (`CREATE VIRTUAL TABLE ... USING fts5(...)`) | **UNSUPPORTED (expected)** — `Err("Parse error: no such module: fts5")` | Waddle does not use FTS5 today; if indexed full-text search is added later, Turso uses Tantivy `USING fts`, not SQLite FTS5. |
| Turso native Tantivy FTS (`CREATE INDEX ... USING fts (body)` + `fts_match(body, 'release')` with `experimental_index_method(true)`) | **PASS** — index creation `Ok(0)` and `fts_match` returned `["m1"]` | Viable optional path if indexed FTS is added to Turso tenants in the future. |
| `SELECT CAST(unixepoch('subsec') * 1000 AS INTEGER)` | **PASS** — subsecond millisecond precision, strictly monotonic across 25 ms sleep, works inside lease `INSERT`/CAS SQL | Confirmed as authoritative Turso DB clock for `PlacementLeaseStore` expiry. |
| WAL checkpoint / flush (`PRAGMA wal_checkpoint(TRUNCATE)`, `PRAGMA wal_checkpoint(PASSIVE)`, `conn.cacheflush()`) | **PASS** — all three returned `Ok` | Confirmed working eviction primitives for M2's bounded LRU FD pool (explicit checkpoint before closing evicted handles, since `wal_autocheckpoint` is unsupported). |

## Measurable Bounds

| Resource / Metric | Pre-Rollout Bound | Enforcement Layer |
| --- | --- | --- |
| **Concurrent requests** | Default `8` in-flight inference requests per tenant (configurable per tier); max `8` tool steps per multi-turn run; minimum `2` dedicated reserved control-plane slots for lease heartbeats & cancellation | Per-tenant Tokio semaphore + `AdmissionLedgerStore` run/step ledger |
| **Tokens** | Explicit per-invocation `max_input_tokens` / `max_output_tokens` and per-run cumulative `max_spend_microcents` cap | `AdmissionLedgerStore` + Verus-verified `budget_ledger` (`spent + reserved <= signed_cap`, zero overflow) |
| **Bytes & memory** | Max request payload `256 KiB`; max response/stream total `1 MiB`; max stream chunk `16 KiB`; bounded stream channel `<= 32` chunks per hop; Wasm linear memory capped at `memories * memory_bytes` per tenant (e.g., `64 MiB` per tenant semaphore) | Broker egress adapter + WIT `@4.0.0` host stream + Wasmtime `ResourceLimiter` |
| **File descriptors** | `capacity <= min(256, (RLIMIT_NOFILE - reserved_fds) / 3)` open tenant DBs (accounting for `.db` + `-wal` + `-shm` = 3 FDs per open DB), checkpoint-on-evict, never evicting pinned handles | Turso LRU FD pool + Verus-verified `lru_fd_pool` |
| **Latency & deadlines** | Broker admission p99 `< 100 ms`; streaming decoupled from `15 s` `STANZA_HANDLER_WEDGE_TIMEOUT`; Wasm fuel + wall-clock epoch interruption (`<= 30 s` non-streaming, bounded per-chunk idle timeout); `ReplicaFence` tick `250 ms` (`FENCE_LEASE_TTL = 5 s`, `FENCE_LEASE_MARGIN = 1 s`, `FENCE_EXPIRY_GRACE = 1 s`, `FENCE_ACKNOWLEDGEMENT_DEADLINE = 10 s`) | `waddle-extensions`, `waddle-broker`, promoted `BootInstant` |
| **Cancellation & drain** | Synchronous Rust `Drop` on host `InferenceStream` triggers non-blocking upstream cancel `< 50 ms` locally and within `1` `ReplicaFence` tick (`<= 250 ms` nominal, `<= 10 s` hard bound) across peer broker replicas; pod drain quiesces streams within `WADDLE_DRAIN_TIMEOUT_SECS` (default `30 s`) | Host `InferenceStream::drop`, broker `ReplicaFence`, graceful shutdown |

## Formal Verification & Release Evidence Gates

### Formal Verification Artifacts (Milestone 1)
- **Quint.SH (`server/specs/quint/`)**:
  - `three_fence_admission.qnt` — no post-revoke-ack dispatch, at-most-once dispatch per attempt, strict fence monotonicity, zero silent payer fallback, fail-closed on stale generation.
  - `placement_lease.qnt` — single active holder per generation, fenced handoff ordering, same-role stale-incarnation denial, monotonic lease generation.
  - `credential_lifecycle_restore.qnt` — destroyed keys unusable after stale backup restore, XEP-0198 enrollment nonce idempotency, plaintext custody boundary (`{client, broker, provider}`).
  - `stream_cancel_and_quarantine.qnt` — budget conservation, zero re-billing on `unknown_after_send` quarantine, permit bounds, bounded cancel latency.
- **Verus (`server/specs/verus/`)**:
  - `aad_codec.rs` — length-prefixed AAD injectivity (`encode(a) == encode(b) ==> a == b`).
  - `three_fence_guard.rs` — strict monotonicity, zero `u64` wraparound, exact fail-closed admission equivalence.
  - `budget_ledger.rs` — zero overflow/underflow, `spent + reserved <= signed_cap`, monotonic non-decreasing `spent`.
  - `ssrf_ip_filter.rs` — completeness over loopback, RFC1918, CGNAT, link-local/metadata, ULA, unspecified, broadcast, multicast, and `::ffff:0:0/96`.
  - `lru_fd_pool.rs` — `open_count <= capacity <= rlimit_nofile - reserved_fds` and never evicting a pinned handle.
- **PObserve (`server/specs/pobserve/`) & `waddle-broker-core::trace_verifier`**:
  - `AdmissionFenceMonitor.p`, `PlacementLeaseMonitor.p`, `StreamCancellationMonitor.p` compiled with `p compile --mode pobserve`, paired with the secret-free JSONL runtime verifier in `waddle-broker-core`.

### Release Evidence & Rollback
Production activation requires passing all 5 Release Evidence gates across both
`(Turso + OpenFGA)` and `(PostgreSQL + SpiceDB)` stacks:
1. **Backend parity & tenant isolation**: cross-tenant read/write/cache/authz isolation and identical contract behavior across both stacks.
2. **Deterministic fencing & single dispatch**: barrier tests and PObserve trace verification proving zero stale writes, zero post-revoke dispatches, and zero `unknown_after_send` re-billing.
3. **Authority & boundary denials**: cells cannot write `inference` authz tuples, mint member intent, bypass SSRF blocklists, or read cross-room MAM archives.
4. **E2E guest migration & zero-leak scans**: `ai-chatbot` and `jev-judgments` run exclusively on `waddle:extension@4.0.0` with zero secret sentinels in logs, metrics, `sm_unacked`, IQ error bounces, or `localStorage`.
5. **Restore & outage resilience**: stale-backup restore drill enforces the destruction ledger before reopening egress; KMS/OpenFGA/SpiceDB/storage-host outages fail closed and recover cleanly.

**Rollback discipline**: Disabling inference or rolling back a release toggles
broker admission / tenant feature flags off while preserving all placement
fences, authz module boundaries, and sealed custody invariants. Rollback MUST
NEVER weaken fences or re-introduce plaintext provider keys into cell configs.
