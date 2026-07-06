# ADR-0017 Phase 3 Plan — Postgres-Authoritative Ownership Claims

Companion execution note to [ADR-0017](./0017-horizontal-scaling-remote-actors.md), the
[Phase 1 completion note](./0017-phase1-completion-authoritative-registration.md), and the
[Phase 2 plan](./0017-phase2-plan-libp2p-swarm.md). Status: **planned, not yet implemented**.
Tracking: issue #1195 (ADR-0017 horizontal-scaling epic), which lists this phase's
checkboxes verbatim as:

- [ ] Epoch-fenced Postgres ownership claims
- [ ] Fenced SM-session persistence + cross-node XEP-0198 resume via claim-steal
- [ ] Durable MUC room ownership
- [ ] XEP-0397 In-band Session Reset (ISR)
- [ ] Retire the DashMap *selection* surface (`select_routable_resources_for_user` /
      `get_resources_for_user`) + the transitional Slice-1 liveness filter, migrating the
      ~14 remaining consumers onto the actor

The epic's own checklist text settles scoping-map open question 3: **D7 (DashMap
selection retirement) is explicit Phase 3 scope**, not a later-phase deferral.

## Goal

Give every claimed entity (`UserActor`, `RoomActor`, SM session, ISR token scope) a real,
epoch-fenced Postgres ownership record, and make every durable consumer of that ownership
(SM persistence, SM janitors, MUC room state, ISR) claim-scoped instead of assuming
exclusive table access. Land the `nodes`/`claims`/`steal_intents` schema and the exact CAS
contract from ADR element 4, the Postgres-fenced `SmPersistenceStorage` split from element
1, cross-node XEP-0198 resume via claim-steal (element 8), durable MUC ownership with
re-election (element 7), and a cluster-correct XEP-0397 ISR store (element 10) — or leave
ISR unadvertised, per the Phase 0 removal, if the store does not ship with it.

## Non-goals (Phase 4 exclusions)

- **No cross-node stanza routing GA.** DM/MUC/presence traffic does not route
  cross-node in Phase 3; the relay actor stays discovery/handshake-only outside the
  narrow uses this phase adds (SM live-steal handshake, MUC Demote ask, claims-table
  liveness lookups riding the control-plane pool — never the stanza hot path; fenced
  ISR/SM/MUC write transactions run on the main pool, per Slice 0).
- **No cross-node janitor-flush test.** Slice 5's guaranteed-flush test is scoped to
  same-node delivery only. The cross-node leg (sender→owner handoff fails, recipient's
  socket lives on a *third* node, delivery over the ordered relay) requires the GA
  cross-node stanza routing this phase explicitly excludes above, and which the ADR's
  own Implementation Plan assigns to Phase 4 (the Phase 2 plan states plainly: "Does
  not route any stanza cross-node (Phase 4)"). Coordinator ruling: Slice 5's test
  smuggled this in as scope creep; it is deferred to Phase 4 and recorded as
  deviation 14 below.
- **No Helm unlock.** `replicaCount > 1` and `clustering.enabled` remain hard-locked in
  the chart (Phase 0 guard). Phase 3 is exercised exclusively by the multi-process
  harness (`server/crates/waddle-server/tests/clustering_cluster_e2e.rs`) against a
  shared Postgres, exactly as Phase 2 was.
- **No NetworkPolicy, PDB, or keypair-pool Helm hook job** — those are Phase 4
  deliverables (element 3/4's Helm-facing pieces).
- **No per-message allowlist origin re-validation** — still deferred (carried risk,
  below).
- **No StatefulSet/per-ordinal-Secret rework** — the pool-lease approach stands.

## Resolution of the 8 scoping-map open questions

### Q1 — Module home for `claims`/`nodes`/`steal_intents`, and the `ClaimStore` trait

**Code research finding that changes the premise**: there is **no `ClaimStore` trait
today**. `session_registry/claims.rs` (`server/crates/waddle-xmpp/src/stream_management/
session_registry/claims.rs`) is an `impl` block of inherent methods directly on
`InMemorySmSessionRegistry` (`claim_session`, `release_claim`, `complete_claim`,
`complete_claim_if_resumable`, …), gated by a private `Vec<Arc<tokio::sync::Mutex<()>>>`
stream-lock shard array in `core.rs` — not a pluggable abstraction. Extracting a trait is
net-new work, not a rename, and this is called out explicitly in the deviations list
below.

**Decision**: split the trait across crates by compile-time reach, not by
convenience:

- **`ClaimStore` trait + typed `Entity`/`EntityType`/`ClaimEpoch` value types + the
  in-process (single-node) impl live in `waddle-xmpp`**, in a new top-level module
  `server/crates/waddle-xmpp/src/ownership/` (sibling to `registry/`, `muc/`,
  `stream_management/`, `isr/`), **unconditionally compiled — no Cargo feature gate**.
  Rationale: `UserActor`/`RoomActor`/SM session code lives in `waddle-xmpp`, which has
  no `clustering` feature and must keep working (and keep needing *some* `ClaimStore`)
  in every build, including a plain single-node SQLite deployment that never touches
  Postgres. Gating the trait itself behind `clustering` would make ordinary SM/session
  code in `waddle-xmpp` either depend on a feature it can't see or grow two code paths
  for "clustering compiled" vs. not — exactly the kind of scattered conditional the
  ADR's element-1 text explicitly forbids ("no conditional SQL is scattered into shared
  statements").
- **The Postgres CAS impl lives in `waddle-server/src/clustering/claims.rs`**, gated
  `#[cfg(feature = "clustering")]`, following the exact `ensure_schema`-on-a-trait
  sibling-module pattern of `lease.rs`/`allowlist.rs` (own `ensure_schema`, own error
  enum, own Postgres-only impl struct, unit + Postgres-gated tests in the same file).
  It implements `waddle_xmpp::ownership::ClaimStore` for a `PostgresClaimStore` type
  defined in `waddle-server` — legal under Rust's orphan rule (local type, foreign
  trait) without needing the trait itself to live downstream.
- `waddle-server/src/clustering/mod.rs` gains `pub mod claims;` next to the existing
  `pub mod lease;` / `pub mod allowlist;` / `pub mod relay;`, with the same "public so
  the harness can provision schema through the production path" doc-comment convention.

This resolves the "how does the non-Postgres arm plug in" half of Q2 as a byproduct:
the trait's existence is what lets `waddle-server` choose the impl at startup
(`clustering.enabled` → `PostgresClaimStore`; otherwise → `InProcessClaimStore`) and
inject a single `Arc<dyn ClaimStore>` into session/registry code — see Q2.

### Q2 — How the non-Postgres `ClaimStore` arm plugs into `session_registry/claims.rs`

**Decision: retrofit, not wrap.** `session_registry/claims.rs`'s inherent methods are
refactored to delegate to an injected `Arc<dyn ClaimStore>` field on
`InMemorySmSessionRegistry` rather than owning the claim/lock bookkeeping directly —
the store's result *gates* whether a claim is granted (`AlreadyClaimed` reproduces the
registry's pre-Slice-1 already-claimed outcome exactly), and every terminal path that
ends a claim (`release_claim`, every branch of `complete_claim`/
`complete_claim_if_resumable`, and `invalidate_sessions_for_jid`'s removal of a claimed
session) releases the store entry, so it cannot leak. The claim/lock bookkeeping that
exists today becomes the body of `InProcessClaimStore` in `waddle-xmpp/src/ownership/
in_process.rs`; `InMemorySmSessionRegistry` keeps its existing `stream_locks` shard
array only as the in-process **contention optimization** the ADR already names for
`StreamLockMap` in the SM-persistence context (element 4: "the lock map remains only as
an in-process contention optimization") — never as the source of correctness once a
`ClaimStore` exists to be the actual authority, even in single-node mode. A *wrap*
(keep the existing methods, layer a `ClaimStore`-shaped facade in front for callers
that want one) was rejected: it would leave two divergent definitions of "claim
semantics" — the legacy inherent methods for existing callers, the trait impl for new
Phase 3 code — able to drift, which is exactly the two-source-of-truth failure mode
Phase 1's own postmortem (register-lag false-negative) already burned the project on
once.

`UserActor`/`RoomActor` claims (which do not exist as claims at all today — they are
pure in-memory ownership by construction, one process, no contention) get trivial
`InProcessClaimStore` semantics too, but **"trivial" is not "idempotent" or
"unconditional"**: single-node `acquire` succeeds iff the entity is currently
unclaimed — the exact same contract `PostgresClaimStore::acquire` enforces across
nodes — so it returns `ClaimError::AlreadyClaimed` on a second acquire against a
still-held entity. What *is* trivial about the single-node case is that there is only
one node to contend with on the entity's actual state transition once a claim is
granted (`steal_stale`/`steal_for_resume` always succeed against an
already-acquired entity, since there is no second node's CAS to lose to), and
`heartbeat`/`demotion_reconciliation` are no-ops (there is no second node's liveness to
track). Same-node contention on `acquire` (two connections racing to claim the same SM
session) is real and enforced, not a no-op. This gives every entity type — not just SM
sessions — one `ClaimStore` abstraction from day one of the trait's existence, so
Phase 3's Postgres impl is a drop-in substitution, never a parallel concept invented
later for MUC/UserActor claims.

### Q3 — D7 scope (settled by the epic)

**Settled, no further analysis needed.** Issue #1195's Phase 3 checklist explicitly
lists retiring the DashMap selection surface and the Slice-1 liveness filter,
"migrating the ~14 remaining consumers onto the actor," as this phase's own
deliverable. See Slice 9 below. The Phase 1 completion note's "later phase" language is
superseded by the epic text.

### Q4 — Multi-process harness file path (settled by code research)

`server/crates/waddle-server/tests/clustering_cluster_e2e.rs` (confirmed; 567 lines
today, `#![cfg(feature = "clustering")]`). It already contains the two `#[ignore]`
scaffolds this phase activates:

```rust
#[tokio::test]
#[ignore = "Phase 3: requires nodes-table heartbeat fencing"]
async fn lone_survivor_and_isolation_fencing() {}

#[tokio::test]
#[ignore = "Phase 3: requires the durable-queue fallback"]
async fn partial_partition_degrades_without_fencing() {}
```

Both are empty-bodied named placeholders (no partial assertions to preserve or
reconcile) — Slice 11 fills them in. A third `#[ignore]` (`dead_publisher_record_visibility_window`,
the kademlia-TTL manual measurement) stays ignored; it is explicitly out of Phase 3
scope (kademlia constants are not addressed by this phase).

### Q5 — `pod_template_hash` claim placement vs. keypair-slot lease interaction

**Decision: no coupling.** The two mechanisms answer different questions and this plan
keeps them orthogonal rather than inventing an interaction the ADR never asked for:
`clustering_keypair_slots` identifies *which libp2p identity* a process holds (an
identity-pool concern, unrelated to code version); `clustering_nodes.pod_template_hash`
identifies *which deployment generation* a process belongs to (a rollout concern, used
only by the claim-acquisition backoff-vs-no-backoff rule in the ADR's drain sequence:
"pods whose hash matches the newest generation acquire released claims without backoff
while old-generation pods back off first"). A node's `pod_template_hash` is read once
from the Kubernetes downward API at startup and stored on its `clustering_nodes` row;
it plays no role in which keypair slot that same node leases, and the keypair-slot
lease plays no role in claim placement. This is stated explicitly here (rather than
left implicit) precisely because the scoping map flagged it as unaddressed — the
resolution is "they don't interact," not "the interaction is TBD."

**Mechanism, PROPOSED (coordinator ruling)**: `clustering_nodes` rows carry
`(pod_template_hash, first_seen / registered_at)` as already shown in the Slice 1 DDL.
"The current generation" for the acquire-backoff rule is defined operationally, not by
a chart-injected expectation: it is **the `pod_template_hash` of the most recently
registered live node** (max `first_seen`/`registered_at` among non-expired `nodes`
rows). Whatever generation is actively rolling in — including the old generation
during a rollback, which re-registers newer rows than the half-rolled-out generation
it is replacing — is, by this definition, automatically "current," and acquires
released claims without backoff; older-generation live pods back off first. This
needs no coordination beyond what `nodes` already durably records. Because the
backoff/no-backoff distinction is only a **placement heuristic** — it decides who
*tries first*, never who *wins* — misclassifying the current generation (e.g. a
racing pair of same-generation pods both registering within the same instant) cannot
violate ownership: the claims CAS (element 4) is the sole authority over who actually
holds an entity, so a wrong backoff guess only costs a few wasted acquire attempts or
a slower rebalance, never a double-owned claim.

### Q6 — `lease_ttl` cluster-wide configuration mechanism

**Decision: reuse the Phase 2 mechanism (Helm value / env var), not a `nodes`-table
config row.** Phase 2 already established this exact pattern for the keypair-slot
lease: `ClusteringLeaseConfig.lease_ttl` (`WADDLE_CLUSTERING_LEASE_TTL_MS`, parsed and
validated in `config.rs`, invariant `lease_ttl >= heartbeat_interval * 2` enforced at
parse time). Phase 3 adds a **second**, conceptually distinct TTL — entity/node
ownership lease, not keypair-slot lease — as its own config value
(`ClusteringNodeLeaseConfig` or new fields alongside the existing
`ClusteringLeaseConfig`; naming TBD in Slice 1, but the mechanism is settled: env var →
`ServerConfig`, read into every CAS call, never read back out of Postgres). A
config-row-in-`nodes` approach was rejected for two reasons: (a) it creates a
chicken-and-egg bootstrap problem (a node must know its TTL before it can register the
very row that would carry it), and (b) it adds schema churn for a value that changes
rarely and, per the ADR's own text, is "treated as a coordinated cluster setting" during
rollout anyway — an env var rolled through the Deployment achieves that coordination
with no new column.

### Q7 — Phase 0 singleton guard vs. Phase 3 `nodes` table

**Decision: keep them structurally separate; do not merge.** Phase 0's exclusive
cluster-singleton lease row exists specifically for the **non-clustering** path
(including SQLite, which the Phase 3 `nodes`/`claims` schema never supports — it is
Postgres-only per element 1). The two mechanisms are gated by the same top-level
branch the codebase already has (`clustering.enabled`): when clustering is disabled,
Phase 0's guard is the only ownership mechanism running, exactly as it does today, and
the `clustering_nodes` table is not even created. When clustering is enabled, node
registration into `clustering_nodes` plus the claims CAS *is* the ownership mechanism,
and Phase 0's singleton-lease code path is simply not engaged (bypassed, not deleted —
it remains the correct behavior for the non-clustering deployments that are still the
default). Merging them was considered and rejected: Phase 0's guard solves "prevent an
*accidental* second replica of a deployment that was never meant to run more than one,"
a safety net; Phase 3's claims solve "coordinate *deliberate* concurrent replicas," a
protocol. Conflating a safety net with a protocol risks the guard's simplicity (and its
value as a last-resort backstop against operator error) leaking Phase 3 complexity into
a code path that exists precisely to be trustworthy when everything else about
clustering is switched off.

### Q8 — ISR token storage shape

**Decision: a dedicated `clustering_isr_tokens` table** (Postgres-only, PROPOSED SQL —
see Slice 8), keyed by the SM-ID (the same non-secret key as the SM session's claim
`entity`), not columns bolted onto `sm_sessions`/`sm_unacked`. Rationale, favoring the
option with less schema churn later: ISR tokens have a distinct lifecycle
(single-use, rotated on every successful ISR, destroyed outright on failed-token auth
per the XEP's anti-brute-force MUST) that is orthogonal to the SM session's own
lifecycle (a session can exist with no ISR token at all — ISR is opt-in per
`<isr-enable/>`). Adding `isr_token`/`isr_mechanism`/`isr_created_at` columns to the
shared, cross-driver `sm_sessions` table would (a) force SQLite's byte-identical schema
to carry ISR columns it can structurally never use safely (no fencing), violating the
"portable impl and schema remain byte-identical for SQLite" element-1 invariant unless
those columns are always `NULL` on SQLite forever, and (b) couple two independently
-versioned features' migrations together. A separate table sidesteps both.

**Compounding decision, flagged for explicit coordinator sign-off**: ISR advertisement
is gated on `clustering.enabled && Postgres`, full stop — single-node/SQLite
deployments do **not** regain ISR in Phase 3.

**Code research correction — the "dead code" premise was wrong.** `IsrTokenStore`
(`waddle-xmpp/src/isr/store.rs`) is **not** fully unwired: `create_token` is a live,
already-exercised call path — `handle_isr_token_request_iq`
(`server/crates/waddle-server/src/server/routes/websocket/handlers/iq/isr_token.rs:12-16`)
calls it, is dispatched from `handlers/iq/mod.rs` (imported at `:140`, matched and
invoked at `:302-303` on `is_isr_token_request`), and the shared store is constructed
in production at `server/crates/waddle-server/src/server/http.rs:626`
(`waddle_xmpp::isr::create_shared_store()`); it is e2e-tested end-to-end in
`server/crates/waddle-server/tests/xep0054_0049_0191_ws.rs`
(`websocket_isr_token_request_returns_token`, `:660-678`). Only `validate_token`/
`consume_token` are genuinely unwired. The plan's earlier "zero live call sites, already
dead code" claim is corrected here.

**Worse — the live path is a XEP-0397 conformance violation.** XEP-0397 mints and
returns an ISR token exclusively as an **inline** `<isr-enable/>`/`<isr-enabled/>`
element riding SASL2 `<authenticate/>`/`<success/>` (element 10, and the Slice 8
XEP fact-check below) — it is never issued via a standalone IQ round-trip. The live
`token-request` IQ handler (`urn:xmpp:isr:0` custom namespace, per the e2e test) is
therefore a live violation of this repo's XEP-conformance hard rule: it uses ISR
machinery under a shape XEP-0397 does not define, outside the XEP's own SASL2
envelope. This is not a new-code question; it is existing, currently-shipping,
non-conformant behavior.

**Decision, revised**: this plan extracts an `IsrTokenStore` trait mirroring the
`ClaimStore` split (trait + `InMemoryIsrTokenStore` in `waddle-xmpp`,
`PostgresIsrTokenStore` in `waddle-server/src/clustering/isr.rs`) and wires only the
Postgres path into the live resume flow, exactly as before — but Slice 8 additionally
**inventories and retires the IQ issuance path outright**: delete
`handle_isr_token_request_iq` and its `isr_token.rs` module, delete its dispatch in
`handlers/iq/mod.rs` (`is_isr_token_request` check and the `:302-303` call), and delete
`websocket_isr_token_request_returns_token` from `xep0054_0049_0191_ws.rs`, replacing
token issuance with the XEP-0397-conformant inline `<isr-enable/>`/`<isr-enabled/>`
flow on `<enable/>`/`<enabled/>`. Single-node ISR remains unadvertised exactly as
Phase 0 left it (`clustering.enabled && Postgres` gate, per above). This is a
scope-narrowing deviation from a literal reading of ADR element 10 (which does not
textually restrict ISR to clustered deployments) **and** a conformance-driven
retirement of shipped code, both called out again in the deviations list.

## Slice breakdown

Every slice is council-reviewable and independently CI-green, matching the Phase 2
convention. Ordering follows the scoping map: **Slice 0 (D8 foundation) → Slices 1–3
(D1) → Slice 4 (D2) → Slice 5 (D3) → Slice 6 (D4) → Slice 8 (D6)**, with **Slice 7 (D5)
parallel-after-Slice-1**, **Slice 9 (D7) trailing Slice 5**, and **Slices 10–11 (D9/D10)
last**.

---

### Slice 0 — DB capacity configurability + dedicated control-plane pool (D8 foundation)

Ships first because Slice 1's claims CAS and every heartbeat/claim-liveness statement
in every later slice must run on this pool, not the shared statement pool (element
4/12: "pool exhaustion must degrade stanza latency, never lease liveness"). Fencing
SELECTs inside fenced write transactions are *not* in that set — they run on the main
pool with the writes they guard; see the pool-assignment rule below.

**Locked spec** (element 12, verbatim): *"Pool size becomes a `DatabaseConfig` field
surfaced through Helm values in Phase 3, not Phase 4... The liveness control plane runs
on its own dedicated pool... the heartbeat CAS and claim CAS statements never queue
behind fenced bulk writes, backstop fencing SELECTs, claims-read storms, or janitor
batches."*

**Design**: `DatabaseConfig` gains a `pool_size: u32` field (default 10, preserving
today's hardcoded behavior at the two sites in `server/crates/waddle-server/src/
db/backend.rs` — `SqlxSqliteAdapter::connect` line 60 and `SqlxPostgresAdapter::connect`
line 74 — both currently `.max_connections(10)` with no config surface). A second,
independently-sized pool is added for the control plane: `ControlPlanePoolConfig {
size: u32 }` (small default, e.g. 4), constructed via the *same* `DatabaseAdapter`
trait/`connect_backend` function already in `backend.rs` — reusing the existing
`ConnectionGuard`/`Value`/parameter-rewrite machinery rather than inventing a second
connection-management type. `Database` gains a way to hand out a `ConnectionGuard`
against the control-plane pool specifically (e.g. `Database::control_plane_guard()`).

**Pool assignment is exhaustive and exclusive (blocker fix — corrects a
pool/transaction contradiction in the original draft)**: the control-plane pool hosts
**only node/claim liveness statements** — the keypair-slot lease heartbeat (`lease.rs`,
currently on the shared global pool per its own doc comment — moved here), the claims
acquire/steal/expire/heartbeat/demotion-reconciliation statements (Slice 1–3),
steal-intent CRUD (Slice 3), and claims point reads issued by the claims code itself.
**Nothing else runs on it.** In particular, the SM-fencing `FOR SHARE` SELECT (Slice 4)
does **not** run here: the fencing SELECT and the write it guards must share one
pooled connection inside one `Database::begin()` transaction (a bare correctness
requirement — a lock taken on a *different* connection than the write protects
nothing), so every fenced SM/MUC/ISR write transaction (Slices 4, 7, 8) — fencing
SELECT included — runs entirely on the **main (shared/data) pool**. This is exactly
what element 12 means by its isolation list: "the heartbeat CAS and claim CAS
statements never queue behind fenced bulk writes, **backstop fencing SELECTs**,
claims-read storms, or janitor batches" is a statement about what the *control-plane*
pool must never queue behind, not a statement that fencing SELECTs run on it — a
backstop fencing SELECT queuing on the control-plane pool would violate element 12 by
letting fenced-write contention degrade lease liveness, the exact metastable failure
this pool split exists to prevent. Slices 4, 7, and 8 below state this pool
assignment explicitly at their own dependency sections so it isn't left implicit
per-slice. Metrics (blocker fix — corrects a dead-code-forward-declaration
contradiction in the original draft): routing-cache hit/miss, NotOwner NACK
(sent/received, by entity type), and claims-table point-read rate — all named in
element 12's load model — do **not** get their counter/gauge definitions here. The
repo's dead-code hard rule prefers landing an instrument with its first caller over
`pub`-widening a forward-declared-but-uncalled function, so each instrument lands in
`clustering/metrics.rs` alongside the Slice 1-3 code that actually calls it (see
Slice 1's Files line) rather than ahead of time in this slice.

**Files**: `server/crates/waddle-server/src/db/backend.rs` (both adapters), `server/
crates/waddle-server/src/db/mod.rs` (or wherever `DatabaseConfig` lives — control-plane
pool field + accessor, plus a `pool_size`/`control_plane_pool.size >= 1` guard so a
misconfigured zero fails construction immediately instead of hanging on sqlx's
`acquire_timeout`), `server/crates/waddle-server/src/config.rs` (Helm-surfaced
`pool_size` + control-plane pool size env vars, same `from_env`/typed-error pattern),
`server/crates/waddle-server/src/clustering/lease.rs` (heartbeat moves to the control-plane
pool — a small, behavior-preserving edit).

**Gating (blocker fix — corrects an eager-provisioning contradiction in the original
draft)**: the control-plane pool is opened only when the configured driver is Postgres
**AND** `clustering.enabled` is true, wired at the `main.rs` call site that builds
`DatabaseConfig` from `DatabaseRuntimeConfig` + `ServerConfig`. Every non-clustered
Postgres deployment (the common case today) has no code path that issues a
control-plane statement, so provisioning the pool anyway would only hold idle
connections against the database and expose server startup to a transient failure on
a second connect-validate — for a feature that specific deployment never requested.

**Tests**: unit tests for config parsing/defaults (mirrors existing `ClusteringConfig`
test style); a Postgres-gated test asserting the control-plane pool is a distinct
`PgPool` from the main pool (different `max_connections`, observable via
`pool.size()`); no new harness scenario (this slice is plumbing, proven by later
slices actually using the pool).

**Dependencies**: none (first slice). Everything else in this phase depends on it.

---

### Slice 1 — `nodes`/`claims` schema + `ClaimStore` trait + core CAS (D1, part 1)

**Locked spec — table shapes** (scoping map, drawn from ADR element 4; exact CREATE
TABLE DDL is not given verbatim by the ADR, so the DDL below is **PROPOSED**, following
the `clustering_` prefix convention already established by
`clustering_keypair_slots`/`clustering_peer_allowlist`):

```sql
-- PROPOSED
CREATE TABLE IF NOT EXISTS clustering_nodes (
    node_id           TEXT PRIMARY KEY,
    node_epoch        TEXT NOT NULL,
    heartbeat         TIMESTAMPTZ NOT NULL DEFAULT now(),
    expired           BOOLEAN NOT NULL DEFAULT FALSE,
    pod_template_hash TEXT
);

-- PROPOSED — `entity` stores `entity_key(entity)`'s `<entity_type_tag>:<id>`
-- encoding (deviation 16), not the bare id: the type must be folded into the
-- key itself so two different-typed entities sharing an id never collide.
CREATE TABLE IF NOT EXISTS clustering_claims (
    entity      TEXT PRIMARY KEY,
    entity_type TEXT NOT NULL,   -- 'user_actor' | 'room_actor' | 'sm_session'
    node_id     TEXT NOT NULL,
    node_epoch  TEXT NOT NULL,
    claim_epoch BIGINT NOT NULL DEFAULT 0
);

-- PROPOSED — backs the demotion-reconciliation query (element 4/Slice 2), the
-- dead-node orphan-reaper scan (element 9/Slice 5), and general per-node claim
-- listing; the ADR promises this query is indexed, not a sequential scan.
CREATE INDEX IF NOT EXISTS clustering_claims_node_id_node_epoch
    ON clustering_claims (node_id, node_epoch);
```

**Locked spec — CAS/fencing SQL contract** (element 4, quoted verbatim from the ADR;
these shapes are NOT to be altered):

- *Acquire*: `INSERT ... ON CONFLICT (entity) DO NOTHING` + `rows_affected == 1` check.
- *Expire*:
  ```sql
  UPDATE nodes SET expired = true WHERE node_id=$owner AND node_epoch=$theirs
    AND NOT expired AND heartbeat < now() - $lease_ttl
  ```
- *Steal (stale owner)*:
  ```sql
  UPDATE claims SET node_id=$me, node_epoch=$my_node_epoch,
    claim_epoch = claim_epoch + 1
    WHERE entity=$e AND claim_epoch=$observed
    AND <owner-stale LEFT-JOIN predicate over nodes>
  ```
  with `rows_affected == 1` required, and the LEFT JOIN predicate reading only the
  **committed** `expired` flag (`nodes.node_id IS NULL OR nodes.expired OR node_epoch
  mismatch`) — never a raw `heartbeat < now() - ttl` comparison.

  **Implementation note (minor fix)**: the ADR's "LEFT JOIN" phrasing describes the
  *predicate's logic*, not copy-pasteable SQL — Postgres `UPDATE` has no `LEFT JOIN`
  clause, and `UPDATE ... FROM` is inner-join semantics, which would silently drop the
  "owner row missing entirely" case the predicate exists to cover. The realized SQL is
  a `NOT EXISTS`/`EXISTS` correlated subquery pair over `clustering_nodes`
  (`NOT EXISTS (SELECT 1 FROM clustering_nodes n WHERE n.node_id = claims.node_id AND
  NOT n.expired AND n.node_epoch = claims.node_epoch)`, equivalently `OR EXISTS (...
  expired ...) OR NOT EXISTS (... matching node row at all ...)`), preserving the exact
  three-way disjunction (`node_id IS NULL OR expired OR node_epoch mismatch`) the ADR
  specifies.
- *Steal (consent/epoch-only — SM resume paths exclusively)*:
  ```sql
  UPDATE claims SET node_id=$me, node_epoch=$mine, claim_epoch = claim_epoch + 1
    WHERE entity=$e AND claim_epoch=$observed
  ```
  **Compiler-enforced, not conventional**: the `ClaimStore` trait exposes this variant
  only as `steal_for_resume(entity, observed_epoch, witness: ResumeIdentityProof)`,
  where `ResumeIdentityProof` is constructable only inside the resume module (private
  field, minted after the SASL-identity↔snapshot bare-JID check — Slice 6). General
  stale-owner takeover is the separate `steal_stale(entity, observed_epoch, staleness:
  StalePredicate)` method, which cannot express the no-staleness variant. No caller
  outside the resume path can even name the consent CAS.
- *Heartbeat*:
  ```sql
  UPDATE nodes SET heartbeat = now() WHERE node_id=$me AND node_epoch=$mine
    AND NOT expired AND heartbeat >= now() - $lease_ttl
  ```
  `rows_affected == 0` ⇒ fencing loss ⇒ immediate demotion of all local claims.
- *Fencing transaction* (used by every durable write on behalf of a claimed entity —
  Slices 4/5/7/8 all build on this exact shape):
  ```sql
  BEGIN;
  SELECT 1 FROM claims
    WHERE entity=$e AND node_id=$me AND claim_epoch=$mine
    FOR SHARE;
  -- writes only if the SELECT returned a row
  COMMIT;
  ```
  Banned (per the ADR, enforced by code review / no alternate helper being offered):
  (a) a bare lockless join read of `claims`; (b) fencing columns denormalized onto
  written tables.

**`ClaimStore` trait** (new, `waddle-xmpp/src/ownership/mod.rs`):

```rust
pub enum EntityType { UserActor, RoomActor, SmSession }
pub struct Entity { pub entity_type: EntityType, pub id: String } // typed, never a bare String at call sites
pub struct ClaimEpoch(pub i64);

/// Closed enum, not a free-form predicate builder: the two ADR-specified WHERE
/// shapes are the only staleness sources `steal_stale` accepts, so a caller cannot
/// invent a third staleness definition at a call site.
pub enum StalePredicate {
    /// The owner-stale LEFT-JOIN-equivalent predicate (element 4's `nodes.node_id
    /// IS NULL OR nodes.expired OR node_epoch mismatch`), realized as the
    /// NOT EXISTS/EXISTS subquery pair above.
    OwnerStale,
    /// The steal-intent predicate (Slice 3): `EXISTS (SELECT 1 FROM steal_intents
    /// WHERE entity=$e AND created_at < now() - $intent_ttl)`.
    StealIntentExpired { intent_ttl: std::time::Duration },
}

/// Constructable only by `ownership::resume::verify_resume_identity` (see below) —
/// the field is private to the `ownership` module, not merely to this file, so no
/// code in either crate can mint one except by passing the real SASL-identity ↔
/// snapshot-owner pair through that check (blocker fix; Slice 6 detail).
pub struct ResumeIdentityProof { _private: () }

#[async_trait]
pub trait ClaimStore: Send + Sync {
    async fn ensure_schema(&self) -> Result<(), ClaimError>;
    async fn acquire(&self, entity: &Entity, me: &NodeIdentity) -> Result<ClaimEpoch, ClaimError>;
    async fn steal_stale(&self, entity: &Entity, observed: ClaimEpoch, staleness: StalePredicate, me: &NodeIdentity) -> Result<ClaimEpoch, ClaimError>;
    async fn steal_for_resume(&self, entity: &Entity, observed: ClaimEpoch, witness: ResumeIdentityProof, me: &NodeIdentity) -> Result<ClaimEpoch, ClaimError>;
    async fn fence(&self, entity: &Entity, me: &NodeIdentity, mine: ClaimEpoch) -> Result<bool, ClaimError>; // advisory-only, own transaction — see Slice 4's design note; NEVER the write-path fencing mechanism
    async fn release(&self, entity: &Entity, me: &NodeIdentity, mine: ClaimEpoch) -> Result<(), ClaimError>;
    async fn release_many(&self, entities: &[Entity], me: &NodeIdentity) -> Result<(), ClaimError>; // batched release for Slice 10's drain (~18k modeled claims) — one round-trip, not one-at-a-time
}
```

(Node heartbeat/expire/demotion-reconciliation are a **separate** `NodeLeaseStore`-style
concern, not on `ClaimStore` itself — see Slice 2 — because they operate on `nodes`
rows per-node, not per-entity, matching the ADR's own "heartbeats are per node, not per
entity" framing.)

**`fence` is advisory-only, never the write-path mechanism (blocker fix — see Slice 4's
design note for the full crate-boundary reasoning)**: the standalone `fence()` method
opens its own transaction on its own connection and answers "do I still hold this claim
right now," useful for a caller like a health-ask handler that wants a point-in-time
answer with no write attached. It is never called from inside a fenced write's own
transaction, because doing so would take the `FOR SHARE` lock on a *different*
connection than the write — exactly the bug this whole blocker fix closes. Every
fenced write (Slices 4, 7, 8) issues its own inline `SELECT ... FOR SHARE` on its own
`Database::begin()` transaction instead of calling `fence()`.

**`ResumeIdentityProof`'s minting mechanism moves to a dedicated submodule, detailed in
Slice 6** — the type is declared here (so `ClaimStore`'s signature can name it), but no
constructor lives in `ownership/mod.rs` itself; see Slice 6 for
`ownership::resume::verify_resume_identity`.

**Files**: `server/crates/waddle-xmpp/src/ownership/mod.rs` (trait + types, new
module), `server/crates/waddle-xmpp/src/ownership/in_process.rs` (`InProcessClaimStore`
— single-node semantics identical in contract to the Postgres impl: acquire succeeds
iff the entity is unclaimed, same-node contention enforced, not idempotent-Ok'd),
`server/crates/
waddle-xmpp/src/ownership/resume.rs` (new — `verify_resume_identity`, the sole
constructor of `ResumeIdentityProof`; stubbed here, fleshed out in Slice 6 once the
resume call sites exist), `server/crates/waddle-server/src/clustering/claims.rs` (new,
`#[cfg(feature = "clustering")]`, `PostgresClaimStore` — schema +
Acquire/Steal-stale/Steal-consent + advisory `fence`/`release`/`release_many`, running
on the Slice 0 control-plane pool), `server/crates/waddle-server/src/
clustering/mod.rs` (`pub mod claims;`), `server/crates/waddle-xmpp/src/
stream_management/session_registry/claims.rs` + `core.rs` (retrofit onto `Arc<dyn
ClaimStore>`, per Q2). `clustering/metrics.rs` gains no new instruments this slice —
the claims-table point-read and `NotOwner` NACK sent/received counters land with
their first *production* callers in later slices (`fence` has zero production
callers this slice; see Slice 0's note).

**Tests**: Postgres-gated integration tests (mirroring `lease.rs`'s test style
exactly): acquire/steal/heartbeat/fencing races; **the interleaving race explicitly
named by the ADR** — a steal commit interleaved inside a fenced multi-statement
transaction (the cross-node resurrection/double-promotion case lockless join fencing
fails); steal-from-vanished-node (missing `nodes` row); cross-entity-type
non-collision (a `UserActor` and a `RoomActor` sharing the same id must be two
distinct claims — the `entity` primary key encodes `entity_type` into the key
itself, not just the write-only column). Unit tests: `steal_for_resume` is
unreachable outside a module holding a `ResumeIdentityProof` (a compile-fail /
type-level test, not a runtime assertion — matches the ADR's "unrepresentable rather
than merely forbidden" framing); `entity_key` injectivity across entity types and
ids containing `:` (including ids equal to one of the tag strings themselves).

**Dependencies**: Slice 0 (control-plane pool).

---

### Slice 2 — Demotion reconciliation + self-fencing + readiness + hysteresis (D1, part 2)

**Locked spec** (element 4, quoted verbatim — the previous draft paraphrased this as a
partial quote; corrected here per minor fix 15): the per-heartbeat-interval
reconciliation query runs *"over `claims WHERE node_id = $me AND node_epoch = $mine`,
diffs the result against its local owned-entity set, and demotes/tombstones anything it
no longer owns"*. Self-fencing on
heartbeat CAS returning 0 rows or Postgres unreachable for N intervals: stop serving
claimed entities **before** the lease becomes stealable, and flip the client-facing HTTP
readiness probe to not-ready, cleared only on successful re-registration under a fresh
`node_id`/`node_epoch` plus claim re-acquisition. Isolation rule: refuse to renew only
when `clustering_nodes` shows **two or more** other live nodes and this node reaches
**none** of them over the swarm for M consecutive intervals (N=2 lone-survivor carve
-out: swarm unreachability alone never fences with exactly one other live node).
Re-registration hysteresis: re-acquire claims only after observing swarm reachability
to at least one live peer whenever other live rows exist, exponential backoff on
re-registration.

**`NodeLeaseStore` sketch (major fix 9 — the second store this phase actually needs,
specified rather than left as a name-only forward reference from Slice 1)**: operates
on `clustering_nodes` rows per-node (never per-entity, per the ADR's own "heartbeats
are per node, not per entity" framing), sibling to `ClaimStore` but not part of it:

```rust
#[async_trait]
pub trait NodeLeaseStore: Send + Sync {
    async fn register(&self, me: &NodeIdentity, pod_template_hash: Option<String>) -> Result<(), ClaimError>;
    async fn heartbeat(&self, me: &NodeIdentity) -> Result<bool, ClaimError>; // false ⇒ fencing loss
    async fn expire(&self, owner: &NodeIdentity, lease_ttl: Duration) -> Result<bool, ClaimError>;
    async fn mark_draining(&self, me: &NodeIdentity) -> Result<(), ClaimError>; // Slice 10: stop acquiring new claims, keep serving owned ones
}
```

`register` covers both fresh startup and post-fence re-registration under a new
`node_id`/`node_epoch` (Q7/element 4). The Postgres impl lives alongside
`PostgresClaimStore` in `clustering/claims.rs` (same file, same control-plane pool);
the in-process/single-node arm is trivial (`register`/`heartbeat` always succeed,
`expire` never fires — there is only one node).

**Files**: `server/crates/waddle-server/src/clustering/claims.rs` (reconciliation
query + demote/tombstone), new `server/crates/waddle-server/src/clustering/
self_fence.rs` (isolation detection reusing the swarm's connected-peer gauge from Phase
2 Slice 1, hysteresis state machine), `server/crates/waddle-server/src/server/mod.rs`
(readiness probe wiring — the HTTP health route needs a shared `AtomicBool`/similar
flipped by this module), `server/crates/waddle-server/src/clustering/metrics.rs`
(heartbeat-age gauge, heartbeat-write-latency histogram + alert threshold).

**Tests**: Postgres-gated: reconciliation demotes an entity whose claim was stolen out
from under a live local actor; **the renewal-vs-expire interleaving** (renewal
evaluated pre-expiry, committed post-steal, must return 0 rows — the expired-flag
ordering point; moved here from Slice 1 since it exercises `NodeLeaseStore`'s
heartbeat/expire CAS, which does not exist until this slice); **lapsed-lease
heartbeat CAS** (paused node observes fencing loss on wake; moved here for the same
reason). Harness (`clustering_cluster_e2e.rs`): the two
previously-`#[ignore]`d scaffolds become fillable **here** at the fencing-primitive
level (full activation is Slice 11, once Slice 5's durable-queue path also exists for
`partial_partition_degrades_without_fencing`); `lone_survivor_and_isolation_fencing`
can be filled and un-ignored in this slice since it only needs heartbeat fencing, not
the durable queue.

**Dependencies**: Slice 1.

---

### Slice 3 — `steal_intents` unwedge/owner-veto path (D1, part 3)

**Locked spec — table** (ADR verbatim): *"`steal_intents (entity, reporter_node,
created_at DEFAULT now())`... `UNIQUE (entity, reporter_node)` with `ON CONFLICT
(entity, reporter_node) DO UPDATE SET created_at = EXCLUDED.created_at`... and an index
on `(entity, created_at)`."*

```sql
-- PROPOSED (column shapes locked by the ADR text above; exact DDL syntax is ours)
CREATE TABLE IF NOT EXISTS clustering_steal_intents (
    entity        TEXT NOT NULL,
    reporter_node TEXT NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (entity, reporter_node)
);
CREATE INDEX IF NOT EXISTS clustering_steal_intents_entity_created_at
    ON clustering_steal_intents (entity, created_at);
```

Steal CAS substitutes `EXISTS (SELECT 1 FROM steal_intents WHERE entity=$e AND
created_at < now() - $intent_ttl)` for the owner-stale predicate. Owner's heartbeat
loop reads intents against its own claims, health-asks the owning actor, clears with an
epoch-fenced DELETE on success (unforgeable veto — proven by writing under its own live
epoch). Applies to **both** `RoomActor` and `UserActor` claims; SM-session claims are
never stolen this way (identity-bound resume only, Slice 6).

**Rule (major fix 10 — which CAS variant may touch an SM-session claim, stated once,
binding on Slices 3/5/6 so no implementer wires the wrong one in)**:

1. **`steal_stale` with `StalePredicate::StealIntentExpired` (the steal-intent path,
   this slice) never applies to `sm_session` claims.** Only `UserActor`/`RoomActor`
   claims accumulate `steal_intents` rows or get stolen through them.
2. **`steal_stale` with `StalePredicate::OwnerStale` (plain dead-owner takeover) MAY
   apply to `sm_session` claims, exclusively via the orphan reaper (Slice 5, element 9)**
   for garbage collection, expiry, or Q6 promotion of a dead node's detached sessions —
   never for a *live* session, and never as a substitute for the resume path.
3. **`steal_for_resume` is exclusively the resume path's consent/epoch-only CAS**
   (Slice 6, element 8) and requires a `ResumeIdentityProof`. No other code path —
   including the orphan reaper — may call it, and (per the Slice 1 trait shape) no
   other code path can even name it.

Rule 2 and rule 3 target the same claims (`sm_session`) with different CAS variants for
different reasons (dead-owner GC vs. live identity-bound resume) — implementers must
not conflate them; Slice 6's interleaving test (below) exists specifically to prove
they don't race each other incorrectly when both fire near-simultaneously.

**Abandoned-intent sweep note (minor fix 20)**: the janitor sweep of abandoned intent
rows (reporter died before clearing) is keyed purely by time —
`created_at < now() - k * intent_ttl`, **no entity filter** — which the
`(entity, created_at)` index does not serve; it is a full scan of the
`clustering_steal_intents` table. This is deliberate and named, not an oversight: the
table is small by construction (`UNIQUE (entity, reporter_node)` with refresh-not-
accumulate upserts caps it at one row per reporter×entity pair with an active
grievance, so it tracks current cluster ill-health, not history), and the sweep runs on
the orphan reaper's slow cadence (Slice 5), never on the routing-critical path — the
index exists for the steal CAS's per-entity `EXISTS` probe, which *is* hot-path.
Adding a second index on bare `created_at` for a table expected to hold tens of rows
would be write amplification for nothing.

**Files**: `server/crates/waddle-server/src/clustering/claims.rs` (steal_intents CRUD +
veto path), `server/crates/waddle-xmpp/src/registry/user_actor.rs` (internal health-ask
handler + proactive wedge-kill-and-conflict-close on a failed self health-check —
pre-empts the steal at `intent_ttl`), `server/crates/waddle-xmpp/src/muc/
room_actor.rs` (same health-ask handler for rooms).

**Tests**: Postgres-gated: steal-intent veto vs. expiry. Harness: the
**deposed-owner-with-live-socket case** (wedged `UserActor`, steal at `intent_ttl`,
reconciliation conflict-closes the socket within one heartbeat interval).

**Dependencies**: Slice 2 (reconciliation is the "guarantee" backstop this path's fast
path complements).

---

### Slice 4 — Postgres-fenced `SmPersistenceStorage` (D2)

**Locked spec** (element 1, quoted verbatim — this is the phase's most explicit
"do-not-improvise" text): *"Cluster mode selects a Postgres-only fenced implementation
of `SmPersistenceStorage` — a full second implementation of the trait built on
`Database::begin`, not a decorator: the portable impl acquires a pooled connection per
statement (`ConnectionGuard`), so no wrapper can place the `FOR SHARE` fencing lock in
the same transaction as the inner impl's writes, and multi-statement methods
(`delete_session`, `store_session_atomic`, `record_promotion_failure`) each need the
fencing SELECT inside one `Transaction`. Two trait-shape divergences from the portable
impl are explicit and accepted: (a) detach writes ignore the caller-supplied
`detached_at` and stamp/re-read Postgres `now()`... (b) expiry listing evaluates the
window in SQL against Postgres `now()`, treating the trait's `now` parameter as
advisory in the fenced impl. The portable impl and schema remain byte-identical for
SQLite."*

**Code research finding, confirming the ADR's premise**: the portable
`DatabaseSmPersistence` (`server/crates/waddle-server/src/sm_persistence.rs`) uses
`Database::guard()` (pooled `ConnectionGuard`, one connection per statement) for every
trait method **except** `store_session_atomic`, which already overrides the trait's
default with a real `Database::begin()` transaction
(`sm_persistence/atomic_store.rs::store_session_atomic`, delete + upsert + N appends,
committed once). So `store_session_atomic` only needs the fencing SELECT *added* to its
existing transaction; `delete_session` and `record_promotion_failure` need to move from
`guard()`-per-statement to a `begin()`-wrapped fencing transaction from scratch.

**Design decision not explicit in the ADR text, resolved against the real crate
boundary — this supersedes the trait-delegation sketch in the scoping map (blocker
fix 2, flagged for sign-off)**: the fenced impl needs to know, at each write, "what
claim epoch do I currently believe I hold for this SM-ID" to bind `$mine` in the
fencing SELECT, and — per the Slice 0/4/7/8 pool-assignment fix — that SELECT must run
on the *same connection, same transaction* as the write it guards. The scoping map's
original idea was a `ClaimStore` trait method taking a borrowed transaction
(`fence_in(&self, tx: &mut Transaction<'_>, ...)`) so the in-transaction check could be
delegated back to `ClaimStore`. **Checked against the real types, this does not
compile.** `Transaction<'a>` (`server/crates/waddle-server/src/db/backend.rs:443-552`)
is a `waddle-server`-local type wrapping `sqlx::Transaction<'_, Sqlite | Postgres>`
(`TransactionInner`); `ClaimStore` (Q1) lives in `waddle-xmpp`; and
`waddle-server/Cargo.toml` depends on `waddle-xmpp`, never the reverse (verified: no
`waddle-server` dependency appears in `waddle-xmpp/Cargo.toml`). A `ClaimStore` trait
method cannot name `waddle_server::db::backend::Transaction` as a parameter type
without an illegal reverse dependency — so `fence_in` as sketched is not
implementable, and this plan does not pretend otherwise.

**Resolution (real, implementable design)**: `PostgresFencedSmPersistence` issues the
`SELECT ... FOR SHARE` SQL **inline, itself**, on its own `Database::begin()`
transaction — using the exact fencing-transaction SQL shape Slice 1 locks — rather than
calling into `ClaimStore` for the in-transaction check at all. The epoch **value**
bound into that inline SELECT comes from `PostgresFencedSmPersistence`'s own cached
last-acquired `ClaimEpoch`, read once from the `<enable/>`-time `ClaimStore::acquire`
call and held as plain in-process state (a stored value, not a trait call). The
standalone `ClaimStore::fence` method (Slice 1) still exists, but purely as an
**advisory, own-transaction** check for non-write-path callers (e.g. a health-ask
handler); it is never invoked from inside a fenced write's transaction, and no Slice 4
code calls it. This keeps the public `SmPersistenceStorage` trait untouched (per the
ADR's "full second implementation of the trait" language) while giving the impl the
in-transaction context the locked SQL requires — at the cost of duplicating the
fencing-SELECT SQL shape between `PostgresClaimStore`'s own methods and
`PostgresFencedSmPersistence`'s inline usage, which is the accepted tradeoff of the two
types living in the crate that actually owns the transaction.

**Files**: new `server/crates/waddle-server/src/sm_persistence_fenced.rs` (mirrors
`sm_persistence.rs`'s module doc style; sibling file, not a submodule of the portable
impl — "not a decorator" extends to file layout too), implementing `waddle_xmpp::
stream_management::persistence::SmPersistenceStorage` for `PostgresFencedSmPersistence`.
No new schema: same `sm_sessions`/`sm_unacked` tables, byte-identical for SQLite.

**Tests**: dedicated Postgres-gated test module (new, alongside
`sm_persistence/tests.rs`'s existing style) covering: every trait method under the
fenced impl behaves per divergence (a)/(b) above; a steal committed mid-transaction
causes a concurrent fenced write to observe 0 rows from the `FOR SHARE` SELECT and
abort before the write; `delete_session`/`record_promotion_failure` are genuinely
atomic with their fencing check (no window where the SELECT passes but the steal lands
before the subsequent statement).

**Dependencies (corrected — blocker fix 1)**: Slice 1 (`ClaimStore` — supplies the
cached acquire-time `ClaimEpoch` this impl binds into its own inline `FOR SHARE` SELECT;
see the design note above for why the fencing check is issued inline rather than
delegated to `ClaimStore`). Slice 0 is a **transitive** dependency only, through Slice
1's own control-plane-pool CAS statements — the fencing SELECT here runs on the **main
pool**, inside the same `Database::begin()` transaction as the write it guards, exactly
like every other statement `PostgresFencedSmPersistence` issues. The previous draft's
claim that the fencing SELECT itself needs the control-plane pool was the
pool/transaction contradiction this fix resolves: a control-plane-pool connection and a
main-pool connection cannot share one transaction, so the fencing lock and the write it
guards must always be on the same pool — here, the main pool.

---

### Slice 5 — Claim-scoped durable-SM consumers (D3)

**Locked spec** (element 9, quoted verbatim — major fix 6 restores a clause the
previous draft dropped mid-sentence): `restore_from_persistence` *"hydrates only
sessions whose claim this node holds or can acquire at startup (acquire-then-hydrate);
it never performs unscoped full-table hydration, and restore-time expired-row deletion
moves behind the claim (and uses Postgres `now()` against the DB-stamped
`detached_at`, element 4)."* SM-expiry janitor *"sweeps and Q6-promotes only
self-claimed sessions; promotion executes under the row-locked fenced epoch."*
Graceful drain *"releases claims per entity, after that entity's final fenced
writes."* Orphan reaper: *"any node may steal such claims (fenced CAS) and then expire
or promote them, after first committing the expire CAS on the owner's `nodes` row."*

**Design addition (major fix 6)**: restore-time expired-row deletion — today an
unscoped delete of rows past their expiry window, run before/alongside the unscoped
hydration read — becomes claim-scoped exactly like the hydration it accompanies: a row
is only deleted once this node holds (or has just acquired) its claim, and the
staleness window is evaluated in SQL against Postgres `now()` compared to the
DB-stamped `detached_at` column (never a node-local clock), consistent with element 4's
"all time predicates use Postgres `now()`" rule. This closes the same class of bug the
acquire-then-hydrate change closes for reads: an unscoped delete racing a second node's
concurrent claim-scoped hydration of the same row would be a lost-write/phantom-expiry
hazard, not merely a correctness nicety.

**Files**: `server/crates/waddle-xmpp/src/stream_management/session_registry.rs` +
`core.rs` (claim-scoped `restore_from_persistence` — acquire-then-hydrate instead of
unscoped read), `server/crates/waddle-server/src/server/session_janitors.rs`
(`spawn_sm_expiry_janitor`, currently at line 91 — claim-scope its sweep to
`node_id=$me`; add a new `spawn_orphan_reaper_janitor` alongside the existing **eight**
janitor-spawn functions already in that file — corrected count, minor fix 17:
`spawn_sm_expiry_janitor`, `spawn_pending_delivery_claim_janitor`,
`spawn_push_service_publish_job_janitor`, `spawn_notification_outbox_janitor`,
`spawn_graceful_shutdown_drain`, `spawn_auth_state_janitor`,
`spawn_room_dormancy_janitor`, `spawn_user_actor_reaper`), `server/crates/waddle-server/src/
sm_promotion.rs` + `pending.rs` (promotion executes inside the Slice 4 fenced write
path), `server/crates/waddle-server/src/pending_delivery/flush.rs` (claim-scoped
sweep-flush for owned bare JIDs, per element 5's flush-poke/janitor split — this
janitor is the *guaranteed* flush path, the flush poke remains the optimization).

**`pending_delivery` schema additions** (summarized from the ADR Implementation Plan's
Phase 3 section — which states the schema *"is specified in **one place** here to
prevent dropping a column group"* — with element 5 as the underlying rationale):
per-`(origin stream → recipient)`
**sequence number** for sticky-failover ordering gap detection; dedup dimensions
**`recipient + origin_stream_id + inbound_seq`** under a recipient-scoped **UNIQUE**
constraint; the
**original ingress timestamp** for XEP-0203 `<delay/>` stamping on janitor flush.
`sm_unacked` already carries `original_receipt_at_ms`, so only `origin_stream_id`/
`inbound_seq` and the per-pair sequence number are net-new there. These are schema
additions to *existing* tables (`pending_delivery`, `sm_unacked`) — PROPOSED column
names/types, to be finalized against the actual current schema in
`server/crates/waddle-server/src/pending_delivery/` (not independently re-verified in
this research pass; flagged as a slice-1-of-implementation task to confirm exact
current column set before adding).

**Tests**: Postgres-gated: acquire-then-hydrate at startup claims only unclaimed/
self-claimed rows; restore-time expired-row deletion only fires under the claim and
evaluates the window against Postgres `now()`/DB-stamped `detached_at` (major fix 6);
duplicate-promotion (double-janitor) prevention; the **guaranteed-flush janitor test,
scoped to same-node delivery** (coordinator ruling, major fix 4 — the previous draft's
"multi-node janitor flush" case smuggled in Phase-4 cross-node stanza routing, which
this phase's own Non-goals exclude and which the ADR Implementation Plan assigns to
Phase 4; see the Non-goals bullet above and deviation 14). The same-node version:
sender's write lands in `pending_delivery` for a bare JID this node also owns and whose
socket is local, and the janitor sweep delivers it within one sweep interval through the
`UserActor`'s full delivery surface — proving the guaranteed path independent of the
flush-poke optimization, without requiring a routed cross-node relay ask this phase
does not otherwise need. Harness: extend `clustering_cluster_e2e.rs` with a
claim-scoped-hydration scenario (two nodes, shared Postgres, kill one, assert the
survivor's `restore_from_persistence` claims and hydrates only the dead node's orphaned
sessions, not its own already-claimed ones twice).

**Dependencies (corrected — minor fix 18)**: Slice 4 (fenced SM persistence). The
previous draft hard-depended this slice on Slice 3, but the orphan reaper uses
`steal_stale` with `StalePredicate::OwnerStale` directly (Slice 1) — it has no
steal-intents involvement at all (see the rule in Slice 3 above: steal-intents never
apply to `sm_session` claims). Dropped to a soft note: Slice 3 and Slice 5 touch the
same `claims.rs` file and are easiest to review in the order given, but neither
requires the other's code to compile or pass tests.

---

### Slice 6 — Cross-node XEP-0198 resume via claim-steal (D4)

**Locked spec** (summarized from element 8 — minor fix 15: this is a restructured
bullet list, not a verbatim quote, relabeled accordingly; true verbatim spans stay
quoted below): claims row created at `<enable/>` time (entity =
SM-ID). Three branches:

1. **Detached, owned elsewhere**: identity check (bare JID match) **before** any write
   → `steal_for_resume` (consent/epoch-only CAS) → resume. Mismatch → `<failed/>`
   `not-authorized`, claim epoch untouched.
2. **Live, owned elsewhere**: handshake, not a bare write — remote ask to old owner
   (detach-flush snapshot + `<conflict/>` close per XEP-0198 §5/"Resumption"), only on
   ack does the epoch-bump CAS commit. Identity check gates the **destructive close**
   itself, not just the subsequent CAS (defense in depth against a wrong-identity
   `previd` forcing a disconnect before rejection).
3. **Owner unreachable, lease fresh**: **hold** the `<resume/>` response (conformant —
   XEP-0198 mandates no response deadline) and retry the handshake with backoff, capped
   at `min(remaining lease TTL, resume-handshake timeout)`.

**XEP fact-check (this plan's own verification pass against `xeps/xep-0198.xml`,
confirming the ADR's claims)**: `<failed/>` is confirmed to optionally carry an `h`
attribute (XML schema: `<xs:attribute name='h' type='xs:unsignedInt' use='optional'/>`)
"if the server recognizes the 'previd' as an earlier session that has timed out" — this
supports the ADR's owner-unreachable path emitting `<failed/>` with `h` when
appropriate. The "Acks" section (the 4th top-level section, matching an ADR citation of
"§4") contains the exact wording the ADR relies on: *"MUST NOT be withheld for any
condition other than a timeout"* — verified verbatim. The `<conflict/>` stream-error
use is in the "Resumption" section (5th top-level section) as a **SHOULD**, not MUST:
*"If the former stream is resumed and the server still has the stream for the
previously-identified session open at this time, the server SHOULD send a 'conflict'
stream error and close that stream."* One caveat for the plan: XEP-0198's own text and
examples only ever demonstrate `<failed/>` with `feature-not-implemented` (no support)
or `item-not-found` (unrecognized/expired `previd`) as the child condition; the ADR's
choice of `<resource-constraint/>` for the owner-unreachable-past-window case is a valid
generic RFC 6120 condition under the XEP's general "MUST be one of the stanza error
conditions defined in RFC 6120" rule, but is not itself an XEP-0198-demonstrated example
— worth noting in the dedicated test suite's assertions as "our chosen condition," not
"the XEP's named condition," so a future reader doesn't go looking for it in the spec
text.

**`ResumeIdentityProof` construction (blocker fix 3)**: the previous draft had
`ResumeIdentityProof`'s field private to `waddle-xmpp/src/ownership/mod.rs` while
saying it is "minted in `session_registry/` and `waddle-server`" — those are different
modules (and, for `waddle-server`, a different *crate*), so neither could actually
construct a value with a field private to `ownership::mod`; this does not compile.
Corrected design: the identity check itself — not just the proof type — lives in
`ownership::resume` (stubbed in Slice 1, completed here):
`pub fn verify_resume_identity(sasl_identity: &BareJid, snapshot_owner: &BareJid) ->
Option<ResumeIdentityProof>`, returning `Some` only on exact bare-JID match. The
`ResumeIdentityProof` field stays private to the `ownership` module tree (visible to
`resume.rs` as a sibling submodule, invisible everywhere else in either crate), so
`verify_resume_identity` is the **only** function, in either crate, capable of
producing one. `stream_management.rs` (`waddle-server`) and
`session_registry/tombstone.rs`/`trait_impl.rs` (`waddle-xmpp`) do not construct
`ResumeIdentityProof` directly; they call `ownership::resume::verify_resume_identity`
with the locally-SASL-authenticated bare JID and the loaded snapshot's bound bare JID,
and pass the resulting `Option` through to `steal_for_resume` (mismatch ⇒ `None` ⇒
`<failed/>` `not-authorized`, claim untouched, exactly as element 8 requires).

**Files**: `server/routes/websocket/stream_management.rs` (three resume branches; calls
`ownership::resume::verify_resume_identity` per above), `server/crates/waddle-xmpp/src/
ownership/resume.rs` (completed here: `verify_resume_identity`), `server/crates/
waddle-xmpp/src/stream_management/session_registry/tombstone.rs` +
`trait_impl.rs` + `traits.rs` (claim-aware resume outcomes, same identity-check call),
`server/crates/waddle-server/src/clustering/relay.rs` (the live-steal handshake rides
`RelayHandle` — the **first production (non-harness) caller**; this slice also pays
down `RelayHandle`'s cancellation-safety debt, below, rather than carrying it).

**Janitor-vs-resume ordering invariant (major fix 11)**: the orphan reaper (Slice 5)
may `steal_stale` a dead node's detached `sm_session` claim for GC/expiry/promotion at
the same time a client attempts to resume that very session on a third node — Slice 3's
rule above says both are legal uses of the same claim, so this slice states the
ordering that keeps them from corrupting each other: **snapshot load happens only
after the `steal_for_resume` CAS has committed, under the newly-won epoch, and is
itself performed inside a fencing read** (a `SELECT ... FOR SHARE` against the
just-won claim row, same shape as every other fenced read in this phase) — never
before the CAS, and never against a snapshot read on a stale epoch. This closes the
interleaving where a reaper wins `steal_stale` on the same entity mid-resume: the
resuming node's `steal_for_resume` CAS observes the epoch the reaper already bumped,
loses (0 rows), and fails cleanly with `<failed/>` `item-not-found` (the reaper has, by
definition, decided the session is dead) rather than resuming a snapshot the reaper is
concurrently deleting/promoting.

**RelayHandle cancellation-safety paydown (major fix 12 — resolves the carried risk
below, does not defer it)**: coordinator ruling is to pay this down **in this slice**,
not push it to Phase 4. `RelayHandle`'s own doc comment
(`server/crates/waddle-server/src/clustering/relay.rs:352-360`) already names the exact
gap: unlike `spawn_supervised` (`:215-239`, `:322`), which races every swarm-command
await against a `CancellationToken` in a `biased` `select!` specifically so an
in-flight `register` doesn't panic against an already-closed command channel during
local shutdown, `RelayHandle` "owns no cancellation token to race against." This slice
adds one: `RelayHandle::new` gains a `stop_token: CancellationToken` constructor
parameter (threading the same clustering-scope token `spawn_supervised` already
receives), and every `RelayHandle` method that awaits a swarm-command reply
(`resolve`/`ping`/`crash`/`sleep`/`echo_stanza`, and the new live-steal handshake ask)
races that await against the token in a `biased` `select!`, exactly mirroring
`spawn_supervised`'s pattern — cancellation wins the race so an ask in flight during
local shutdown returns a typed cancellation outcome instead of panicking against a torn
-down swarm. Because the token is a constructor parameter, Slice 7's MUC Demote ask
-caller gets the same protection for free once it passes the same clustering-scope
token when constructing its `RelayHandle`. The "coordinator can decide whether to pay
this down in Slice 6/7 or accept it through Phase 3" hedge in the carried-risks section
is removed accordingly (see below).

**Held-response window vs. client-side retry (minor fix 22)**: the owner-unreachable
branch (3, above) holds the `<resume/>` response while retrying the handshake. A client
that gives up waiting may open a **second** connection and send a **second**
`<resume/>` for the same `previd` while the first is still held. This is not a new
hazard: the epoch CAS resolves it exactly as the two-simultaneous-live-resume race
does — whichever `steal_for_resume`/handshake completes first wins the epoch bump, the
other observes a stale epoch and fails cleanly. The accepted cost is connection churn
(the client now has two connection attempts in flight, one of which will be told
`<failed/>` or successfully close), which is logged, not specially suppressed —
introducing suppression logic to detect "the same client is retrying while held" would
add a second correctness-bearing mechanism next to the epoch CAS for no safety benefit
the CAS doesn't already provide.

**Tests (dedicated Rust test suite, per the XEP hard rule, same PR as the behavior)**: a
two-registry (two-node-simulating) XEP-0198 suite covering: h-counter integrity across
steal; `<conflict/>` close of the old stream; deferred-h/handoff coupling (`<r/>`
answered immediately with `h` excluding unresolved handoffs); dedup under at-least-once
retry including the resume-retransmit race and the recipient-claim-move retry; the
**two-simultaneous-live-resume race** (owner acks the first handshake, second requester
loses the consent epoch CAS, falls back to detached path); the **pure two-node
detached-steal race** (major fix 11 — both nodes race `steal_for_resume` against the
same detached claim with no live owner involved; the loser observes 0 rows and falls
back per the detached-path branch rules, never retrying the same CAS blind); the
**reaper-wins-mid-resume interleaving** (major fix 11 — the orphan reaper's
`steal_stale` commits while a resume is in flight for the same claim; the resume's
`steal_for_resume` CAS must observe the bumped epoch and fail cleanly, never load a
snapshot the reaper is concurrently touching); the
**forged-previd-wrong-identity case** (returns `not-authorized` without stealing, via
`verify_resume_identity` returning `None`). Harness: cross-node live-steal handshake
end-to-end (two real processes, shared Postgres).

**Dependencies**: Slice 5 (claim-scoped SM consumers must exist before resume can
steal a claim that a janitor might simultaneously be reaping), Slice 1 (`steal_for_resume`).

---

### Slice 7 — Durable MUC room ownership + re-election (D5, parallel-after-Slice-1)

**Locked spec** (summarized from element 7, with one true verbatim span quoted — minor
fix 15): new owner *"restores configuration, affiliations,
and subject from Postgres before accepting any join."* Two-part demotion: best-effort
acked `Demote { entity, new_epoch }`; guaranteed backstop = the same fencing SELECT
element 4 prescribes, run before every local fan-out (MAM insert doubles as the
backstop when archiving is on). Phase 4 GA gates (not this phase) assert outcast
denial + password/members-only hold after steal — this phase builds the mechanism, GA
gates its correctness.

**Code research finding, corrected (minor fix 16)**: the previous draft attributed the
"no room/affiliation table exists in any schema" reasoning to
`session_janitors.rs`'s `spawn_room_dormancy_janitor` doc comment (lines ~1142–1155).
That attribution is wrong: the doc comment there (confirmed by re-reading it) discusses
a *different* claim entirely — that eviction of fully-dormant in-memory rooms is safe
because `GetOrCreateRoom` re-entry spawns a fresh `RoomActor` with state identical to
`MucRoom::is_dormant`'s definition of dormant, i.e. it is about **in-memory
dormancy-eviction safety**, not about the absence of a durable room/affiliation table.
The "no room/affiliation table exists in any schema" phrasing is this plan's **own
synthesis** from the ADR's element-7 text (which itself cites `muc/room.rs`'s
`is_dormant()` for a related but distinct point about in-memory-only room state) — not
a quote or paraphrase of any code comment. Corrected here to avoid miscrediting
`session_janitors.rs` with an argument it doesn't make. Also note:
`room_actor.rs`'s own doc comment describes it as "part of the Phase 3 actor-model
migration," referring to a **prior, unrelated** actor-migration phase (predates
ADR-0017's phase numbering) — disambiguated here to avoid confusing that with
ADR-0017 Phase 3.

**Files**: `server/crates/waddle-xmpp/src/muc/room.rs` (occupant roster + config +
affiliation + subject durability hooks), `room_actor.rs` +
`room_actor/{admin_handlers.rs,occupancy_handlers.rs,snapshot_handlers.rs}` (restore
-before-accepting-joins on claim acquisition; fenced pre-fan-out SELECT), `room_
affiliations.rs` + `affiliation/{mod.rs,config.rs,list.rs,resolver.rs}` (durable
affiliation-list read/write), `room_registry.rs` / `room_registry_actor.rs` /
`room_registry_handle.rs` (claim acquisition on `GetOrCreateRoom`), `subject.rs`
(durable subject read on restore).

**Tests**: Postgres-gated: fenced pre-fan-out SELECT returns 0 rows immediately after a
steal commits (deposed owner's very next broadcast is blocked). Harness: room ownership
steal restores config/affiliations/subject before the new owner accepts a join (this
phase's mechanism test; the Phase 4 GA gate re-runs it as outcast-denial-after-steal
once GA-gating lands).

**Pool assignment (blocker fix 1, stated explicitly per Slice 0)**: the fenced
pre-fan-out `SELECT ... FOR SHARE` runs inline, inside the same `Database::begin()`
transaction as the broadcast's write (the MAM archive insert when archiving is on;
otherwise the standalone autocommit fencing SELECT itself is the one write-adjacent
statement), on the **main pool** — never the control-plane pool, for the same
same-connection-same-transaction reason Slice 4 states. Room/occupant/affiliation
durable writes (roster rows, config, affiliation lists, subject) are likewise main-pool
statements; only `RoomActor`'s own claim acquire/steal/heartbeat traffic (via
`ClaimStore`, Slice 1) rides the control-plane pool.

**Dependencies**: Slice 1 only (`ClaimStore` for `RoomActor` entities) — deliberately
**not** dependent on Slices 4–6, so it can be worked in parallel by a separate
contributor/session, per the scoping map's "D5 parallel-after-D1" ordering.

---

### Slice 8 — XEP-0397 ISR cluster-correct (D6)

**Locked spec** (element 10, quoted verbatim — major fix 7 restores a clause the
previous draft elided): token consume *"fetches the token row by the
non-secret key (the SM-ID/claim), compares the stored token against the presented token
in Rust with a constant-time primitive (`subtle`/`constant_time_eq`), and only then
performs the delete — all inside one epoch-fenced, `FOR SHARE`-locked transaction
preserving single-use atomicity, bound to the same authenticated-identity check as
resume (element 8)."* Matching the token in a SQL `WHERE` clause... is explicitly
banned. Two failure paths
per the XEP: authenticated-but-resume-impossible → `<success/>` containing
`<inst-resume-failed/>` wrapping XEP-0198 `<failed/>`; SM-ID valid but token auth failed
→ `<failure/>` **and** epoch-fenced delete of the session state the SM-ID identified.

**XEP fact-check (this plan's own verification, material findings for the
implementation)**:
- **XEP-0397's status is `Deferred`**, not Draft/Active — the ADR and epic still commit
  to implementing it, but this plan flags the spec's own instability as a reason the
  dedicated test suite (not the wire format) is the durable source of truth for our
  conformance claims.
- **Correct namespace is `https://xmpp.org/extensions/isr/0`** — the vendored XEP-0397
  source itself contains a literal typo (`htpps://...`) in two places; the Registrar
  Considerations section confirms the `https://` spelling is canonical. This plan uses
  `https://xmpp.org/extensions/isr/0` and treats the `htpps` occurrences as a spec-file
  typo, not an alternate valid form.
- **Namespace version mismatch between the two vendored XEPs**: XEP-0397's own examples
  wrap everything in `urn:xmpp:sasl:1`, while the vendored XEP-0388 in this repo uses
  `urn:xmpp:sasl:2` throughout. XEP-0397's revision history explains this (it targeted
  an older SASL2 namespace pre-dating a rename). **This plan uses `urn:xmpp:sasl:2`**
  (the vendored, current XEP-0388's namespace) for all SASL2 envelope elements
  (`<authenticate/>`, `<success/>`, `<failure/>`, `<continue/>`), since that is the
  namespace this codebase's SASL2 support (wherever it lives) already targets — this is
  called out explicitly as a deviation from XEP-0397's own examples, in favor of
  consistency with the vendored XEP-0388.
- **Single-use + rotation confirmed**: XEP-0397 §"Successful Stream Resumption" requires
  the server's success reply to include a **new** ISR token (rotation on every use), and
  §"Performing Instant Stream Resumption" requires token destruction on both success
  *and* a failed-token attempt against a valid SM-ID.
- **XEP-0388 mechanically explains why the failure path carries no ISR wrapper**: per
  XEP-0388's own `<failure/>` rule ("The server MUST NOT process any inline features
  requested by the client in a failed authentication request"), a failed authentication
  cannot carry any processed inline-feature result — so ISR's failed-token path is
  correctly a bare `<failure/>` with a standard SASL condition child, not a
  `<inst-resume-failed/>`-wrapped anything. This corroborates the ADR's element-10 text
  exactly.

**`clustering_isr_tokens` schema** (PROPOSED — no ADR-locked DDL exists for this table):

```sql
-- PROPOSED
CREATE TABLE IF NOT EXISTS clustering_isr_tokens (
    sm_id      TEXT PRIMARY KEY,   -- same non-secret key as the sm_session claim entity
    token      TEXT NOT NULL,      -- compared in Rust with subtle::ConstantTimeEq, never in SQL WHERE
    mechanism  TEXT NOT NULL,      -- the pinned SASL mechanism from <isr-enable mechanism=".."/>
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

Consume flow: the caller first runs the **same authenticated-identity check as resume**
(major fix 7 — reusing `ownership::resume::verify_resume_identity`, Slice 6, against
the locally SASL-authenticated bare JID and the SM session's bound bare JID); on
mismatch the flow never reaches the token compare at all and returns `not-authorized`,
mirroring element 8's "identity check before any write" rule exactly. On match:
`SELECT token FROM clustering_isr_tokens WHERE sm_id=$1` (point read by non-secret key,
never by token) **inline, on the main pool** (blocker fix 1 — this is a fenced write
transaction, same pool-assignment rule as Slices 4/7) inside the same `Database::begin()`
transaction as the `sm_session` claim's own `SELECT ... FOR SHARE` fencing check →
Rust-side `subtle::ConstantTimeEq` compare → on match, `DELETE ... WHERE sm_id=$1`
**and** the rotation `INSERT` of the next resumption's new token row, **both in that
same one transaction** (minor fix 21 — delete-then-insert is not two operations that
happen to be adjacent, it is one atomic unit; the reply sent to the client is
constructed only from the post-`COMMIT` values, never from pre-commit in-memory state,
so a rollback can never leave a reply describing a token that was never durably
rotated); on mismatch, `DELETE ... WHERE sm_id=$1` unconditionally in the same fenced
transaction (destroy on failed-token attempt, per the XEP's anti-brute-force MUST) and
the caller returns the bare SASL2 `<failure/>` after that commit.

**IQ-issuance retirement inventory (major fix 5 — the non-conformant live path this
slice deletes, per the XEP-conformance hard rule and the corrected Q8 finding)**:

- `server/crates/waddle-server/src/server/routes/websocket/handlers/iq/isr_token.rs` —
  **delete** the module (`handle_isr_token_request_iq`, the live `create_token` caller).
- `server/crates/waddle-server/src/server/routes/websocket/handlers/iq/mod.rs` —
  **delete** the dispatch: the `mod isr_token;` declaration (`:92`), the
  `handle_isr_token_request_iq` import (`:140`), the `is_isr_token_request` match and
  call (`:302-303`), and the now-unused
  `isr::{build_isr_token_error, build_isr_token_result, is_isr_token_request, ...}`
  imports (`:15`) to keep clippy/knip-equivalent dead-code checks clean.
- `server/crates/waddle-server/tests/xep0054_0049_0191_ws.rs` — **delete**
  `websocket_isr_token_request_returns_token` (`:660-678`), the e2e test that pins the
  non-conformant IQ shape.
- The `urn:xmpp:isr:0` IQ builders in `waddle-xmpp`'s `isr` module
  (`build_isr_token_error`/`build_isr_token_result`/`is_isr_token_request`) — delete or
  repurpose; nothing may keep constructing the standalone-IQ shape.

Replacement: token issuance moves to the XEP-0397-conformant inline
`<isr-enable/>` → `<isr-enabled/>` exchange riding `<enable/>`/`<enabled/>` (and
rotation riding the SASL2 `<success/>`), per the locked spec and XEP fact-check above.

**Files**: the retirement inventory above, plus: `server/crates/waddle-xmpp/src/isr/
store.rs` (extract `IsrTokenStore` trait + rename current struct to
`InMemoryIsrTokenStore`, mirroring the `ClaimStore` split — flagged as a structural
refactor, not new-code-only), new `server/crates/
waddle-server/src/clustering/isr.rs` (`#[cfg(feature = "clustering")]`,
`PostgresIsrTokenStore`; its fenced consume transaction runs on the **main pool** per
the Slice 0 rule — see the consume flow above), the SASL2/ISR wire-handling call site
(exact file not identified by this research pass — likely alongside whatever module
currently handles the `<enable/>`/SASL2 inline-feature negotiation; **flagged as an
open item to locate during Slice 8 implementation**, not a blocking gap in this plan).

**Tests (dedicated Rust test suite, per the XEP hard rule, same PR)**: constant-time
comparison is actually used (a targeted test asserting no early-exit branching on byte
mismatch position — as much as can be asserted without timing side-channel
infrastructure, at minimum asserting `subtle::ConstantTimeEq` is the comparison
primitive used, not `==`); **identity binding (major fix 7)** — a consume attempt whose
locally SASL-authenticated bare JID does not match the SM session's bound bare JID is
rejected via the same `ownership::resume::verify_resume_identity` check as resume,
before the token compare runs and without destroying the token or session state (the
anti-brute-force destroy applies to failed *token* auth on a valid SM-ID, not to a
wrong-identity caller who never reaches the token compare); both XEP-0397 failure
cases end-to-end
(authenticated-but-resume-impossible → `<success/>`+`<inst-resume-failed/>`+`<failed/>`,
continues session establishment; failed token auth → bare `<failure/>`, asserts the
detached session state and claim are destroyed); token rotation on success (a second
consume with the old token fails; delete+insert commit atomically — a crash injected
between them must roll both back, never leaving a deleted-but-unrotated state, per the
one-transaction rule above); the IQ path is gone (a `token-request` IQ now yields
`service-unavailable`, not a token). ISR advertisement itself: a test asserting disco/
stream-features omit `<isr/>` unless `clustering.enabled && Postgres`.

**Dependencies**: Slice 4 (fenced SM persistence — ISR consume rides the SM claim's
fencing transaction), Slice 6 (resume machinery ISR piggybacks on).

---

### Slice 9 — Retire the DashMap selection surface + Slice-1 liveness filter (D7)

**Settled scope** (Q3): the epic's own checklist. Retires `select_routable_resources_for_user`
(`connection_registry/resources.rs:21`) and `get_resources_for_user` (`:227`), migrating
all ~14 confirmed call sites (enumerated in the scoping map: `routing.rs` ×2,
`admin/channels.rs`, `presence/subscription/directed.rs`, `message/
group_dm_invite.rs`, `pubsub_fanout.rs` ×2, `presence/probe.rs` ×3, `presence/
subscription/delivery.rs`, `interpret/offline_delivery.rs`, `interpret/
groupchat_archive.rs`, `sm_promotion/pending.rs`, `sm_promotion/live.rs` ×2 — the
`live.rs:72,76` call sites confirmed by code research to be
`collect_live_targets`'s two `select_routable_resources_for_user` calls inside the
`LiveDecision::DeliverToFull`/`DeliverToBareWithFanout` arms) onto the actor's
`SelectRoutableResources`, and retires the transitional Slice-1 DashMap-liveness
intersection filter (`select_bare_jid_live_targets`'s `filter_deliverable`), switching
to self-healing via `TrySendPeer` → `DroppedClosed` eviction exactly as the Phase 1
completion note's "later slice" describes. The headless-persistence gate (`
run_headless_recipient_pass` on an all-`DroppedClosed` bare-JID delivery) ships
alongside the filter retirement, per that note's own dependency ("only needed once the
liveness filter is retired").

**Files**: `waddle-xmpp/src/registry/connection_registry.rs` (or wherever
`select_routable_resources_for_user`/`get_resources_for_user` are defined — deletion),
all ~14 call sites above (migrate to actor `SelectRoutableResources`/`GetResources`
asks), `waddle-xmpp/src/routing.rs` (bare-JID else-branch: add the headless-pass
trigger), the empty-actor reaper interaction (already landed in Phase 1 — no change
needed, just re-verify it still holds once the filter's stale-extra protection is gone).

**Tests**: the four Phase-1-completion-note unit tests this retirement was deferred
behind (actor set complete synchronously; ownership-gated unregister; actor-register
failure rollback; reaper prunes only truly-empty actors) get a fifth: bare-JID delivery
to a sole stale extra now self-heals via `DroppedClosed` eviction instead of relying on
the filter, with no message loss (the headless pass fires when no live target
delivered). Existing e2e suites (`dm_delivery_mam.cue`, `muc_groupchat_fanout.cue`,
`multi_device_carbons.cue`, `xep_0198_stream_management.cue`) must stay green unchanged.

**Dependencies**: Slice 5 (D3 — the scoping map's "D7 trailing D3" ordering: claim
-scoped SM consumers, some of which are among the 14 callers, e.g. `sm_promotion/
pending.rs` and `live.rs`, should already be claim-aware before their DashMap-selection
call sites are touched, to avoid re-touching the same lines twice).

---

### Slice 10 — Graceful per-entity drain + drain observability (D9)

**Locked spec** (element 4/Implementation Plan, summarized): per-entity, not
phase-ordered drain — mark draining in `nodes` (stop acquiring **new** claims, keep
serving already-owned draining sessions); for each owned entity, complete final fenced
writes **while still holding the claim**, then release (batched via
`ClaimStore::release_many`, Slice 1 — not one-at-a-time, given ~18k modeled
claims); no global "promote what remains" step after release (a fencing violation by
the ADR's own rules). Observability: drain-duration histogram,
`claims_released_on_drain`/`claims_abandoned_on_drain` counters, alert on nonzero
abandonment. Rollout-aware placement via `pod_template_hash`: the
most-recently-registered-live-node mechanism defined in Q5 above — newest-generation
pods acquire released claims without backoff, old-generation pods back off first;
misclassification only mis-tunes the backoff heuristic, never ownership (the CAS is
authoritative, per Q5).

**Batch-release invariant (major fix 13)**: an entity enters the release batch only
**strictly after its final fenced write's transaction has committed** — never when the
write is merely issued, buffered, or in flight. Batching changes the *shape* of the
release traffic (one `release_many` round-trip instead of ~18k singles), not its
*ordering contract*: releasing a claim whose final write is still uncommitted would
hand the entity to a new owner able to write concurrently with the old owner's
still-open transaction, the exact dual-writer window fencing exists to exclude. The
batch is therefore built incrementally from per-entity commit completions, and
`release_many` is called only over entities whose commits have returned.

**Drain-interrupted relay asks (major fix 12 follow-through)**: with `RelayHandle`
carrying the clustering-scope `CancellationToken` (paid down in Slice 6), a relay ask
in flight when drain fires (e.g. a live-steal handshake or Demote ask racing shutdown)
resolves to the typed cancellation outcome instead of hanging or panicking — and that
cancellation path is **the tested mechanism feeding `claims_abandoned_on_drain`**: an
entity whose final handshake/write was cancelled mid-drain is counted abandoned (left
fenced-safe for the orphan reaper), not silently dropped from both counters.

**Files**: `server/crates/waddle-server/src/server/mod.rs` (`stop_token`, around line
182 where `clustering::start_if_enabled` is invoked — drain sequencing hooks into the
same shutdown path `spawn_graceful_shutdown_drain` in `session_janitors.rs` already
uses for the Q6 SM drain, line 723), `server/crates/waddle-server/src/clustering/
claims.rs` (batched release via `release_many`, draining-node marker on
`clustering_nodes` via `NodeLeaseStore::mark_draining`, Slice 2), Helm
`terminationGracePeriodSeconds` formula (`claimReleaseBudget` chart value, already
named as a Phase-0-introduced value consumed "from Phase 3 on" per the ADR's own
Implementation Plan text — this slice is what actually consumes it).

**Tests**: harness **drain-at-modeled-scale measurement** (drain thousands of claimed
entities; assert wall clock fits `claimReleaseBudget`) as an exit criterion, per the
ADR's own Phase 3 text. Postgres-gated: **ordinary in-process batch-construction
ordering** (major fix 13 — under normal, uninterrupted drain, an entity provably enters
the release batch only after its final fenced write's commit returns; asserted by
instrumenting commit/batch-append order, not only by the crash case); batched release
preserves the writes-before-release-per-entity ordering under a forced early SIGKILL
simulation (claims remain fenced-safe, merely un-released, if the budget overruns);
drain-interrupted relay ask increments `claims_abandoned_on_drain` (the cancellation
path above); **`release_many`'s epoch-blind ABA window** (`ClaimStore::release_many`'s
doc comment, Slice 1) — an entity queued for the release batch that this same
draining node re-claims at a higher epoch (e.g. a XEP-0198 `steal_for_resume` landing
back on this node) before the batched DELETE actually runs must not have that
brand-new claim silently deleted by the stale batch entry; this interleaving belongs
in the drain test suite alongside the ordinary batch-construction-ordering test above,
proving the Slice 2 draining-marker + Slice 10 batch-ordering mitigations narrow the
window in practice, not merely in the doc comment's argument.

**Dependencies**: Slice 5 (claim-scoped consumers must exist for "final fenced writes"
to mean anything), Slice 7 (room state drain), Slice 6 (SM detach-on-drain).

---

### Slice 11 — Activate the two deferred Phase 2 harness fencing scaffolds (D10)

Fills in and un-ignores:

```rust
#[tokio::test]
async fn lone_survivor_and_isolation_fencing() { /* Slice 2 primitives */ }

#[tokio::test]
async fn partial_partition_degrades_without_fencing() { /* Slice 5's durable-queue path */ }
```

`lone_survivor_and_isolation_fencing` could technically be filled as early as Slice 2
(heartbeat fencing alone suffices) but is formally activated here so both scaffolds
land together as this phase's harness-maturity capstone, matching the scoping map's
"D9/D10 last" ordering. `partial_partition_degrades_without_fencing` requires Slice 5's
durable `pending_delivery` fallback to exist (a single dead link between two of three
nodes must degrade to that path, not fence).

**Files**: `server/crates/waddle-server/tests/clustering_cluster_e2e.rs` only.

**Tests**: the two scaffolds themselves are the test deliverable.

**Dependencies**: Slice 2 (isolation fencing), Slice 5 (durable-queue fallback).

---

## Exit criteria for the phase overall

The multi-process harness (`clustering_cluster_e2e.rs`) must prove, end-to-end, across
real processes sharing one Postgres:

1. A claim acquired by node A is stolen by node B only via one of the three sanctioned
   CAS shapes (stale-owner, consent/epoch-only, steal-intent), never a lockless read.
2. A node that loses fencing (heartbeat CAS returns 0 rows) demotes all local claims and
   flips readiness to not-ready before its lease becomes stealable.
3. Lone-survivor-at-N=2 keeps serving; a single dead link among three nodes degrades to
   the durable queue without fencing either endpoint (Slice 11's two scaffolds).
4. Cross-node XEP-0198 resume succeeds for all three branches (detached-steal,
   live-handshake, held-response-then-steal) and correctly rejects a
   wrong-identity/forged-`previd` attempt.
5. A MUC room ownership steal restores configuration/affiliations/subject before
   accepting joins, and a deposed owner's next broadcast is blocked by the fencing
   SELECT.
6. XEP-0397 ISR (if shipped) consumes a token via non-secret-key lookup + constant-time
   compare, rotates on success, and destroys state on failed-token auth — or ISR stays
   unadvertised if the store does not ship with this phase.
7. Drain at modeled scale (thousands of claims) fits the `claimReleaseBudget` chart
   value.

This maps to the epic's five Phase 3 checkboxes: (1) ties the "epoch-fenced Postgres
ownership claims" box to criteria 1–3; (2) ties "fenced SM-session persistence +
cross-node XEP-0198 resume" to criterion 4; (3) ties "durable MUC room ownership" to
criterion 5; (4) ties "XEP-0397 ISR" to criterion 6; (5) the DashMap-selection-surface
retirement (Slice 9) is proven by the unchanged e2e `.cue` suite staying green plus the
new self-healing delivery test, not by the clustering harness (it is a single-node
concern).

## Carried risks / deferred to Phase 4

- **Per-message allowlist origin re-validation.** Still deferred, exactly as Phase 2
  left it: kameo's remote ask/tell handlers don't hand handlers the transport-level
  sender `PeerId`, so re-checking the allowlist per inbound message needs sender
  identity surfaced to handlers, landing with Phase 4's cross-node routing (alongside
  claim-epoch checks). Enforcement through Phase 3 remains exactly: denial at
  connection establishment, plus revocation of already-connected peers within one
  refresh interval.
- **`RelayHandle` cancellation-safety debt — resolved in Slice 6, no longer carried.**
  `relay.rs`'s doc comment notes `RelayHandle` "owns no cancellation token to race
  against" and that its pre-Phase-3 callers (the swarm smoke test, and "the Phase 4
  cross-node callers this type is built for") don't race local shutdown. Slices 6 and
  7 of this phase are the first production callers (the SM live-steal handshake, and
  the MUC Demote ask) — both invoked from contexts that also watch the clustering stop
  token. **Coordinator ruling: the debt is paid down in Slice 6** — `RelayHandle::new`
  gains a clustering-scope `CancellationToken` constructor parameter and every remote
  ask races it in a `biased` `select!`, mirroring `spawn_supervised` (see Slice 6's
  design note); Slice 10's drain path then treats a cancelled in-flight ask as the
  tested mechanism feeding `claims_abandoned_on_drain`. Listed here only so readers of
  Phase 2's carried-risk register see its closure recorded, not as an open risk.
- **`ensure_schema` vs. `SELECT`-only grants.** `allowlist.rs`'s own doc comment: once
  Phase 4's hardened runtime role lands (`SELECT`-only on the allowlist table), `CREATE
  TABLE IF NOT EXISTS` still requires `CREATE` on the schema even when the table
  already exists (Postgres checks the ACL before the existence short-circuit) — so
  every `ensure_schema` call this phase adds (`clustering_nodes`, `clustering_claims`,
  `clustering_steal_intents`, `clustering_isr_tokens`) must be dropped or gated when
  that grant change lands. Not addressed in Phase 3; flagged for the Phase 4 chart
  work.
- **sqlx pool acquire/statement timeout tuning.** Noted during the Phase 2 review and
  not revisited here: the control-plane pool (Slice 0) and the fenced SM-persistence
  path (Slice 4) both add new call patterns against sqlx pools whose acquire/statement
  timeouts have never been tuned against this workload. Left as a Phase 4 (or
  production-rollout) tuning exercise once real load-shape data exists.

## Hard-rule compliance notes

- **Typed payloads everywhere.** `Entity`/`EntityType`/`ClaimEpoch`/`NodeIdentity`/
  `ResumeIdentityProof`/`StalePredicate` are all typed values, never bare `String`s at
  call sites — matching the pattern already established by `NodeId`, `LeaseIdentity`,
  `LeasedSlot` in Phase 2's `lease.rs`. `EntityType` is a closed enum
  (`UserActor`/`RoomActor`/`SmSession`), not a string tag, so an entity-type typo is a
  compile error, not a runtime mismatch against a `TEXT` column value (the SQL layer
  still serializes it to `TEXT` at the storage boundary only, per the typed-payloads
  rule's "serialization to String/Vec<u8> happens only at the I/O boundary").
- **No `#[allow(...)]` / `#![allow(...)]`.** None introduced by this plan; every new
  module (`ownership/`, `clustering/claims.rs`, `clustering/isr.rs`,
  `sm_persistence_fenced.rs`, `clustering/self_fence.rs`) follows the existing
  `lease.rs`/`allowlist.rs` convention of clean clippy under `--all-features -D
  warnings`.
- **XEP custom test-suite hard rule.** XEP-0198 cross-node resume (Slice 6) and
  XEP-0397 ISR (Slice 8) each get a dedicated Rust test suite landing in the same PR as
  their behavior, per the rule and per the ADR's own Implementation Plan text
  ("Every phase that changes XEP behavior carries that XEP's dedicated Rust test suite
  in the same PR"). XEP-0397's stream-features/disco advertisement is re-added **only**
  once the Postgres store ships with this testable behavior (Slice 8) — if Slice 8 slips
  out of this phase for any reason, ISR stays unadvertised exactly as Phase 0 left it,
  never advertised-but-untested.
- **Postgres-only, feature-gated where the ADR requires it.** `clustering/claims.rs`,
  `clustering/isr.rs`, and `clustering/self_fence.rs` are `#[cfg(feature =
  "clustering")]`, consistent with every other Phase 2 clustering module. The
  `ClaimStore`/`IsrTokenStore` **trait definitions** and their in-process impls are
  deliberately **not** feature-gated (Q1/Q2), since ordinary single-node builds must
  keep compiling and running against them.

## Deviations from / extensions to the ADR text (need coordinator sign-off)

1. **`ClaimStore` split across crates** (trait + in-process impl in `waddle-xmpp/src/
   ownership/`, Postgres impl in `waddle-server/src/clustering/claims.rs`) — the ADR
   names the trait but not its module home; this plan picks a cross-crate split driven
   by compile-time reach (Q1).
2. **`session_registry/claims.rs` retrofit, not wrap** — existing inherent claim methods
   on `InMemorySmSessionRegistry` are refactored onto `Arc<dyn ClaimStore>` rather than
   left in place behind a new facade (Q2). This is a structural change to existing code,
   not purely additive.
3. **No `ClaimStore` trait exists today** — confirmed by code research. The scoping
   map's touches list could be read as implying partial groundwork exists; it does not.
   This is entirely net-new work, called out so nobody underestimates Slice 1's size.
4. **`PostgresFencedSmPersistence` issues its fencing SQL inline, with `ClaimStore` as
   an epoch-value side channel only** (corrected — blocker fix 2): the impl holds an
   `Arc<dyn ClaimStore>` solely to obtain the cached `<enable/>`-time `ClaimEpoch`
   value; the in-transaction `SELECT ... FOR SHARE` check itself is issued inline by
   the fenced impl on its own `Database::begin()` transaction, because a
   borrowed-transaction trait method (`fence_in(&self, tx: &mut Transaction<'_>, ...)`)
   cannot be expressed on a `waddle-xmpp` trait — `Transaction` is a
   `waddle-server`-local type (`db/backend.rs:443-552`) and the dependency runs the
   wrong way. The standalone `ClaimStore::fence` survives as advisory-only (own
   transaction, never the write-path mechanism). The ADR's "full second implementation
   of the trait" language still forecloses changing `SmPersistenceStorage`'s
   signatures (Slice 4).
5. **Q5 resolved as "no coupling"** between `pod_template_hash` and the keypair-slot
   lease — the ADR doesn't say they interact; this plan states explicitly that they
   don't, rather than leaving the scoping map's question open. **Extended (major fix
   14)** with a PROPOSED mechanism for "current generation": the `pod_template_hash`
   of the most recently registered live `clustering_nodes` row, so a rollback's
   re-rolling generation is automatically current; misclassification can only mis-tune
   the acquire-backoff heuristic, never ownership (the claims CAS stays authoritative).
6. **Q6 resolved as env-var/Helm-value config**, not a `nodes`-table row, for the new
   entity/node `lease_ttl` — reusing the Phase 2 mechanism rather than inventing a
   second one.
7. **Q7 resolved as "keep Phase 0's singleton guard and Phase 3's `nodes` table
   structurally separate"** rather than merging them into one mechanism.
8. **Q8 resolved as a dedicated `clustering_isr_tokens` table** (PROPOSED SQL, no
   ADR-locked DDL) **and** ISR advertisement gated on `clustering.enabled && Postgres`
   — meaning single-node/SQLite deployments do not regain ISR in this phase, a
   scope-narrowing choice beyond what ADR element 10's text literally requires. The
   original rationale's "IsrTokenStore is already dead code" claim is **corrected**
   (major fix 5): `create_token` is live and e2e-tested via the IQ path deviation 9
   retires.
9. **`IsrTokenStore` extracted into a trait** (`InMemoryIsrTokenStore` +
   `PostgresIsrTokenStore`), mirroring the `ClaimStore` split — a structural refactor
   of existing code, not additive-only — **and the live IQ-based token-issuance path
   is retired outright** (corrected, major fix 5): `handle_isr_token_request_iq`, its
   `handlers/iq/mod.rs` dispatch, and its e2e test are deleted because standalone-IQ
   issuance is non-conformant with XEP-0397 (issuance is inline
   `<isr-enable/>`/`<isr-enabled/>` on `<enable/>`/`<enabled/>`, never an IQ), a live
   violation of the repo's XEP-conformance hard rule (Slice 8).
10. **D1 split into three slices** (core CAS; demotion/self-fencing/readiness;
    steal-intents) and **D8 promoted to Slice 0**, ahead of being bundled with D1 as the
    scoping map's "D1(+D8)" grouping might suggest — a sequencing refinement, not a
    contradiction of the dependency graph.
11. **Control-plane pool implemented as a second sized pool via the existing
    `DatabaseAdapter`/`ConnectionGuard` machinery**, rather than a new connection
    -management abstraction — the ADR mandates the pool's existence and isolation, not
    its construction mechanics (Slice 0).
12. **XEP-0397/XEP-0388 namespace choice**: this plan commits to `urn:xmpp:sasl:2` (the
    vendored, current XEP-0388's namespace) for all SASL2 envelope elements, diverging
    from XEP-0397's own (older, pre-rename) `urn:xmpp:sasl:1` examples — flagged because
    combining the two vendored XEPs verbatim is not internally consistent (Slice 8).
13. **`RelayHandle`'s cancellation-safety debt becomes live in Slices 6/7 and is paid
    down in Slice 6** (corrected — major fix 12): sooner than the "Phase 4 cross-node
    callers" the existing doc comment anticipated, and resolved rather than carried —
    `RelayHandle::new` gains a clustering-scope `CancellationToken` parameter and every
    remote ask races it in a `biased` `select!`, mirroring `spawn_supervised`; Slice
    10's drain treats the cancelled-ask outcome as the tested feed for
    `claims_abandoned_on_drain`.
14. **Slice 5's cross-node janitor-flush test leg is deferred to Phase 4** (major fix
    4): the guaranteed-flush test in this phase covers same-node delivery only; the
    third-node-socket variant requires routed cross-node stanza delivery, which the
    ADR's Implementation Plan assigns to Phase 4 and this phase's Non-goals exclude.
15. **`ResumeIdentityProof` minting relocated into `ownership/resume.rs`** (blocker
    fix 3): the originally drafted shape (field private to `ownership/mod.rs`, minted
    from `session_registry/` and `waddle-server`) cannot compile across module/crate
    privacy boundaries. The identity check itself —
    `ownership::resume::verify_resume_identity(sasl_identity, snapshot_owner) ->
    Option<ResumeIdentityProof>` — is now the sole constructor in either crate, so
    holding a proof implies the real identity pair passed the check (Slices 1/6, reused
    by ISR consume in Slice 8).
16. **`clustering_claims.entity` stores a type-prefixed key (`entity_key`,
    `<entity_type_tag>:<id>`), not the bare `Entity::id`** (council fix): the ADR's
    element-4 column shapes name `entity`/`entity_type` as separate columns but do not
    say the primary key must fold the type in; binding `entity.id` alone would let a
    `UserActor` and a `RoomActor` sharing the same id collide on one row, since
    `entity_type` would otherwise only ever be written, never consulted to
    disambiguate. The tag set (`user_actor`/`room_actor`/`sm_session`) is closed and
    pairwise prefix-free, so the encoding is unambiguous even when `id` itself contains
    `:` (Slice 1, `waddle-server/src/clustering/claims.rs`).
