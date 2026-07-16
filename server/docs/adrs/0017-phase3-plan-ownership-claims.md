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
ISR unadvertised if the store does not ship with it. **Corrected premise (FIX 8,
council-adjudicated)**: ISR was never removed by an earlier phase — before this slice it
was advertised **unconditionally and non-conformantly** (the wrong namespace,
`urn:xmpp:isr:0`, and a bare `<isr/>` element with no `<mechanisms>` child, gated on
nothing). This slice is the first to gate the advertisement and fix its wire shape.

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
reconcile) — Slice 11 fills them in (verbatim scaffold names as they existed at
this Q4 decision's time; Slice 11's own corrigenda later renames
`partial_partition_degrades_without_fencing` to
`whole_node_isolation_fences_then_self_heals_without_operator_intervention` to
describe what actually lands — see that slice's "As-landed" text below). A third
`#[ignore]` (`dead_publisher_record_visibility_window`,
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
flow on `<enable/>`/`<enabled/>`. Single-node ISR remains unadvertised under this
slice's own `clustering.enabled && Postgres` gate (per above) — not, as an earlier
draft of this text incorrectly claimed, a re-imposition of a Phase 0 removal (FIX 8,
council-adjudicated correction): ISR was previously advertised unconditionally and
non-conformantly, never removed. This is a scope-narrowing deviation from a literal
reading of ADR element 10 (which does not textually restrict ISR to clustered
deployments) **and** a conformance-driven retirement of shipped code, both called out
again in the deviations list.

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
    // Added post-implementation (Slice 4, deviation 26 — council-adjudicated FIX 1):
    // acquire, or on conflict, an idempotent self-reacquire iff the existing row's
    // owner is exactly `me` (same node_id AND node_epoch); otherwise `AlreadyClaimed`,
    // exactly as `acquire` would return. `acquire` itself is unchanged and stays
    // strictly "fail if already claimed" — this is a distinct method, not a behavior
    // change to `acquire`.
    async fn ensure_claimed(&self, entity: &Entity, me: &NodeIdentity) -> Result<ClaimEpoch, ClaimError>;
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
    // Correction (implementation-time finding): `heartbeat` also takes
    // `lease_ttl` — the CAS's own `AND heartbeat >= now() - lease_ttl`
    // freshness predicate needs the value bound in, exactly like `expire`
    // below; the original sketch omitted it.
    async fn heartbeat(&self, me: &NodeIdentity, lease_ttl: Duration) -> Result<bool, ClaimError>; // false ⇒ fencing loss
    async fn expire(&self, owner: &NodeIdentity, lease_ttl: Duration) -> Result<bool, ClaimError>;
    async fn mark_draining(&self, me: &NodeIdentity) -> Result<(), ClaimError>; // Slice 10: stop acquiring new claims, keep serving owned ones
    // Added during implementation (not in the original sketch): the
    // isolation rule and the demotion-reconciliation query both need a
    // `clustering_nodes`/`clustering_claims` read this store already owns
    // the pool/table for, so both land here rather than inventing a third
    // store for two read-only queries.
    async fn count_other_live_nodes(&self, me: &NodeIdentity, lease_ttl: Duration) -> Result<usize, ClaimError>;
    async fn reconcile(&self, me: &NodeIdentity, locally_owned: &[Entity]) -> Result<Vec<Entity>, ClaimError>;
    // Added in Slice 3 (steal-intents unwedge/owner-veto path, deviation
    // 22 — intent CRUD lands here, not on the cross-crate `ClaimStore`,
    // since every caller is clustering-internal to waddle-server):
    async fn report_steal_intent(&self, entity: &Entity, reporter: &NodeIdentity) -> Result<(), ClaimError>;
    async fn owner_steal_intents(&self, me: &NodeIdentity) -> Result<Vec<(Entity, ClaimEpoch)>, ClaimError>;
    // Council-adjudicated fix (Slice 3, post-implementation review): returns
    // the affected-row count, not a bare `Result<(), ClaimError>` — a
    // single data-modifying CTE (`WITH fenced AS (SELECT ... FOR SHARE)
    // DELETE ... WHERE EXISTS (SELECT 1 FROM fenced)`) whose lock order is
    // the deliberate opposite of `steal_stale(StealIntentExpired)`'s own
    // consume-CTE, so the two serialize on the intent rows rather than
    // race an unlocked `EXISTS` read. `run_node_lease` treats a zero-rows
    // result after a nonzero `owner_steal_intents` entry as "possibly
    // deposed" and demotes immediately instead of believing the veto
    // succeeded.
    async fn clear_steal_intent(&self, entity: &Entity, me: &NodeIdentity, mine: ClaimEpoch) -> Result<u64, ClaimError>;
}
```

`register` covers both fresh startup and post-fence re-registration under a new
`node_id`/`node_epoch` (Q7/element 4). The Postgres impl lives alongside
`PostgresClaimStore` in `clustering/claims.rs` (same file, same control-plane pool).
Correction (implementation-time finding, supersedes the original sketch's "the
in-process/single-node arm is trivial" line): **no in-process/single-node
`NodeLeaseStore` implementation exists, and none is needed.** Unlike `ClaimStore`
(Q1/Q2), `NodeLeaseStore` itself lives entirely in `waddle-server`'s
`clustering/claims.rs`, gated `#[cfg(feature = "clustering")]` — no ordinary
single-node `waddle-xmpp` code has, or needs, a node-lease concept, since
`start_if_enabled` (the sole call site that constructs a `NodeLeaseStore` at all)
returns immediately when `clustering.enabled` is false, before any `NodeLeaseStore`
is ever constructed. There is exactly one production implementor
(`PostgresClaimStore`) plus a test double (`self_fence.rs`'s `FakeLease`) — never a
trivial single-node arm standing in for "no clustering."

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
what lands there as `whole_node_isolation_fences_then_self_heals_without_operator_intervention`,
renamed from this scaffold's original `partial_partition_degrades_without_fencing`);
`lone_survivor_and_isolation_fencing`
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

Steal CAS substitutes a data-modifying-CTE variant of `EXISTS (SELECT 1 FROM
steal_intents WHERE entity=$e AND created_at < now() - $intent_ttl)` for the owner-stale
predicate (council-adjudicated fix, below: the CTE *consumes* the authorizing intents
rather than merely reading them). Owner's heartbeat loop reads intents against its own
claims, health-asks the owning actor, and clears with an epoch-fenced DELETE on success —
**corrected phrasing (FIX 1(e))**: the guarantee is that this clear and a concurrent
steal serialize on the intent rows themselves (deadlock-abort-safe, see below), not any
inherent "unforgeability" of the write. Applies to **both** `RoomActor` and `UserActor`
claims; SM-session claims are never stolen this way (identity-bound resume only, Slice
6).

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
veto path — including the consume-CTE `steal_stale(StealIntentExpired)` and the
`FOR SHARE`-fenced `clear_steal_intent`, see the council-adjudicated fix below),
`server/crates/waddle-server/src/clustering/self_fence.rs` (`run_node_lease`'s owner-side
veto-scan wiring: `owner_steal_intents` → per-entity `health_check` →
`clear_steal_intent`/`demote`, deadline/cancellation-armed identically to every other
control-plane call in that function), `server/crates/waddle-server/src/config.rs`
(`ClusteringStealIntentConfig`/`WADDLE_CLUSTERING_STEAL_INTENT_TTL_MS`, deviation 23),
`server/crates/waddle-xmpp/src/registry/user_actor.rs` (internal health-ask
handler + proactive wedge-kill-and-conflict-close on a failed self health-check —
pre-empts the steal at `intent_ttl`), `server/crates/waddle-xmpp/src/muc/
room_actor.rs` (same health-ask handler for rooms).

**Council-adjudicated fix — the veto race, full closure (post-implementation review
finding)**: the first-landed pair was vulnerable to write skew under READ COMMITTED —
`clear_steal_intent`'s DELETE was gated on an *unlocked* `EXISTS` over `clustering_claims`,
and `steal_stale(StealIntentExpired)`'s UPDATE was gated on an *unlocked* `EXISTS` over
`clustering_steal_intents`; both could commit concurrently (a healthy owner's veto and a
stealer's claim both landing against the same observed epoch). Both statements were
redesigned as data-modifying CTEs that serialize on the **intent rows themselves**:
`steal_stale(StealIntentExpired)` becomes `WITH consumed AS (DELETE FROM
clustering_steal_intents WHERE entity=? AND created_at < now() - intent_ttl RETURNING 1)
UPDATE clustering_claims ... WHERE ... AND EXISTS (SELECT 1 FROM consumed)` — the DELETE
row-locks the authorizing intents, so a concurrent veto-clear and a steal serialize on
them: whichever deletes first wins, the loser's own predicate re-check (Postgres's
EvalPlanQual) observes nothing left to act on. This also closes the instant-re-steal hole:
a successful steal consumes the very intents that authorized it, so the new owner starts
with a clean slate and the full `intent_ttl` window of protection. `clear_steal_intent`
becomes `WITH fenced AS (SELECT 1 FROM clustering_claims WHERE entity=? AND node_id=? AND
claim_epoch=? FOR SHARE) DELETE FROM clustering_steal_intents WHERE entity=? AND EXISTS
(SELECT 1 FROM fenced)`, and — this is a **trait-signature change** — now returns the
affected-row count (`Result<u64, ClaimError>`, not `Result<(), ClaimError>`):
`run_node_lease` treats zero rows affected after a nonzero
`owner_steal_intents` entry as "possibly deposed" and demotes immediately rather than
believing the veto succeeded. The two statements deliberately acquire their
`clustering_claims`/`clustering_steal_intents` locks in **opposite order** — this is what
makes them serialize on the intent rows rather than race an unlocked read — so under
contention Postgres may abort either side with a `40P01 deadlock_detected` error; this is
safe (a typed `ClaimError::Backend`, never a panic; the loser retries next tick/scan) and
is documented explicitly in both statements' doc comments. Proven by a Postgres-gated
concurrent stress test (`steal_intent_veto_vs_steal_stress_never_both_succeed_same_round`)
racing two real connections against one entity across 200 rounds, re-seeding an
already-aged intent each round, asserting the invariant that a clear reporting
`rows_affected > 0` and a steal succeeding against the same observed epoch never both
happen in a round. The earlier doc-comment claim that this uses "the same single-statement
CAS discipline `steal_stale`/`steal_for_resume` already use" was corrected: those two are a
self-CAS on their own row; this is the cross-table case, and the actual guarantee is
serialization on the intent rows, deadlock-abort-safe — not any inherent
"unforgeability" of the write.

**Tests**: Postgres-gated: steal-intent veto vs. expiry; epoch-fenced clear is a no-op
for a deposed owner (and now asserts the exact affected-row count — `0` for a deposed
owner, `1` for the current owner); SM-session exclusion is a typed rejection
(`ClaimError::SmSessionExcludedFromStealIntent`) at both the intent-report surface
(`report_steal_intent`) and defensively inside the steal CAS itself
(`steal_stale`/`StalePredicate::StealIntentExpired`); the refresh-not-accumulate upsert;
`owner_steal_intents` returns only entities with an outstanding intent; the FIX 1(d)
concurrent stress test above; a `decode_entity` mismatched-prefix rejection test (a row
whose key does not start with its own `entity_type`'s tag is skipped and logged, never
silently mangled). Unit tests for
the health-ask handler surfaces (`UserActor`/`RoomActor` `HealthCheck`,
`health_check_or_wedge_kill`'s bounded-ask-then-kill behavior against a genuinely wedged
`UserActor`) and for `run_node_lease`'s veto-scan wiring against a configurable
`LocallyClaimedEntities` fake (healthy → `clear_steal_intent`; unhealthy → proactive
`demote`; and — the FIX 1(b) regression — a healthy check whose `clear_steal_intent` call
itself reports zero rows affected → proactive `demote`, not a believed-successful veto).

**Deferred (implementation-time finding, see deviation 21)**: the **harness**
**deposed-owner-with-live-socket case** as originally scoped — a real `UserActor` with a
live socket, claimed in production, stolen via the intent path in
`clustering_cluster_e2e.rs` — is not genuinely testable this slice: no production code
acquires a `UserActor`/`RoomActor` Postgres claim until Slices 5-7 wire
`LocallyClaimedEntities` to something non-empty (Slice 2's `NoLocallyClaimedEntities` is
still the only production wiring). Faking claim acquisition inside the harness just for
this one scenario would exercise a code path production never takes, not the mechanism
this test is meant to prove. The mechanism itself (CAS, intent CRUD, veto-scan wiring,
actor-level health-ask/wedge-kill primitives) is fully landed and unit-tested per above;
the harness scenario is carried forward to land alongside Slice 5-7's real claim
acquisition, where a genuinely wedged, genuinely-claimed `UserActor` can exist.

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
As landed (FIX 5(d) — the full touched-paths list, corrected from the single-file
description above): `server/crates/waddle-xmpp/src/ownership/mod.rs` (`ensure_claimed`
trait method + `Entity`'s new `Display` impl, deviation 26/FIX 2),
`server/crates/waddle-xmpp/src/ownership/in_process.rs` (`InProcessClaimStore`'s
`ensure_claimed`, and its claim map widened to also track each claim's owning
`NodeIdentity`, not just its epoch), `server/crates/waddle-xmpp/src/
stream_management/persistence.rs` (`SmPersistenceError::NotOwner`'s field retyped from
`String` to a typed `Entity`, FIX 2; new `ClusterColocationMismatch` variant, deviation
27), `server/crates/waddle-server/src/clustering/claims.rs`
(`PostgresClaimStore::ensure_claimed`), `server/crates/waddle-server/src/clustering/
mod.rs` (new `ClusteringHandles` return type — see its own paragraph below — replacing
`start_if_enabled`'s previous `Result<(), ClusteringError>`), `server/crates/
waddle-server/src/clustering/self_fence.rs` (`NodeLeaseRunConfig` gains
`live_identity: SharedNodeIdentity`, seeded and updated by `run_node_lease` alongside
every `identity = fresh` reassignment — the Slice 4+ follow-up plumbing note below,
now landed rather than merely flagged), `server/crates/waddle-server/src/lib.rs`
(`#[cfg(feature = "clustering")] pub mod sm_persistence_fenced;`),
`server/crates/waddle-server/src/server/mod.rs` (`start_with_config` threads
`start_if_enabled`'s returned `ClusteringHandles` onto `AppStateDeps`),
`server/crates/waddle-server/src/server/state.rs` (`AppState`/`AppStateDeps` gain
`clustering_claims: ClusteringHandles`), `server/crates/waddle-server/src/server/
http.rs` (`create_sm_session_registry` takes the `ClusteringHandles` and the global
`Database` handle, calling `open_for_cluster_mode`), `server/crates/waddle-server/src/
sm_persistence.rs` (`open_for_cluster_mode` dispatcher — the driver-selection function
described above, now also carrying FIX 4's co-location check and a
`crate::db::redact_database_url` helper for safely embedding DSNs in the resulting
error), `server/crates/waddle-server/src/db/mod.rs` (`Database` gains a
`database_url` field + `Database::database_url()` accessor, deviation 27).

**`ClusteringHandles` (FIX 8(c)'s plumbing, now landed)**: `start_if_enabled` returns
`Result<ClusteringHandles, ClusteringError>` instead of `Result<(), ClusteringError>`.
`ClusteringHandles` carries `claim_store: Option<Arc<dyn ClaimStore>>` and
`node_identity: Option<SharedNodeIdentity>` — both `None` whenever clustering is
disabled, this binary lacks the `clustering` feature, or (defensively) the subsystem
produced no live handles. `claim_store` wraps the *same* `Database` clone the
node-lease loop itself uses (not a second, independent store), and `node_identity` is
the same `SharedNodeIdentity` `run_node_lease` updates on every re-registration —
`ClusteringHandles::claim_pair()` hands both back together as
`Option<(Arc<dyn ClaimStore>, SharedNodeIdentity)>`, since a `ClaimStore` with no live
identity to bind into its CAS calls (or vice versa) is never a usable combination.
`AppState`/`AppStateDeps` carry it as `clustering_claims`, and
`server/http.rs::create_sm_session_registry` reads `state.clustering_claims.claim_pair()`
to decide, alongside `server_config.clustering.enabled`, which `SmPersistenceStorage`
`open_for_cluster_mode` constructs.

**Slice 4+ follow-up plumbing (FIX 8(c)) — landed, not merely flagged**:
`self_fence::run_node_lease` (Slice 2) previously held this node's current
`NodeIdentity` as a plain loop-local variable — reassigned in place on every
re-registration — with no getter or shared handle exposing "the identity this node
currently believes it holds" to any other call site. `NodeLeaseRunConfig` now carries
`live_identity: SharedNodeIdentity`; `run_node_lease` seeds it with the loop's initial
identity and calls `live_identity.set(identity.clone())` again alongside every
`identity = fresh` reassignment, so any other holder of a clone — this slice's
`PostgresFencedSmPersistence`, via the new `ClusteringHandles` — always observes the
identity currently in force rather than a stale pre-fence snapshot.

**Tests**: dedicated Postgres-gated test module (new, alongside
`sm_persistence/tests.rs`'s existing style) covering: every trait method under the
fenced impl behaves per divergence (a)/(b) above; a steal committed mid-transaction
causes a concurrent fenced write to observe 0 rows from the `FOR SHARE` SELECT and
abort before the write; `delete_session`/`store_session_atomic`/
`record_promotion_failure` are genuinely atomic with their fencing check (no window
where the SELECT passes but the steal lands before the subsequent statement) —
including a dedicated concurrent-race variant per method (FIX 6), not just
`delete_session`'s; `list_all_sessions` round-trips every persisted session under the
fenced impl (FIX 6); FIX 1's `ensure_claimed` idempotence for a self-reacquire and
`AlreadyClaimed` for a foreign owner, on both `ClaimStore` implementations; a
concurrent-first-writes race (two tasks, one fresh stream_id, both writes succeed,
exactly one `clustering_claims` row) at both the bare-`ClaimStore` level and through
`PostgresFencedSmPersistence` itself.

**Dependencies (corrected — blocker fix 1)**: Slice 1 (`ClaimStore` — supplies the
cached `ensure_claimed`-time `ClaimEpoch` (deviation 26) this impl binds into its own
inline `FOR SHARE` SELECT; see the design note above for why the fencing check is
issued inline rather than delegated to `ClaimStore`). Slice 0 is a **transitive**
dependency only, through Slice
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

**Forward note (deviation 26)**: `restore_from_persistence`'s acquire-then-hydrate step
should call `ClaimStore::ensure_claimed`, not a bare `acquire`, for the same reason
Slice 4's own `claim_epoch_for` side channel does — a session already claimed by this
exact node/epoch (e.g. re-observed across a retried hydration pass, or already picked
up by Slice 4's lazy first-fenced-write path before startup hydration reaches it) must
self-reacquire idempotently rather than spuriously failing with `AlreadyClaimed`. A
genuinely different node's claim still correctly fails hydration for that session.

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
>
> **Amended in place (FIX 6d, council-adjudicated)**: this paragraph's premise did not
> survive contact with the actual codebase — see **deviation 28**, ~950 lines below in
> the deviations log, for the implementation-time correction: no existing unscoped
> restore-time delete was found to claim-scope (issue #1098 deliberately hydrates
> expired sessions rather than deleting them at restore time). Acquire-then-hydrate
> claim-scopes the READ instead, and the claim-scoped SM-expiry janitor is the sole
> deletion path. Left here, unedited below this note, as the originally-drafted design
> intent; deviation 28 is the authoritative as-built record. (This paragraph previously
> mis-cited itself as "deviation 27" from `core.rs`'s doc comment — corrected to 28,
> since Slice 4's follow-up plumbing fix claimed 27 first.)

**Reporter calling convention (FIX 4, forward reference)**: this slice (or a later one,
per deviation 23's "Slice 5+" scoping) is where a genuine cross-node reporter — a node
whose deliveries to an entity's owner keep failing/getting NACKed — first calls
`NodeLeaseStore::report_steal_intent`. That call MUST happen on **crossing a failure
threshold** (N consecutive failed/NACKed delivery attempts), never on every individual
failed attempt: `report_steal_intent`'s upsert refreshes `created_at` on every call, so a
reporter calling it once per failed attempt faster than `intent_ttl` elapses between
attempts perpetually resets the intent's age and the steal can never clear `intent_ttl` —
permanently starving the exact unwedge path this mechanism exists to provide. See
`report_steal_intent`'s own doc comment (`waddle-server/src/clustering/claims.rs`) for
the full constraint.

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

> **As-landed correction (FIX 3, council-adjudicated)**: "promotion executes inside the
> Slice 4 fenced write path" understated the actual gap — Slice 4 fenced only
> `SmPersistenceStorage` (the `sm_sessions`/`sm_unacked` tables); the `pending_delivery`
> INSERT itself, issued from `sm_promotion/pending.rs::insert_pending`, ran completely
> unfenced. The as-landed design: a new
> `PendingDeliveryStorage::insert_fenced(row, origin_stream_id)` trait method (default
> impl falls back to the unfenced `insert` — byte-identical for every non-clustered
> implementation), overridden by `DatabasePendingDeliveryStorage` only when clustering
> is enabled AND this storage's own database is co-located with the clustering global
> database (`pending_delivery::open_for_cluster_mode`, mirroring
> `sm_persistence::open_for_cluster_mode`'s identical co-location-then-construct
> pattern one table over — a mismatch fails startup with the new
> `PendingStorageError::ClusterColocationMismatch`, never a silent unfenced fallback
> under a misconfigured co-location). The fenced path runs the `SELECT ... FOR SHARE`
> against `clustering_claims` and the `pending_delivery` INSERT in one
> `Database::begin()` transaction — the exact shape
> `sm_persistence_fenced::PostgresFencedSmPersistence::assert_fenced` already
> establishes, duplicated here (not shared via a generic helper) for the same
> "the lock and the write it protects must share one connection, and this impl owns its
> own inline fencing SQL" reason Slice 4's own design note gives. `origin_stream_id` is
> threaded from `promote_session_unacked`'s `session.stream_id` through
> `PromotionContext` into `insert_pending`, which now calls `insert_fenced` instead of
> the bare `insert` unconditionally (the fallback default makes this a no-op behavior
> change for every non-clustered deployment). A `NotOwner` result is treated exactly
> like any other storage failure by the caller: `confirm_drained` is NOT called, the
> durable SM row survives for the current (genuine) owner's own promotion pass.

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
evaluates the window against Postgres `now()`/DB-stamped `detached_at` (major fix 6,
subject to the deviation 28 correction above); duplicate-promotion (double-janitor)
prevention — landed as `pending_delivery::tests::insert_fenced_prevents_duplicate_promotion_across_claim_states`,
council-adjudicated FIX 3: two nodes attempting to promote the same SM session's
unacked queue under different claim states (one holding a now-stale/deposed claim, the
other the current one) resolve to exactly one winner, the loser aborting fenced
(`PendingStorageError::NotOwner`) before any write. **Amended (FIX 6b/6c, council-
adjudicated)**:
- The **guaranteed-flush janitor test, scoped to same-node delivery** (coordinator
  ruling, major fix 4 — the previous draft's "multi-node janitor flush" case smuggled in
  Phase-4 cross-node stanza routing, which this phase's own Non-goals exclude and which
  the ADR Implementation Plan assigns to Phase 4; see the Non-goals bullet above and
  deviation 14) is itself **deferred**, not landed this slice: its prerequisite —
  claim-scoped `pending_delivery` sweep-flush for owned bare JIDs — was deferred
  alongside deviation 35's nullable/unpopulated schema columns (no `UserActor`-claim
  concept exists yet to scope the sweep to; see deviation 35). Bound to the same
  Phase-4 entry as deviation 35 in "Carried risks / deferred to Phase 4," below.
- The **multi-process kill-one hydration harness scenario** (extend
  `clustering_cluster_e2e.rs`: two nodes, shared Postgres, kill one, assert the
  survivor's targeted hydration claims and hydrates only the dead node's orphaned
  sessions, not its own already-claimed ones twice) **moves to Slice 11's harness
  capstone** — see that slice's text for the binding sentence. The **sweep-level**
  guarantee (same claim-scoping behavior, one process, no multi-process harness
  needed) is covered NOW by a landed Postgres-gated test:
  `session_janitors::orphan_reaper_sweep_tests` exercises
  `run_orphan_reaper_sweep` end-to-end — seeds a stale-owner claim + a persisted
  session, sweeps, and asserts the expire CAS committed, the steal won, targeted
  hydration (FIX 2's `hydrate_reclaimed`, not `restore_from_persistence` — see that
  method's own doc comment on why it is startup-time-only) landed the session into
  memory, and exactly-once behavior holds under a concurrent second sweep.

**Forward reference (deviation 21, see also deviation 34 below)**: this slice's own
Files list does NOT wire a production `UserActor`/`RoomActor` claim-acquisition call
site — see deviation 34 for the correction to this paragraph's original forward
reference, which incorrectly implied this slice was where it would land. The harness's
deferred **deposed-owner-with-live-socket** scenario (Slice 3's owner-veto path,
exercised against a genuinely wedged, genuinely-claimed `UserActor` with a live socket)
remains carried forward — now bound explicitly to Phase 4's first slice that wires
`UserActor` Postgres claims (see "Carried risks / deferred to Phase 4," below).

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

**Forward note (deviation 26)**: the `<enable/>`-time claim-creation step above should
call `ClaimStore::ensure_claimed`, not a bare `acquire` — a fresh `<enable/>` for a
stream-id this node has never seen still gets a plain fresh claim (the self-reacquire
path never fires when there is no existing row), but `ensure_claimed`'s self-idempotence
is exactly what keeps this call from spuriously conflicting with Slice 4's own lazy
first-fenced-write acquisition for the same stream-id on this same node — the two call
sites coexist with no explicit hand-off protocol between them precisely because both go
through the same idempotent-for-self primitive.

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

**Files** (corrected — the as-landed shape replaces three of this list's originally
-named files with one new, concrete-typed one; see the explanation immediately below):
`server/routes/websocket/stream_management.rs` (three resume branches; calls
`ownership::resume::verify_resume_identity` per above), `server/crates/waddle-xmpp/src/
ownership/resume.rs` (completed here: `verify_resume_identity`), new
`server/crates/waddle-xmpp/src/stream_management/session_registry/cross_node_resume.rs`
(claim-aware cross-node resume dispatch: the three branches, the one-shot-CAS
discipline, the `RemoteResumeAsker` trait, and the identity-check call), `server/
crates/waddle-server/src/clustering/relay.rs` (the live-steal handshake rides
`RelayHandle` — the **first production (non-harness) caller**; this slice also pays
down `RelayHandle`'s cancellation-safety debt, below, rather than carrying it).

**Why `cross_node_resume.rs` replaces `tombstone.rs`/`trait_impl.rs`/`traits.rs` in this
list**: the originally-drafted Files list named those three existing
`session_registry/` files as the home for "claim-aware resume outcomes, same
identity-check call," anticipating that cross-node dispatch would be woven into the
existing trait-object/tombstone machinery those files already own. As implemented, the
entire cross-node dispatch — the three-branch logic, the one-shot-CAS discipline
(deviation 38), and the `RemoteResumeAsker` injection trait — is instead concrete,
inherent-`impl`-block code on `InMemorySmSessionRegistry` in its own new file,
`cross_node_resume.rs`, called directly from `stream_management.rs::handle_sm_resume`
only after the existing `claim_session`/`SmSessionRegistry`-trait path returns
`Ok(None)`. `tombstone.rs`/`trait_impl.rs`/`traits.rs` are untouched by this slice: there
was no concrete need to route cross-node dispatch through the trait-object layer those
files own, since the only production caller is the concrete `InMemorySmSessionRegistry`
type itself.

**Trust-model precision (council-adjudicated, deviation 54)**: the identity binding
`verify_resume_identity` performs — and every claim-aware resume outcome built on top
of it — protects against malicious/buggy **clients** and honest-node mistakes only. It
does not, and structurally cannot, protect against a **compromised allowlisted node**:
a remote node answering (or issuing) a `RelayResumeSteal` ask could forge
`requester_bare_jid` on the wire, and nothing in this identity check would detect it.
This is consistent with, not a gap introduced by, the ADR's enrolled-node-fully-trusted
cluster membership model — every other cross-node ask this phase adds makes the
identical trust assumption — but is worth stating plainly here so a future reader does
not mistake this identity check for a defense against a compromised peer.

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
and subject from Postgres before accepting any join."* This narrowed #1355 work keeps
immutable exact fences for actor-owned durable load, save, and delete operations. The
mutable cache remains the existing legacy pre-fanout groupchat dispatch/MAM consumer
until #1283 replaces it. Phase 4 GA gates (not this phase) assert outcast denial +
password/members-only hold after steal — this phase builds the mechanism, GA gates its
correctness.

At the #1355 stack boundary, generic restore returning `None` does not create a
durable parent. The exact-fenced subject UPDATE remains non-creating and returns a
typed pre-apply failure when that parent is absent, so an acknowledged subject can
never disappear on actor replacement. #1352 must atomically initialize the complete
room plus initial Owner to remove that temporary unavailability while preserving
creator/status-201 semantics; #1350 then hardens the broader parent/child integrity
contract.

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

**Files, amended (FIX 2–9 pass, deviations 66–73)**: `server/crates/waddle-xmpp/
src/muc/durable.rs` (`RoomClaimFenceContext`, ready-entry cache publication),
`server/crates/waddle-xmpp/src/ownership/mod.rs` (`Entity` hand-written
`Deserialize`, `ENTITY_ID_MAX_LEN`), `server/crates/waddle-server/src/
muc_durable.rs` (exact actor-owned durable fencing and legacy cache checks),
`server/crates/waddle-server/src/server/topology.rs`
(`seed_initial_xmpp_topology`'s `ClaimHeldByAnotherNode` continue),
`server/crates/waddle-server/src/server/routes/websocket/cleanup.rs`
(`get_or_create_room_actor`'s typed `Result`), `server/crates/waddle-server/src/
server/routes/websocket/handlers/presence/muc.rs` (join-path error mapping),
`server/crates/waddle-xmpp/src/muc/room_registry_actor.rs`
(`live_room` async + claim release), `server/crates/waddle-server/src/
clustering/{local_claims.rs,mod.rs}` (`health_check` fix,
`demote_room_actor`, `MucDurableStoreInit`), `server/crates/waddle-server/src/
admin/channels.rs` (`group_dm_rename_update_error` demote wiring).

**Tests**: Postgres-gated: fenced pre-fan-out SELECT returns 0 rows immediately after a
steal commits (deposed owner's very next broadcast is blocked). Harness: room ownership
steal restores config/affiliations/subject before the new owner accepts a join (this
phase's mechanism test; the Phase 4 GA gate re-runs it as outcast-denial-after-steal
once GA-gating lands). **Forward reference (deviation 21)**: this slice is also the
first production wiring of a `RoomActor` Postgres claim, so the harness's deferred
**deposed-owner-with-live-socket** scenario (Slice 3's owner-veto path, the `RoomActor`
counterpart — a genuinely wedged, genuinely-claimed room with a live occupant socket)
can land alongside this slice's own harness work.

**Pool assignment (blocker fix 1, stated explicitly per Slice 0)**: actor-owned
durable writes (config, affiliation lists, subject, and delete) run on the **main
pool** in the same transaction as their exact fence check — never the control-plane
pool, for the same same-connection-same-transaction reason Slice 4 states. Only
`RoomActor`'s own claim acquire/steal/heartbeat traffic (via `ClaimStore`, Slice 1)
rides the control-plane pool.

**Dependencies**: Slice 1 only (`ClaimStore` for `RoomActor` entities) — deliberately
**not** dependent on Slices 4–6, so it can be worked in parallel by a separate
contributor/session, per the scoping map's "D5 parallel-after-D1" ordering.

**As landed (deviations 56–64, see the deviation log for full detail)**: occupant-roster
durability is deferred (56, its sole consumer needs Phase-4 cross-node presence
fan-out); the cache-backed pre-fanout backstop remains the existing legacy path (57);
`LocallyClaimedEntities::owned()`
widened to `async fn` for the actor-backed `RoomLocalClaims` implementor (59);
`Entity`/`EntityType`/`ClaimEpoch` gained `Serialize`/`Deserialize` for the Demote
ask's wire payload (60, `Entity::id` gained a matching wire-length bound in 73);
claim acquisition + dead-owner re-election land in `RoomRegistryActor` with a new
`ClaimHeldByAnotherNode` error for the live-foreign-owner case (61); the Demote ask
(part (a) of the two-part demotion protocol) is implemented end-to-end, receiving
side included, via a new `swarm::RelayBridges` bundle (62); the non-clustered path is
confirmed byte-identical via `InProcessClaimStore` defaults, with the durable schema
landed as `clustering_muc_rooms`/`clustering_muc_room_affiliations` (63); and the
deposed-owner-with-live-socket `RoomActor` harness scenario lands in
`clustering_cluster_e2e.rs`, reusing the restore-before-join mechanism itself as the
wedge point (64).

**Council-adjudicated FIX 2–9 pass, additionally landed (deviations 66–73, see the
deviation log for full detail)**: every durable-relevant `RoomActor`
mutation handler fails closed on exact ownership BEFORE mutating and surfaces a
non-ownership persist failure typed AFTER mutating (66); a dead/panicked
`RoomActor`'s Postgres claim is released rather than
orphaned, and `RoomLocalClaims::health_check` reports unhealthy (not healthy) when
no live local actor exists for a claim this node believes it holds (67); a genuine
`RestoreDurableRoomState` load failure fails closed — joins refuse until a bounded
inline retry succeeds, never served against defaults (68); topology bootstrap
treats a room claimed by another live node as a normal steady-state condition and
keeps seeding the rest (69); the MUC join path no longer silently drops a join with
no presence reply for any registry/actor failure (70); the three named
kick/ban/members-only-eviction fan-out sites were traced and confirmed already
covered by (66)'s mutation gating, with no independent fan-out needing its own
fenced check (71); a `MucDurableStore` init failure is now fatal to clustering
startup rather than logged-and-continued (72); and `Entity::id` gained the same
wire-length bound `SmSessionId` already has (73).

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
`feature-not-implemented` — corrected per deviation 79 and the actual landed test,
matching this repo's real unhandled-IQ catch-all condition, not `service-unavailable` —
not a token). ISR advertisement itself: a test asserting disco/
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
to self-healing via `TrySendPeer` → `DroppedClosed` eviction — the retirement the Phase
1 completion note deferred to "a later slice", though (deviations 87-92 below, in
particular deviation 90) via a substitute mechanism the note didn't name, not either of
its own two named preconditions. The headless-persistence gate (`
run_headless_recipient_pass` on an all-`DroppedClosed` bare-JID delivery) ships
alongside the filter retirement, per that note's own dependency ("only needed once the
liveness filter is retired"). **Note (routing.rs scope):** the `routing.rs` ×2 sites
migrate `waddle_xmpp::routing::StanzaRouter`, which — code research confirms — has no
production construction site; it is pre-existing/test-only. Migrating it is correct
per this slice's stated scope, but it does not reduce live production DashMap traffic
and should not be counted toward this slice's risk-reduction total.

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

**LANDED (implementation note, deviations 87-92).** The two DashMap methods were replaced by
crate-level async free functions — `waddle_xmpp::registry::select_routable_resources_for_user`
and `get_resources_for_user` in the new `waddle-xmpp/src/registry/selection.rs`
(each: `GetUser` on the `UserRegistryActor`, then `SelectRoutableResources` /
`GetResources` on the resolved `UserActor`; degrade to empty on any actor
error) — rather than open-coding the `GetUser`+ask pair at every site. The two
inherent `ConnectionRegistry` methods and their four DashMap unit tests were
deleted from `connection_registry/resources.rs` + `connection_registry/tests.rs`.
Migration map (17 sites): `routing.rs` ×2 (`StanzaRouter` gained a
`user_registry: ActorRef<UserRegistryActor>` field; its constructor and 9 unit
tests updated); `admin/channels.rs` (`run_group_dm_leave` +
`handle_group_dm_leave` + `register` threaded a `user_registry` handle, wired
at `http.rs` and 3 `tests/iq.rs` call sites); `presence/subscription/directed.rs`;
`presence/subscription/delivery.rs`; `presence/probe.rs` ×3;
`message/group_dm_invite.rs`; `pubsub_fanout.rs` ×2; `interpret/offline_delivery.rs`
(guards on `deps.user_registry`); `interpret/groupchat_archive.rs::push_inbox_update`
(gained `user_registry: Option<&ActorRef<…>>`, threaded through
`GroupchatInboxProjectionInputs`, `groupchat_inbox.rs`, `displayed_marker.rs`,
`iq/archive_inbox_upload.rs`); `sm_promotion/live.rs` (`build_online_resources`,
`collect_live_targets`) + `pending.rs` (a `DeliveryHandles<'a>` bundle to stay
under clippy's arg cap) — threaded through `sm_promotion.rs`
(`promote_session_unacked` signature, `DisplacedPromotionDeps`,
`PromotionContext`) and its two production callers in `session_janitors.rs` and
three `DisplacedPromotionDeps` construction sites (`cleanup.rs` ×2,
`stream_management/registration.rs`). Slice 5 had already made the
`sm_promotion` sites' *fenced write* claim-aware; this slice removed their
DashMap *selection* reads only, per the "avoid re-touching the same lines"
dependency. **Count reconciliation:** the "Settled scope" paragraph above
cites the scoping map's ~14 confirmed direct former-DashMap call sites; the 17-site
migration map here is that same set plus the downstream threading/wiring sites the
migration required to satisfy those call sites' new `user_registry` parameter —
`http.rs`, `tests/iq.rs` ×3, `groupchat_inbox.rs`, `displayed_marker.rs`, and
`archive_inbox_upload.rs` — which are not themselves former-DashMap call sites.

The Slice-1 liveness filter (`select_bare_jid_live_targets`'s DashMap-liveness
intersection + `filter_deliverable` helper) was deleted; `route_to_connection`'s
bare-JID arm now selects from the actor alone (`SelectRoutableResources`, then
`GetResources`). Self-healing wiring: `deliver_peer_to_full` /
`deliver_direct_to_full` / `deliver_to_detached` / `deliver_one_via_actor` now
return a typed `DeliveryDisposition` (`DeliveredLive` / `QueuedDetached` /
`Dropped`); the bare-JID arm tracks whether *anything* landed across the live +
detached delivery set and, when nothing did, runs `run_headless_recipient_pass`
(local-domain only) — the headless-persistence gate that ships alongside the
filter retirement. The empty-actor reaper is unchanged and still holds.

**Deviation (accepted behaviour change, council-visible; deviation 90).** The retired filter
guaranteed selection was *byte-for-byte* identical to legacy even inside the
lagging-unregister window. Without it, a stale extra holding a **unique top
priority** whose channel has already closed is still ranked top by the actor
and selected alone (the max-priority collapse no longer runs after a DashMap
intersection), so on that first delivery the live *lower*-priority resources are
not live-delivered — the message is persisted (the shared fan-out recipient
pass archives it regardless of the live-send outcome; for the non-fan-out paths
the new headless gate persists it) and the DroppedClosed send evicts the ghost,
so the very next bare-JID delivery converges on the true live top-priority
resource. This is the filter→self-healing trade the Phase 1 completion note
deferred to "a later slice" — though, as deviation 90 details, delivered via a
substitute mechanism (the headless-persistence gate) rather than either of the
note's own two named preconditions (the note's actual language describes the
risk the filter mitigates as "a stale-extra message-loss window", not the "no
message loss" framing this text previously and inaccurately quoted). No message
is lost under the substitute mechanism either: it is only briefly routed to
offline storage instead of a live lower-priority resource in the narrow
stale-ghost window. The two Slice-1 exact-parity guard tests
(`route_to_connection_bare_jid_filters_stale_actor_extra_absent_from_dashmap`,
`…_stale_top_priority_extra_does_not_promote_lower`) were rewritten to assert
this new self-healing behaviour
(`…_sole_stale_extra_self_heals_via_dropped_closed` — this is the fifth test
above — and `…_stale_top_priority_extra_self_heals_to_live_lower`, which also
proves convergence on the next delivery). The four presence-probe/subscription
tests that broke were fixed at the test-setup level only: they registered the
*requester* on the DashMap alone, so after the probe/subscription path's
migration to the actor the requester's resources were invisible; switching them
to the production-parity `register_test_connection` (dual-register) restores the
requester in the actor tree — no privacy/routing assertion was weakened.

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

**As-landed summary (implementation-time findings; see deviations 93–105,
in "Deviations from / extensions to the ADR text" below and the "Slice 10
council-adjudicated fixes pass" corrigenda addendum, for the full
detail)**: the acquire-side draining gate this section's spec assumed
already existed (per `release_many`'s own trait doc comment) did not — it
is landed here (93). Q5's mechanism needed a new `first_seen` column no
prior slice's DDL specified (94). Rollout-aware backoff required two
call-site strategies across the `waddle-xmpp`/`waddle-server` crate
boundary, unified behind one pure decision function and a new
`RolloutBackoff` trait (95). The generic drain (`clustering::drain::
run_shutdown_drain`) is scoped to `RoomActor` only, composing with — not
replacing — the existing Q6 SM drain via a shared metrics surface, not a
shared code path (96, 97). `start_if_enabled` now returns the node-lease
task's join handle so process exit genuinely waits on the drain completing
(98). `claimReleaseBudget` landed as a typed config value plus an
unconditional Helm wire-through (99). The drain-at-modeled-scale exit
criterion landed as a single-process Postgres-gated test rather than a
multi-process harness scenario (100). The `release_many` ABA window is
reproduced at the SQL level and proven closed in practice for the one
entity type the production drain path actually batches (101). A
council-adjudicated review pass then found the seal-before-release barrier
still trivially defaulted `true` in production (never actually implemented
for `RoomActor`, closed for real in 104) and that heartbeat renewal
stopped the instant shutdown fired rather than surviving the drain window
(closed in 105); the drain-interrupted-relay-ask interleaving was found
unreachable this slice (102), and the SIGKILL-simulation test substitution
was recorded explicitly (103).

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
"D9/D10 last" ordering. `partial_partition_degrades_without_fencing` (landed as
`whole_node_isolation_fences_then_self_heals_without_operator_intervention` — see
"As-landed" below) requires Slice 5's durable `pending_delivery` fallback to exist (a
single dead link between two of three nodes must degrade to that path, not fence).

**Binding (FIX 6b, council-adjudicated)**: Slice 5's own Tests paragraph deferred its
multi-process kill-one hydration scenario here — this is that scenario's landing slice.
Extend `clustering_cluster_e2e.rs` with a claim-scoped-hydration harness case: two real
processes, shared Postgres, kill one, assert the survivor's targeted hydration
(`hydrate_reclaimed`, via the orphan reaper) claims and hydrates only the dead node's
orphaned sessions, never re-hydrating its own already-claimed ones a second time. The
single-process, sweep-level version of this same guarantee already landed in Slice 5
itself (`session_janitors::orphan_reaper_sweep_tests`) — this harness case is the
multi-process capstone proving the identical behavior holds across a real process kill,
not merely within one process's own claim bookkeeping.

**Binding (Slice 7 / deviation 70)**: the MUC join-bounce wire shape lands here too — a
join against a foreign-owned room in the two-node harness must receive the conformant
`<presence type='error'>` with `<resource-constraint/>` (`type='wait'` semantics), never
a silent drop; asserting it over real processes proves what a doubles-based unit test
cannot.

**Files (as-landed)**: `server/crates/waddle-server/tests/clustering_cluster_e2e.rs`
(the two scaffolds plus the two bound scenarios); plus, from the post-landing
corrigenda closing deviations 109–111:
`server/crates/waddle-server/src/clustering/self_fence.rs` (deviation 109's new test),
`server/crates/waddle-xmpp/src/stream_management/session_registry/cross_node_resume.rs`
(deviation 110's new test), and — deviation 111's sole sanctioned production change —
`server/crates/waddle-server/src/config.rs` (the new
`WADDLE_CLUSTERING_ORPHAN_REAPER_INTERVAL_MS` field/parsing/validation) and
`server/crates/waddle-server/src/server/session_janitors.rs` +
`server/crates/waddle-server/src/server/http.rs` (threading it in, replacing the
hardcoded constant).

**Tests**: the two scaffolds themselves are the test deliverable, plus the two bound
scenarios above (kill-one hydration; foreign-owned-room join bounce); plus deviations
109–111's three additions (two new unit tests, one new config-parsing test).

**Dependencies**: Slice 2 (isolation fencing), Slice 5 (durable-queue fallback).

**As-landed**: `lone_survivor_and_isolation_fencing` was already filled as of this
slice's start (Slice 2). This slice filled and un-ignored the second scaffold, landed
as `whole_node_isolation_fences_then_self_heals_without_operator_intervention` (renamed
from the plan's own scaffold name, `partial_partition_degrades_without_fencing` —
deviations 107/108: no per-edge partition primitive exists in this harness, and
asserting the durable-`pending_delivery` cross-node fallback INSIDE a partition test
would be non-diagnostic, not unobservable — the landed test instead proves targeted
isolation-fencing plus genuine unattended self-healing recovery; see the "Exit criteria
for the phase overall" section below for the honest criterion-3 accounting), added the
Slice 5 kill-one claim-scoped hydration capstone
(`orphan_reaper_kills_one_node_and_hydrates_only_its_orphaned_sessions`, deviation 106: no
production caller ever expires a silently-dead peer's node-lease row today, so the test
seeds that one precondition directly through the same public `ClaimStore::expire` method
the production reaper itself calls, mirroring the already-accepted single-process
`orphan_reaper_sweep_tests` convention), and added the Slice 7/deviation-70 MUC
foreign-owned-room join-bounce wire-shape assertion
(`muc_join_bounces_when_room_claim_is_held_by_another_node`).

A council-adjudicated post-landing review of this capstone found two coverage gaps and
one missing operational knob, closed in the same slice (deviations 109–111 below, full
detail in the "Slice 11 as-landed corrigenda" section):

- **Deviation 109**: `self_fence.rs`'s terminal "demote ALL local claims" branch (exit
  criterion 2) had no test combining a genuine fencing-loss trigger with a POPULATED
  `LocallyClaimedEntities` — closed with
  `self_fence::tests::node_lease_demotes_every_owned_claim_on_fencing_loss`. The demote-all
  code itself was already correct; only the test coverage was missing.
- **Deviation 110**: cross-node XEP-0198 resume's branch 3 (owner-unreachable
  hold-and-retry, exit criterion 4) had only failure/timeout coverage, never a retry
  that actually succeeds — closed with
  `cross_node_resume::tests::fix_b_flaky_asker_holds_then_succeeds_on_a_later_attempt`.
- **Deviation 111 (the one sanctioned production change in this otherwise harness-only
  slice)**: the orphan reaper's cadence (`session_janitors::ORPHAN_REAPER_INTERVAL`) was
  the only cluster timer with no env override — every sibling timer already has one. Added
  `WADDLE_CLUSTERING_ORPHAN_REAPER_INTERVAL_MS` (default 120s, parsed/validated identically
  to every sibling `ClusteringConfig` timer) and set it to 500ms in
  `clustering_cluster_e2e.rs::spawn_cluster_server`'s env list, so
  `orphan_reaper_kills_one_node_and_hydrates_only_its_orphaned_sessions` observes a real
  sweep in seconds instead of waiting out the ~120s production default per attempt. Minimal
  and deliberately scoped to the timer's period only — the reaper's logic is unchanged.

See deviations 106–111, below, for the full as-built record.

---

## Exit criteria for the phase overall

Restated here, per the Slice 11 corrigenda, with the actual backing test(s) for each
criterion — not the plan's original, uncited prose. Most criteria are proven by
dedicated Postgres-gated unit/integration suites (not exclusively the multi-process
`clustering_cluster_e2e.rs` harness, contrary to this section's original framing); each
line below names its module and test function(s).

1. **A claim acquired by node A is stolen by node B only via one of the three sanctioned
   CAS shapes (stale-owner, consent/epoch-only, steal-intent), never a lockless read.**
   `clustering::claims::tests`: `steal_stale_with_stale_epoch_loses_the_race` +
   `steal_from_vanished_node_succeeds_via_not_exists` (stale-owner shape),
   `steal_for_resume_succeeds_against_a_fresh_owner` +
   `steal_for_resume_with_stale_epoch_loses_the_race` (consent/epoch-only shape),
   `steal_intent_unanswered_past_ttl_allows_the_steal` +
   `steal_intent_veto_clears_before_expiry_blocks_the_steal` (steal-intent shape), and
   `steal_commit_interleaved_inside_a_fenced_transaction` (every shape commits inside a
   fenced transaction, never a lockless read). Cross-node re-election through the same
   stale-owner shape is proven again, at the `RoomActor` layer, by
   `muc::room_registry_actor::tests::get_or_create_room_steals_from_a_dead_owner_and_notifies_it`.
2. **A node that loses fencing (heartbeat CAS returns 0 rows) demotes all local claims
   and flips readiness to not-ready before its lease becomes stealable.**
   `clustering::self_fence::tests::node_lease_self_fences_on_heartbeat_cas_zero_rows`
   (readiness flips not-ready on a genuine CAS-zero-rows fence) plus, closing this exit
   criterion's own coverage gap (deviation 109, Slice 11 corrigenda),
   `node_lease_demotes_every_owned_claim_on_fencing_loss` (the SAME fencing-loss trigger,
   this time with a POPULATED `LocallyClaimedEntities` holding 2+ owned claims, proving
   the terminal demote-all loop actually demotes every one of them, not just the ones a
   veto scan happens to touch).
3. **Lone-survivor-at-N=2 keeps serving; a single dead link among three nodes degrades
   to the durable queue without fencing either endpoint (Slice 11's two scaffolds).**
   `clustering_cluster_e2e::lone_survivor_and_isolation_fencing` proves the first half
   (Part 1: N=2 lone survivor keeps serving; Part 2: N>=3 isolation DOES fence). **The
   second half is NOT met as originally scoped** — deviations 107/108 (Slice 11
   corrigenda): no per-edge (single-link) partition primitive exists anywhere in this
   harness, only whole-node symmetric isolation, and the durable-`pending_delivery`
   fallback path cannot be asserted as a *partition-triggered* behavior at all (see
   deviation 108's precise restatement — that write path never consults cross-node
   liveness, so it fires identically in a fully healthy cluster and proves nothing
   partition-specific). What IS proven, by
   `clustering_cluster_e2e::whole_node_isolation_fences_then_self_heals_without_operator_intervention`
   (renamed from the plan's original scaffold name,
   `partial_partition_degrades_without_fencing`, to match): a fully, symmetrically
   isolated node self-fences while its uninvolved peers stay ready throughout, then —
   with no process restart and no operator action — genuinely self-heals once
   connectivity returns (fresh `NodeIdentity` re-registers, the original row is
   committed-expired, readiness re-arms). This is a real and useful property, but it is
   a narrower one than "a single dead link... degrades... without fencing either
   endpoint" as originally written.
4. **Cross-node XEP-0198 resume succeeds for all three branches (detached-steal,
   live-handshake, held-response-then-steal) and correctly rejects a
   wrong-identity/forged-`previd` attempt.**
   `xep0198_cross_node_resume`: `pure_two_node_detached_steal_race_exactly_one_winner`
   (detached-steal), `live_handshake_via_asker_falls_through_to_detached_path`
   (live-handshake), `forged_previd_wrong_identity_returns_not_authorized_without_stealing`
   (forged-`previd` rejection); `clustering_cluster_e2e::cross_node_resume_live_steal_handshake`
   proves the same live-handshake branch over real subprocesses. The
   held-response-then-steal branch's own coverage gap (only failure/timeout paths were
   exercised) is closed by deviation 110 (Slice 11 corrigenda):
   `stream_management::session_registry::cross_node_resume::tests::fix_b_flaky_asker_holds_then_succeeds_on_a_later_attempt`
   drives a retry that actually iterates the bounded backoff loop at least twice before
   landing `Claimed` on a later attempt, proving the hold-and-retry path's SUCCESS case,
   not just its `OwnerUnreachable` timeout case (already covered by
   `fix1_slow_asker_never_holds_past_the_handshake_budget`).
5. **A MUC room ownership steal restores configuration/affiliations/subject before
   accepting joins, and a deposed owner's next broadcast is blocked by the fencing
   SELECT.**
   `muc::room_registry_actor::tests::restore_durable_room_state_applies_before_any_join`
   (restore-before-join, config/affiliations/subject) and
   `muc_durable::tests::check_fenced_fanout_returns_false_immediately_after_a_steal_commits`
   (the deposed owner's very next fenced pre-fan-out SELECT returns false immediately
   after a steal commits — the plan's own Slice 7 Tests entry, quoted verbatim in that
   test's doc comment).
6. **XEP-0397 ISR (if shipped) consumes a token via non-secret-key lookup +
   constant-time compare, rotates on success, and destroys state on failed-token auth —
   or ISR stays unadvertised if the store does not ship with this phase.**
   `clustering::isr::tests`: `consume_matches_and_rotates_inside_the_fenced_transaction`
   (non-secret-key lookup + rotation), `consume_uses_constant_time_equality_not_eq`
   (constant-time compare), `consume_with_wrong_token_destroys_the_row_without_sql_where_comparison`
   (destroys state on failed-token auth).
7. **Drain at modeled scale (thousands of claims) fits the `claimReleaseBudget` chart
   value.** `clustering::drain::tests::drain_at_modeled_scale_fits_the_claim_release_budget`.

This maps to the epic's five Phase 3 checkboxes: (1) ties the "epoch-fenced Postgres
ownership claims" box to criteria 1–3 (with criterion 3's honest caveat above); (2) ties
"fenced SM-session persistence + cross-node XEP-0198 resume" to criterion 4; (3) ties
"durable MUC room ownership" to criterion 5; (4) ties "XEP-0397 ISR" to criterion 6; (5)
the DashMap-selection-surface retirement (Slice 9) is proven by the unchanged e2e `.cue`
suite staying green plus the new self-healing delivery test, not by the clustering
harness (it is a single-node concern).

## Carried risks / deferred to Phase 4

- **No stale-node watchdog: a hard-crashed (SIGKILL/OOM) node's claims are NOT
  auto-reclaimed by the orphan reaper this phase (deviation 106) — the most
  operationally-significant risk this phase carries.** Traced exhaustively in
  deviation 106 (Slice 11 corrigenda, below): the orphan reaper's own candidate scan
  (`list_orphaned_sm_session_claims`) and `steal_stale(OwnerStale)`'s own CAS predicate
  both read ONLY the dead owner's *committed* `clustering_nodes.expired` flag — never a
  raw heartbeat comparison. Auditing every production caller of `NodeLeaseStore::expire`
  turns up exactly two, and neither one commits that flag for a genuinely, silently dead
  peer: (a) the orphan reaper's own per-already-surfaced-candidate reaffirmation (a
  no-op for a row that isn't a candidate yet — circular for the row this risk is about),
  and (b) `self_fence::expire_bounded`, called only on a node's OWN just-superseded
  identity during ITS OWN graceful re-registration. **In production, a bare SIGKILL or
  OOM-kill of a node therefore leaves that node's `sm_session` claims un-reclaimable by
  the orphan reaper until one of two things happens**: a client that held one of those
  sessions attempts its own XEP-0198 `<resume/>`, hitting Slice 6's cross-node
  live-steal handshake (which authorizes via `ResumeIdentityProof` and never touches the
  `expired` flag at all — this path does NOT depend on the watchdog gap and works
  today); or the dead node itself eventually restarts and gracefully drains/re-registers.
  A session that is never resumed, on a node that never comes back, stays claimed by a
  dead node's identity indefinitely. Recommended Phase 4 follow-up (not actioned this
  phase, per this slice's own HARD RULES against speculative production changes): a
  bounded, periodic "stale-node watchdog" that proactively calls
  `NodeLeaseStore::expire` against every heartbeat-stale `clustering_nodes` row
  cluster-wide, not just already-surfaced claim candidates — closing this gap for
  genuinely abandoned nodes in production, not merely inside this phase's harness
  (which seeds the equivalent precondition directly, exactly because no such production
  caller exists yet).
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
- **Deposed-owner-with-live-socket harness scenario (`UserActor` variant) — bound to
  Phase 4's first `UserActor`-Postgres-claims slice (FIX 6a, council-adjudicated).**
  Deviation 21/34: this scenario requires a genuinely wedged, genuinely-claimed
  `UserActor` with a live socket, contested via the steal-**intent** veto path (Slice 3)
  — no slice in this phase's own breakdown (Slices 0-11) wires a production `UserActor`
  claim-acquisition call site (Slice 5 wires only `sm_session`; Slices 6/7 wire
  cross-node SM resume and `RoomActor` respectively). The `RoomActor` variant of this
  same scenario is NOT carried here — it already lands in Slice 7, per that slice's own
  text. Only the `UserActor` variant is deferred, to whichever Phase 4 slice first wires
  `UserActor` claim acquisition.
- **Inline post-fence reclaim vs. another node's concurrent resume (FIX 4, deviation
  32, council-adjudicated) — residual window, not eliminated.** The re-registration
  success path's inline reclaim of this node's own just-expired identity's `sm_session`
  claims (`self_fence::reclaim_own_expired_claims`) can race a genuinely different
  node's own `<resume/>`/claim-steal attempt for one of those same entities, landing in
  the brief window between this node's fence and its own re-registration completing.
  Not specially arbitrated: the ordinary epoch-fenced `steal_stale` CAS resolves it
  exactly like any other concurrent-steal race (first commit wins, the loser retries
  or fails cleanly) — this is the SAME mechanism every other concurrent-claim race in
  this phase relies on, not a gap unique to the inline reclaim. Named here explicitly,
  per the coordinator's ruling that residual windows be described honestly rather than
  implied-closed by the "readiness gate now does real work" framing.
- **Fail-open-detach orphan gap: unclaimed persisted rows invisible to the orphan
  reaper until a restart-time restore (FIX 6e, council-adjudicated).** The orphan
  reaper's candidate scan (`list_orphaned_sm_session_claims`) and the general
  claim-scoped hydration path both operate over `clustering_claims` rows — a row that
  was never claimed in the first place (the best-effort
  `acquire_claim_store_entry_for_detach` call on detach failed, e.g. a transient
  `ClaimStore` backend outage at the exact moment of detach) has no `clustering_claims`
  row for any node to steal, dead or alive. Such a row is invisible to every claim-scoped
  code path this phase adds — it is discovered only when this same node's own next
  `restore_from_persistence` (a restart) re-attempts the claim for it and this time
  succeeds, or never, if the node never restarts. `acquire_claim_store_entry_for_detach`
  is deliberately best-effort by design (a stream id is a freshly minted UUID; risking
  the unacked queue on a claim failure is worse than proceeding without a durable claim
  record — see that function's own doc comment), so this gap is an accepted, narrow
  consequence of that design choice, not a new hazard FIX 2/4/5 introduce. Left
  unaddressed in Phase 3; a Phase 4 fix would need either a periodic full-table
  reconciliation pass (re-introducing the unscoped-scan hazard Slice 5 otherwise
  eliminates, so it would need its own claim-scoped design) or a durable
  "detach acknowledged" marker distinct from the claim row itself.
- **Informational note (FIX 1–9 pass, council-adjudicated): room passwords have NO
  existing mechanism anywhere in this codebase, not merely an un-durable one.** A
  codebase-wide search at the time of this pass found no XEP-0045 §10.1.2 password
  field on `RoomConfig`, no password-check branch on join, and no password-related
  column in the Slice 7 durable schema (`clustering_muc_rooms`) — the feature does
  not exist pre- or post-Phase-3, in memory or durably. This matters for Phase 4's
  planned GA outcast-denial/"password holds" gate (element 7, Phase 4 scope per
  this plan's own exit-criteria text): that gate will need the room-password
  *feature itself* designed and implemented from scratch (config field, join-time
  check, wire error shape) before durability across a steal can be asserted for it
  — Phase 3's `RoomConfig` durability round-trip (deviation 63's
  `clustering_muc_rooms.config_json`) will carry a password field for free once one
  exists on `RoomConfig`, but does not itself create the feature.

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
  in the same PR"). XEP-0397's stream-features/disco advertisement is gated **only**
  once the Postgres store ships with this testable behavior (Slice 8) — if Slice 8 slips
  out of this phase for any reason, ISR stays unadvertised (FIX 8: not, as previously
  drafted here, a reversion to a "Phase 0 removal" — it was never removed, only
  previously advertised unconditionally and non-conformantly), never
  advertised-but-untested.
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
17. **The coarse isolation signal (`ConnectedPeerCount`, `self_fence.rs`) can only
    ever UNDER-fence, never over-fence — the previous revision of this plan's Slice 2
    doc comment had the safety direction backwards** (council-adjudicated fix): "zero
    connected swarm peers of any kind" is a safe approximation of "reaches none of the
    live peers" in exactly one direction. If this node is connected to at least one
    *live* peer, both the real condition and the approximation agree ("not isolated");
    if it is connected to zero peers of any kind, it cannot be connected to a live one
    either, so both again agree (isolated). The gap is a node connected to one or more
    *stale/non-live* peers (a peer whose own `clustering_nodes` row has since gone
    `expired`/`draining`, connection not yet torn down) while reaching zero live
    peers: the real condition is true (isolated from every live peer) but
    `reachable_peers >= 1`, so the approximation says "not isolated" and fails to
    fence when the ADR's literal per-node rule would. Accepted as an interim gap
    pending the Phase 4 `PeerId` ↔ `NodeId` correlation this phase does not build.
18. **`NodeLeaseStore::reconcile` (and, by the same reasoning, `expire`) has no
    production caller this slice** — `LocallyClaimedEntities` is wired to
    `NoLocallyClaimedEntities` (an always-empty stub) in `start_if_enabled`, per
    Slice 2's own doc comment, because no code acquires a Postgres-backed claim
    in production until the fenced `SmPersistenceStorage` (Slice 4) starts calling
    `ClaimStore::acquire` at `<enable/>` time. `reconcile`'s query and demote-callback
    plumbing are real and store-level tested (mirroring the Slice 1 precedent of
    `ClaimStore::fence` landing with zero production callers), not dead code by the
    repo's hard rule — they simply have nothing to reconcile against yet. Likewise,
    nothing in production calls `expire` this slice (the only sanctioned caller,
    `steal_stale`'s `OwnerStale` path, itself has no production caller until claims
    exist to steal) — `count_other_live_nodes`'s own heartbeat-freshness filter (fix
    below) is what keeps the isolation heuristic truthful in the meantime, without
    depending on `expire` ever running.
19. **Readiness clears on `register` + hysteresis alone this slice — the ADR's "plus
    claim re-acquisition" conjunct is not yet real, and becomes real only once Slice
    4+ wires `LocallyClaimedEntities` to something non-empty.** Element 4's text reads
    "cleared only on successful re-registration under a fresh `node_id`/`node_epoch`
    **plus claim re-acquisition**"; `run_node_lease`'s re-registration path (Slice 2)
    flips readiness back to ready as soon as `register` succeeds and
    `can_reacquire_claims` is satisfied, with no claim-reacquisition step at all —
    because there is nothing to reacquire yet (deviation 18: no production claims
    exist this slice). Once Slice 4+ makes `LocallyClaimedEntities` real, the
    re-registration path must gain an actual claim-reacquisition wait (e.g. re-issuing
    `ClaimStore::acquire`/`steal_for_resume` for this node's own previously-owned
    entity set) before flipping readiness — a short constraint comment is left at
    that call site in `run_node_lease` (`self_fence.rs`) marking this as
    Slice-4-plus-onward follow-up work, so it is not silently forgotten once claims
    exist to reacquire.
20. **`count_other_live_nodes` reads the raw `heartbeat` column (heartbeat-freshness
    filter), not only the committed `expired`/`draining` flags** (FIX 1(c),
    council-adjudicated): this does not violate the "never infer expiry from raw
    heartbeat" rule from element 4/Slice 1's steal-CAS design — that ban is scoped to
    fencing CAS predicates deciding whether *another node's claim* may be taken
    (`steal_stale`'s `OwnerStale` predicate), where a race between reading a stale
    heartbeat and the owner's own renewal landing a moment later could let two nodes
    believe they both hold the same claim. `count_other_live_nodes` makes no fencing
    decision over anyone's claims; it is a purely advisory count feeding this node's
    *own* isolation heuristic. Without this filter, a hard-killed node's
    `clustering_nodes` row — never explicitly expired, since nothing calls `expire`
    this slice (deviation 18) — would count as "live" forever, permanently inflating
    every other node's isolation count; the filter also excludes `draining` rows
    (Slice 10's marker, and the just-fenced identity's row per FIX 1(b)) regardless of
    heartbeat freshness, so a node that has just self-fenced stops inflating others'
    counts immediately rather than waiting out the full TTL window.
21. **Slice 3's owner-veto loop, health-ask handlers, and wedge-kill mechanism have no
    production caller this slice — implementation-time finding, same pattern as deviation
    18/19, one slice later.** `run_node_lease`'s veto scan
    (`lease.owner_steal_intents(&identity)` → `local_claims.health_check(entity)` →
    `clear_steal_intent`/`demote`) is real and runs every heartbeat interval, but
    `local_claims.owned()` is always empty (`NoLocallyClaimedEntities`, unchanged from
    Slice 2's wiring in `start_if_enabled` — no code acquires a `UserActor`/`RoomActor`
    Postgres claim until Slices 5-7), so `owner_steal_intents` always returns an empty set
    and the scan is a no-op every interval in production. Likewise, `UserActor::HealthCheck`/
    `ConflictCloseAllResources`/`health_check_or_wedge_kill` and `RoomActor::HealthCheck`
    are real, unit-tested actor primitives with no production call site — no
    `LocallyClaimedEntities::health_check` implementation exists to call them. This mirrors
    deviation 18's "mechanism is real and store-level tested, not dead code by the repo's
    hard rule — it simply has nothing to reconcile against yet" reasoning exactly, applied
    one layer up. The corresponding **harness deposed-owner-with-live-socket case** is
    deferred for the same reason (exact edit landed in Slice 3's own Tests paragraph
    above, not just here, and forward-referenced again from Slices 5 and 7's own Tests
    paragraphs) rather than faked with a claim-acquisition shortcut the harness would
    otherwise never exercise in production. **FIX 3 note for the future real
    implementations**: `LocallyClaimedEntities::demote`'s doc contract (tightened
    post-implementation review) requires hard-kill discipline against a wedged
    target — `UserActor::health_check_or_wedge_kill` is the exemplar `UserActor`'s own
    future `demote` must route through; `RoomActor` has only `HealthCheck` today (no
    `ConflictCloseAllResources`/wedge-kill counterpart yet — that lands with Slice 7's
    real claim wiring), and whichever slice wires a real `RoomActor`-backed
    `LocallyClaimedEntities` must give it an equivalent hard-kill primitive before
    routing `demote` through it, for both the reconcile-deposed and veto-health-fail
    call sites.
22. **Steal-intent CRUD (`report_steal_intent`/`owner_steal_intents`/`clear_steal_intent`)
    lands on `NodeLeaseStore`, not on the cross-crate `ClaimStore` trait.** Q1's
    unconditional-compilation rationale for `ClaimStore` (ordinary single-node
    `waddle-xmpp` code needs *some* `ClaimStore` in every build) does not apply here: every
    caller of intent CRUD is clustering-internal to `waddle-server` (the owner's
    `run_node_lease` veto scan, and — from Slice 5+ — the cross-node reporter that would
    call `report_steal_intent` after N failed deliveries), exactly the same reasoning
    Slice 2 already used to keep `NodeLeaseStore` itself out of the cross-crate split.
    `StalePredicate::StealIntentExpired` remains on the shared `StalePredicate` enum
    (`waddle-xmpp/src/ownership/mod.rs`) since `steal_stale` itself is a `ClaimStore`
    method every implementation must handle; only the intent CRUD that *populates*/*clears*
    the table is server-local.
23. **Config: `ClusteringConfig::steal_intent: ClusteringStealIntentConfig { intent_ttl:
    Duration }`, env var `WADDLE_CLUSTERING_STEAL_INTENT_TTL_MS`** — the plan's Slice 3
    text names `intent_ttl` as a value ("a small multiple of the heartbeat interval," element
    4) but does not lock a field/env-var name; this plan picks the same
    `Clustering<Concern>Config` + `WADDLE_CLUSTERING_<CONCERN>_*` convention `node_lease`/
    `self_fence` already established (Slice 2), rather than folding it into either existing
    struct (it gates a third, independent CAS — the steal-intent predicate — not node-lease
    renewal or isolation fencing). Default 60s (6x the default 10s node-lease heartbeat
    interval); validated `> 0` and `>= 2x WADDLE_CLUSTERING_NODE_LEASE_HEARTBEAT_INTERVAL_MS`,
    mirroring `node_lease_ttl`'s own validation shape. **FIX 7(d) — sharpened floor
    rationale**: the owner-veto scan only runs once per heartbeat tick, so a report that
    lands just *after* one scan has already passed needs a full further interval before
    the *next* scan even looks at it — worst-case phase alignment between "when the
    intent is reported" and "when the owner's scan runs" already consumes most of one
    interval before the owner gets its first chance to observe and clear the intent. A
    bare `> 1x` floor would let an unlucky phase alignment leave zero real interval
    inside the window at all; `2x` is the conservative floor that guarantees at least one
    full scan interval survives inside `intent_ttl` regardless of phase, mirroring
    exactly why `node_lease_ttl` itself uses the same `2x` floor against its own
    heartbeat interval. **No production call site issues
    `StalePredicate::StealIntentExpired` this slice** (deviation 21: the cross-node reporter
    is Slice 5+ scope), so this value is parsed and validated but not yet read by any
    downstream call — the same "config lands with its mechanism, ahead of the mechanism's
    first production caller" pattern `node_lease`/`self_fence` themselves followed in Slice
    2.
24. **`ClaimError::NotYetImplemented` removed** (repo dead-code hard rule): it existed
    solely to mark `StalePredicate::StealIntentExpired` as unrealized in Slice 1's
    `PostgresClaimStore::steal_stale`; Slice 3 implements that predicate, and no other
    variant ever used the error, so the variant is deleted rather than left as an unused
    dead branch. The SM-session exclusion this slice adds (rule 1 of the three-rule
    steal-variant block) is a **new, distinct** typed rejection —
    `ClaimError::SmSessionExcludedFromStealIntent` — not a repurposing of the removed
    variant.
25. **Fencing extended to all seven `SmPersistenceStorage` write methods, not only the
    three the locked spec names by name** (council-adjudicated, post-implementation
    review of Slice 4): element 1's locked text singles out `delete_session`,
    `store_session_atomic`, and `record_promotion_failure` as needing the fencing
    `SELECT` moved *inside* a `Database::begin()` transaction, because those three
    were already (or needed to become) multi-statement — the locked list identifies
    which methods required that structural conversion from per-statement
    `Database::guard()` calls to one transaction, not an exhaustive statement of which
    methods must be fenced at all. `PostgresFencedSmPersistence` fences all seven
    methods that write `sm_sessions`/`sm_unacked` on behalf of an SM-session entity —
    `upsert_session`, `delete_session`, `append_unacked`, `ack_through`,
    `delete_unacked`, `store_session_atomic`, `record_promotion_failure` — each inside
    its own `Database::begin()` transaction with the `FOR SHARE` fencing `SELECT` as
    the first statement, per the module's own "Fencing design (per-method)" doc
    section. Reading the locked list as exhaustive would leave `upsert_session`,
    `append_unacked`, `ack_through`, and `delete_unacked` writing `sm_sessions`/
    `sm_unacked` with no fencing check at all — exactly the unscoped-write hazard this
    whole slice exists to close. The four read-only methods (`get_session`,
    `list_unacked`, `list_expired_sessions`, `list_all_sessions`) remain deliberately
    unfenced, per their own doc comments (a resuming/stealing node must be able to read
    a session it does not yet own, and cross-entity listings are not scoped to one
    claim at all).
26. **`ClaimStore::ensure_claimed` + a per-stream-id `tokio::sync::OnceCell`
    single-flight, replacing this document's earlier "read the epoch once from the
    `<enable/>`-time acquire" interim wording** (council-adjudicated FIX 1,
    post-implementation review of Slice 4): the original Slice 4 design note describes
    `PostgresFencedSmPersistence` caching a `ClaimEpoch` "read once from the
    `<enable/>`-time `ClaimStore::acquire` call" — but nothing in Slice 4 itself calls
    `<enable/>`-time `acquire` yet (that is Slice 5/6 wiring), so this slice's actual
    `claim_epoch_for` side channel calls the CAS itself, lazily, on the first fenced
    write for a stream-id it has not yet seen. A bare `acquire` there hits two
    hazards: (1) two concurrent first writes for the same not-yet-claimed stream_id
    can race two independent `acquire` calls, one of which spuriously loses with
    `AlreadyClaimed` even though both are this same node; (2) once Slice 5/6 wires a
    real `<enable/>`-time `acquire` ahead of this path's first fenced write, that
    first write's own `claim_epoch_for` call would hit the same spurious
    `AlreadyClaimed` against its own process's just-created row (a self-lock). The
    fix adds `ClaimStore::ensure_claimed(entity, me) -> Result<ClaimEpoch, ClaimError>`
    (Slice 1's trait — acquire, or on conflict, an idempotent self-reacquire iff the
    existing row's owner is exactly `me`'s `node_id` **and** `node_epoch`; otherwise
    `AlreadyClaimed`, exactly as `acquire` would return) to both `ClaimStore`
    implementations, and layers a per-stream-id `Arc<tokio::sync::OnceCell<ClaimEpoch>>`
    (stored in `claim_epochs: DashMap<SmSessionId, Arc<OnceCell<ClaimEpoch>>>`, not a
    bare `DashMap<SmSessionId, ClaimEpoch>`) under it in
    `PostgresFencedSmPersistence::claim_epoch_for`, so concurrent callers for the same
    stream-id single-flight onto one in-flight `ensure_claimed` call. `acquire` itself
    is unchanged — still strictly "fail if already claimed" — preserving Slice 1's own
    contract and tests. **Forward note for Slice 5/6**: their `<enable/>`-time
    acquire, and any later claim-reacquisition-on-resume path, may keep calling
    `ensure_claimed` (or `acquire`, where a genuinely fresh claim is intended) freely —
    `ensure_claimed`'s self-idempotence means a later Slice 4 fenced write for the same
    stream-id under the same node identity observes the already-created row as a
    self-reacquire rather than erroring, so the two mechanisms coexist without an
    explicit hand-off protocol between them.
27. **Clustered SM persistence must be co-located with the clustering claims tables in
    the same Postgres database** (council-adjudicated FIX 4, post-implementation review
    of Slice 4): `PostgresFencedSmPersistence` no longer opens an independent pool from
    `WADDLE_XMPP_SM_DATABASE_URL` (or its `WADDLE_DATABASE_URL` fallback) — its fencing
    `SELECT ... FOR SHARE` targets `clustering_claims`, which lives in the clustering
    global database, so a second, independently-resolved database might not even have
    that table. `sm_persistence::open_for_cluster_mode` now takes the same `Database`
    handle `clustering::start_if_enabled` itself received (`db_pool.global()`) and
    constructs `PostgresFencedSmPersistence` by cloning it, never by opening a fresh
    pool from the resolved SM URL. Before that clone happens, the resolved SM
    database URL is compared against the global database's own URL
    (`Database::database_url`, a new accessor); a mismatch while `clustering.enabled`
    fails startup with the new typed
    `SmPersistenceError::ClusterColocationMismatch { sm_database_url, global_database_url }`
    variant (both fields pre-redacted by the caller — DSNs commonly carry credentials —
    via the new `crate::db::redact_database_url` helper) rather than silently running a
    fencing check against a table that may not exist wherever the SM URL actually
    points. When clustering is disabled, or the resolved SM URL is not a
    `postgres://`/`postgresql://` DSN at all, this check is skipped entirely and
    `open_for_cluster_mode` falls back to the portable implementation exactly as
    before — the co-location invariant only binds the specific combination this
    phase's fenced impl actually requires.
28. **Major fix 6's premise did not hold against the actual codebase** (Slice 5
    implementation-time finding): no existing unscoped restore-time delete was found in
    `restore_from_persistence` to "move behind the claim." Issue #1098 deliberately
    hydrates expired sessions instead of deleting them at restore time, precisely so
    their unacked queues still run the Q6 promote → confirm chain rather than being
    silently discarded. Resolution: this slice does not add a new inline restore-time
    delete (which would re-introduce the #1098 bug by deleting before promotion runs).
    Acquire-then-hydrate claim-scopes the READ instead (a row is hydrated only when this
    node's `ClaimStore::ensure_claimed` call for it succeeds), and the now claim-scoped
    SM-expiry janitor (see deviation 30) is the sole deletion path for expired rows —
    closing the ADR's actual hazard (an unscoped delete racing a second node's
    claim-scoped hydration of the same row) without needing a delete that never existed.
29. **The SM-session claim lifecycle is extended from "held only during the
    claim_session → complete_claim resume window" to "held continuously while the
    session sits in `sessions`"** (Slice 5 implementation-time finding, load-bearing for
    acquire-then-hydrate to mean anything in a real cluster). Before this slice,
    `InMemorySmSessionRegistry`'s `ClaimStore` claim was acquired only transiently during
    a resume attempt (`claim_session`) and released again on every path that returned a
    session to the `sessions` map or discarded it — correct in the single-node model,
    where nothing else could ever contend for the same entity, but insufficient once a
    second node's `restore_from_persistence`/orphan reaper can genuinely race for the
    same row: a released claim on a session still sitting in this node's `sessions` map
    is invisible to Postgres, so a second node could legitimately claim and hydrate a
    duplicate in-memory copy. Landed changes, all in
    `waddle-xmpp/src/stream_management/session_registry/{core.rs,claims.rs,trait_impl.rs}`:
    - `trait_impl.rs::store_session` now acquires (`ensure_claimed`) the claim for every
      freshly detached session before returning, via the new
      `claims.rs::acquire_claim_store_entry_for_detach` (best-effort: a failure logs and
      proceeds rather than risking the unacked queue, since a stream id is a freshly
      minted UUID and a genuine collision is not expected in practice).
    - `claim_session` switches from a bare `acquire` to `ensure_claimed` (FIX 1's
      self-reacquire), since the entity is now typically already claimed by this exact
      node from the detach step above; a genuinely foreign claim still correctly fails.
    - `release_claim` no longer unconditionally releases the store entry: it releases
      only when the session is NOT reinserted into `sessions` (expired, or absent from
      `claimed_sessions`) — reinserting a still-owned, merely claim-attempt-aborted
      session must keep the claim, not drop it.
    - Every OTHER terminal removal from `sessions` (`take_session`,
      `displace_stored_session_if_unclaimed`, `cleanup_expired`,
      `invalidate_sessions_for_jid`'s plain-detached branch, `confirm_drained`) now also
      releases the store entry — previously only `claimed_sessions` removals did, which
      was correct under the old (transient-claim) model but leaked claims under the new
      one.
    - `InMemorySmSessionRegistry.node_identity` changes type from a plain `NodeIdentity`
      snapshot to `SharedNodeIdentity` (`with_claim_store`'s signature changes to match),
      mirroring `PostgresFencedSmPersistence`'s own Slice 4 follow-up plumbing fix — every
      claim call site now reads `.current()` at the moment it needs the identity, so a
      self-fence/re-registration mid-process-lifetime is observed immediately rather than
      silently binding claim CAS calls to a stale, pre-fence `node_epoch` forever. Without
      this, acquire-then-hydrate and the orphan reaper's self-reacquire path would be
      subtly broken across every re-registration.
    - Production wiring closed the loop: `server/http.rs::create_sm_session_registry` now
      calls `.with_claim_store(claim_store, node_identity)` using
      `ClusteringHandles::claim_pair()` — previously this call was never made in
      production, so the registry silently stayed on the single-node
      `InProcessClaimStore` default even with clustering enabled and the fenced
      `SmPersistenceStorage` wired in, defeating acquire-then-hydrate entirely.
30. **Fenced-cache eviction (carried debt (a)) closed via a new `SmPersistenceStorage`
    trait method, not a `ClaimStore` hook**: `evict_claim_cache(&self, stream_id:
    &SmSessionId)` (default no-op) lets `InMemorySmSessionRegistry` (which already holds
    `Arc<dyn SmPersistenceStorage>`) notify the fenced impl to drop its per-stream epoch
    cell on every claim-ending path, without `ClaimStore` needing to know about
    `sm_persistence_fenced`'s private cache at all (a `ClaimStore`-level hook would have
    had to be either generic over an opaque cache-key type or SM-specific on a
    supposedly-entity-agnostic trait). `PostgresFencedSmPersistence` overrides it to
    remove the cell; the portable impl keeps the default no-op (it has no such cache).
31. **Real `LocallyClaimedEntities` (`clustering::local_claims::SmSessionLocalClaims`)
    scoped to `EntityType::SmSession` only, wired via a fill-in-later cell**:
    `start_if_enabled` must hand `run_node_lease` a `LocallyClaimedEntities` before the
    SM session registry exists (the registry needs `ClusteringHandles` itself, which
    `start_if_enabled` returns) — resolved with `SmSessionLocalClaims::new()` (empty,
    identical observable behavior to `NoLocallyClaimedEntities` until wired) plus
    `SmSessionLocalClaims::wire(registry)`, called once `server/http.rs::create_sm_session_registry`
    builds the registry. `UserActor`/`RoomActor` claim acquisition is out of this
    slice's Files list (the plan's own "Slices 5-7" framing) — `owned()` therefore never
    reports either type. This is a **narrower** scope than a literal reading of "the
    real `LocallyClaimedEntities`" might suggest, called out because it directly bears
    on deviation 21 (below): the steal-intent veto scan stays exactly as vacuous in
    production as it was under `NoLocallyClaimedEntities`, since steal-intents never
    apply to `SmSession` claims at all (Slice 3 rule 1).
32. **Deviation 19's "readiness clears on register + hysteresis alone" gap — closed in
    substance by FIX 4 (council-adjudicated), superseding this deviation's original
    "narrowly, not via a full owned-entity reclaim" framing.** The original design (as
    first implemented): every entity this node owned before a self-fence is demoted
    (forgotten locally, `ClaimStore` claim left untouched in Postgres) before the
    re-registration retry loop even starts, so `local_claims.owned()` is already empty
    by the time re-registration succeeds — nothing left in this process's own
    bookkeeping to "re-acquire" by name. The closure called `NodeLeaseStore::expire` on
    this node's own just-superseded identity (best-effort, bounded, mirroring
    `mark_draining_bounded`) immediately before flipping readiness, making its dropped
    claims eligible for the orphan reaper (deviation 33) as early as possible — but
    readiness itself did not block on the reaper's own independent cadence actually
    reclaiming anything.
    **FIX 4's closure**: `run_node_lease`'s re-registration success path now does two
    more things, still before `set_ready(true)`: (a) re-runs the `owned()`/`demote`
    sweep a second time (a session may have detached and self-claimed under the STALE
    pre-fence `live_identity` during the retry window itself — the original one-shot
    sweep at fence-entry ran before that window even opened, so it structurally could
    not have seen these); (b) calls a new bounded helper
    (`self_fence::reclaim_own_expired_claims`) that lists this node's own just-expired
    identity's orphaned `sm_session` claims (filtered client-side from
    `list_orphaned_sm_session_claims`'s cluster-wide candidate set — never touching
    another node's claims), `steal_stale(OwnerStale)`s each under the freshly
    re-registered identity, and hydrates the winners via
    `LocallyClaimedEntities::hydrate_reclaimed` (FIX 2's `hydrate_reclaimed` path, never
    `restore_from_persistence`). This makes "claim re-acquisition" a real, inline action
    this exact node takes for its own dropped claims, rather than purely delegating to
    the reaper's independent 120s cadence — the reaper remains the backstop for every
    OTHER node's genuinely dead claims (this inline step's owner filter never touches
    those). **Residual window, named honestly (not eliminated)**: another node's
    `<resume/>`/claim-steal attempt for one of these same entities, racing this node's
    inline reclaim in the brief window between this node's fence and its own
    re-registration completing, is not specially arbitrated here — the ordinary
    epoch-CAS semantics (`steal_stale`'s epoch-fenced UPDATE) resolve it exactly as any
    other concurrent-steal race does: whichever CAS commits first wins, the other
    observes a stale epoch and retries/fails cleanly. See "Carried risks / deferred to
    Phase 4," below, for this window named as its own entry.
33. **Orphan reaper (element 9) reuses `restore_from_persistence` for its hydrate step
    instead of re-deriving codec/expiry logic**: `NodeLeaseStore` gains
    `list_orphaned_sm_session_claims` (a new, read-only, unlocked candidate scan over
    `clustering_claims`/`clustering_nodes` using the identical owner-stale `NOT EXISTS`
    predicate `steal_stale(OwnerStale)` already uses), returning `OrphanedSmSessionClaim
    { entity, epoch, owner }`. `server::session_janitors::spawn_orphan_reaper_janitor`
    (a new ninth janitor, 120s cadence) scans, commits `expire` on each stale owner
    (element 9's ordering requirement), then `steal_stale(OwnerStale)`s what it can; on
    any successful steal it re-runs `sm_session_registry.restore_from_persistence()`
    rather than hand-rolling a promote-or-hydrate branch — a freshly-stolen claim
    self-reacquires via `ensure_claimed`'s self-match (same node/epoch the reaper just
    won under), so the existing, already-tested acquire-then-hydrate logic hydrates
    exactly (and only) what this sweep just reclaimed, whether that means "immediately
    eligible for the SM-expiry janitor's promote → confirm chain" (expired) or "hydrated
    for a future local resume attempt" (not yet expired) — both outcomes the ADR's
    "expire or promote" text names, obtained for free rather than re-derived. This
    requires a second `Arc<dyn NodeLeaseStore>` handle on `ClusteringHandles`
    (`node_lease`, feature-gated like `local_claims`) plus the configured `lease_ttl`
    (`ClusteringHandles::lease_ttl`), since the janitor runs off `WebSocketState`, which
    does not otherwise carry `ServerConfig`/`ClusteringNodeLeaseConfig`.
34. **Deviation 21 (the deposed-owner-with-live-socket harness scenario) remains
    deferred, NOT landed this slice** — correcting the plan's own forward reference
    ("this is also the first slice where a production `UserActor` acquires a real
    Postgres claim"). Slice 5's actual Files list touches only SM-session claim
    acquisition (`session_registry`, the SM-expiry janitor, the new orphan reaper,
    `sm_promotion`/`pending_delivery`) — no `UserActor`/`RoomActor` claim-acquisition
    call site is in scope here (that is Slices 6/7's work, per the plan's own "Slices
    5-7" framing elsewhere). **Correction (FIX 3, council-adjudicated)**: at the time
    this deviation was first written, "`sm_promotion`/`pending_delivery`" here named
    files the Q6 promotion path merely lives in — the Files list's own claim that
    "promotion executes inside the Slice 4 fenced write path" was not yet true of the
    `pending_delivery` insert itself (only `SmPersistenceStorage` was actually fenced;
    the promotion write into `pending_delivery` remained unfenced, exactly the gap FIX
    3's own trigger names). `sm_promotion`/`pending_delivery` are now genuinely
    claim-fenced (`PendingDeliveryStorage::insert_fenced`, threaded from
    `sm_promotion.rs`'s `PromotionContext::origin_stream_id`) — this parenthetical's
    citation is accurate as of FIX 3, not merely aspirational. The deposed-owner-with-live-socket scenario is structurally
    a steal-**intent** veto-path scenario (Slice 3), and steal-intents never apply to
    `SmSession` claims at all (Slice 3 rule 1) — so it cannot be reconstructed against a
    genuinely-claimed SM session either; it requires a genuinely-claimed `UserActor`,
    full stop. Fabricating `UserActor` claim-acquisition wiring here, just to unblock
    this one harness scenario, would pull forward a design decision (where in
    `UserActor`'s lifecycle a claim is acquired/released, how demotion interacts with
    actor supervision) that a later slice should make deliberately — the same
    "don't pull work forward" principle this plan already applies to the Slice 10
    drain boundary. Carried forward again, to land alongside whichever slice first wires
    `UserActor` claim acquisition. **Binding (FIX 6a, council-adjudicated)**: that slice
    does not exist within this phase's own slice breakdown (Slices 0-11 above) — it is
    **Phase 4's first slice that wires `UserActor` Postgres claims**, named explicitly as
    its own entry in "Carried risks / deferred to Phase 4," below, rather than left to
    dangle on a slice number this phase never defines. Slice 7's `RoomActor`-variant
    binding (this same scenario, one type over) is unaffected and stays exactly as
    written in Slice 7's own text above — that binding already lands within this phase.
35. **`pending_delivery`/`sm_unacked` element-5 schema additions land nullable and
    unpopulated this slice** — `origin_stream_id`, `inbound_seq` (the recipient-scoped
    dedup key, `(recipient/stream_id, origin_stream_id, inbound_seq)`, enforced by a new
    partial `UNIQUE` index), and `pair_sequence` (the per-`(origin_stream_id,
    recipient)` ordering counter). Schema only: no insert call site populates them yet.
    Populating them requires threading the origin session's SM-ID/`h` value through
    every `pending_delivery` insert path (Q6 promotion, direct offline-DM fallback, MUC
    reflection fan-out) and building the per-pair ordering/gap-detection consumer that
    reads them — machinery whose actual consumer (cross-node sticky-failover diversion)
    is exactly what this phase's own Non-goals exclude (no cross-node stanza routing
    GA; the cross-node janitor-flush leg is deferred to Phase 4). Landing the columns
    now avoids a second migration once that machinery lands. For the same reason,
    `pending_delivery/flush.rs`'s sweep-flush is **not** claim-scoped to owned bare JIDs
    this slice: that scoping is a `UserActor`-claim concern (see deviation 34) which
    does not exist yet either. The same-node guaranteed-flush test (element 9/element 5,
    Tests list) is satisfied by the EXISTING `pending_delivery` per-row
    claim/flush mechanism (`flushed_in_session`), which already routes through the
    `UserActor`'s full delivery surface on the owning node — this slice adds no new
    behavior there, since bare-JID ownership scoping has no `ClaimStore`-backed concept
    to scope to yet.
36. **Graceful drain is untouched this slice, confirmed against the plan's own Files
    lists**: Slice 5's Files/Tests lists name `session_registry`, the SM-expiry janitor,
    the new orphan reaper, `sm_promotion`/`pending.rs`, and `pending_delivery/flush.rs`
    — none of `session_janitors.rs::spawn_graceful_shutdown_drain`, `server/mod.rs`'s
    drain sequencing, or `ClaimStore::release_many` batching. Slice 10's own Files list
    ("Dependencies: Slice 5... for 'final fenced writes' to mean anything") is the
    slice that actually wires per-entity release-after-final-write into the drain path.
    Slice 5 contributes only what already falls out of the claim-lifecycle work above:
    a session's normal end-of-life paths (`confirm_drained`, `take_session`, etc.)
    already release their `ClaimStore` claim as part of deviation 29's fix, so Slice
    10's batching has a correct, already-tested single-entity release to batch — but no
    drain-specific sequencing/batching/observability is added here.
37. **`ClaimStore` gains `current_claim` (as-landed, Slice 6)**: a new, read-only,
    deliberately unlocked method — `async fn current_claim(&self, entity: &Entity) ->
    Result<Option<ClaimSnapshot>, ClaimError>` returning a new `ClaimSnapshot { owner:
    NodeIdentity, claim_epoch: ClaimEpoch }` — implemented identically to
    `ensure_claimed`'s own conflict-path read (same unlocked-SELECT shape, same "never
    itself an authority" caveat) on both `PostgresClaimStore` and `InProcessClaimStore`.
    Not named in the Slice 1 trait sketch: the cross-node resume path needs to (a)
    distinguish "no claim"/"self-owned" (today's local path, unchanged) from "owned by
    another node" (the new branch dispatch), and (b) learn the observed epoch to bind
    into a subsequent `steal_for_resume` call — no existing method exposes this without
    a mutating side effect.
38. **One-shot CAS discipline in `attempt_cross_node_resume` (as-landed, Slice 6,
    correcting an implementation-time design flaw caught by this slice's own dedicated
    tests)**: the new `InMemorySmSessionRegistry::attempt_cross_node_resume` (new file
    `waddle-xmpp/src/stream_management/session_registry/cross_node_resume.rs`) captures
    the claim epoch it observes exactly once, before its first ask, and every
    `steal_for_resume` call it issues binds that same original epoch — never a freshly
    re-read one, even across held-response retries. An earlier draft re-read
    `current_claim` on every retry, which — proven by this slice's own
    `pure_two_node_detached_steal_race_exactly_one_winner` and
    `reaper_wins_mid_resume_interleaving_resume_fails_cleanly` tests failing under that
    draft — let a resume attempt that lost a CAS re-observe the new (foreign) owner as
    its next "observed epoch" and legitimately re-steal the claim a second time later,
    contradicting the plan's own "the other observes a stale epoch and fails cleanly"
    (minor fix 22) and "fails cleanly ... rather than resuming a snapshot the reaper is
    concurrently touching" (major fix 11) language, both of which describe a single,
    terminal CAS attempt per resume call, not a retried one. Also fixed in the same
    pass: `InProcessClaimStore::steal_stale`/`steal_for_resume` previously ignored
    `observed` entirely (an unconditional overwrite, correct only in the sense that a
    true single node never contends with itself) — this could never lose a race, so it
    could not stand in for a two-node claim-steal simulation. Both methods now
    epoch-check exactly like the Postgres CAS, confirmed by new dedicated unit tests
    (`steal_stale_with_a_stale_observed_epoch_loses_the_race` et al.).
39. **`ClusteringResumeHandshakeConfig`/`WADDLE_CLUSTERING_RESUME_HANDSHAKE_TIMEOUT_MS`
    (as-landed, Slice 6)**: implements the plan's `min(remaining lease TTL,
    resume-handshake timeout)` held-response cap (element 8) as a flat, configured
    timeout validated at parse time to be `<= WADDLE_CLUSTERING_NODE_LEASE_TTL_MS` —
    rather than computing the `min(...)` per request against a live per-owner
    remaining-TTL read, which no existing `NodeLeaseStore` query serves. This makes the
    configured timeout an upper bound that can never exceed a *fresh* lease's remaining
    TTL; the only imprecision accepted is holding a response slightly longer than the
    *shrinking* remaining TTL of an owner whose lease is already partway expired —
    harmless, since the janitor-vs-resume ordering invariant (deviation 38) already
    guarantees a concurrent orphan-reaper steal is observed as a clean CAS loss, never a
    corrupted read. Threaded onto `ClusteringHandles` as
    `resume_handshake_timeout: Option<Duration>` (plus a `resume_handshake_timeout()`
    accessor, unconditionally compiled like `claim_pair`) so
    `stream_management.rs`'s route handler — outside the `clustering` feature gate —
    can read it without conditional compilation.
40. **Live-handshake force-detach: a new secondary per-connection control channel, not a
    repurposed `OutboundStanza`/`DeliveryKind` (as-landed, Slice 6)**: `ConnectionEntry`
    gains a small, dedicated `mpsc::Sender/Receiver<ForceDetachRequest>` pair (capacity
    8, not 1 — see that field's own doc comment for why a second concurrent ask must
    never block a `send` the connection's own select loop will never come back to read,
    which would otherwise wedge the single-threaded `RelayActor` mailbox handling it),
    plus a new best-effort reverse index on `ConnectionRegistry`
    (`sm_stream_owner`/`set_sm_stream_id`) from SM stream id to the `FullJid` currently
    publishing it. Chosen over extending `OutboundStanza`/`DeliveryKind` (which would
    have required a dummy `Stanza` value and touched the already-carefully-ordered main
    connection select loop's stanza-delivery pipeline) specifically to keep this
    addition isolated and auditable in the most race-sensitive file in the codebase.
    `RelayActor` gains a `resume_bridge: Arc<ResumeStealBridge>` field (new
    `waddle-server::clustering::resume_bridge` module) answering the new
    `RelayResumeSteal` ask by looking up the reverse index, re-verifying the hit against
    the entry's own authoritative `sm_stream_id()` (the index is best-effort, never
    proactively swept), and — on a match — sending a `ForceDetachRequest` through the
    connection's own channel and awaiting its ack (bounded by
    `LOOKUP_FORCE_DETACH_ACK_TIMEOUT`, independent of the asking node's own
    resume-handshake budget). `ResumeStealBridge` and the `RemoteResumeAsker` adapter
    over `RelayHandle` (`clustering::resume_asker::SwarmRemoteResumeAsker`) follow the
    same construction-order "wired after the fact" pattern `local_claims` already
    established (constructed empty at swarm-spawn time, before the `ConnectionRegistry`
    exists; completed by `server/http.rs::create_websocket_state`/
    `create_sm_session_registry`).
41. **`RelayHandle` cancellation-safety paydown lands exactly as the coordinator ruling
    specified (as-landed, Slice 6)**: `RelayHandle::new` gains a `stop_token:
    CancellationToken` constructor parameter; every public ask method
    (`ping`/`crash`/`sleep`/`echo_stanza`, plus the new `resume_steal`) is now a thin
    wrapper racing its own `_inner` implementation against `stop_token.cancelled()` in a
    `biased` `select!`, returning the new `RelayAskError::Cancelled` variant on a lost
    race — mirroring `spawn_supervised`'s identical pattern. `ClusteringHandles` gains a
    `stop_token: Option<CancellationToken>` field (the same clustering-scope child token
    `spawn_supervised` already receives) so `server/http.rs` can construct
    `SwarmRemoteResumeAsker` with the correct scope. All seven pre-existing
    `RelayHandle::new` call sites (one production-adjacent smoke test in `swarm.rs`, six
    in `clustering_cluster_e2e.rs`) updated to pass a token; this slice's
    `RelayResumeSteal` ask is `RelayHandle`'s first genuine production (non-harness)
    caller, exactly the trigger the pre-Slice-6 doc comment named as "necessary the
    moment a caller awaits these methods from a task that also watches this node's
    clustering stop token."
42. **`<enable/>`-time claim creation lands as `SmSessionRegistry::ensure_session_claim`
    (as-landed, Slice 6)**, a thin wrapper around `ClaimStore::ensure_claimed` called
    from `handle_sm_enable` immediately after minting the stream id, best-effort and
    logged exactly like the existing detach-time
    `acquire_claim_store_entry_for_detach` (never fails the `<enable/>` handshake
    itself). No stream-shard lock is taken for this call (unlike every other
    `ClaimStore` call site under `CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT`): a freshly
    minted UUID stream id cannot yet appear in `sessions`/`claimed_sessions`, so there is
    nothing else to coordinate with under that lock at this point in the session's
    lifecycle.

### Council-adjudicated corrigenda to the Slice 6 implementation (second review pass)

A second adversarial review of the as-landed Slice 6 code (deviations 37–42 above)
found nine further issues, all fixed in the same pass; each is recorded below in the
same "as-landed" style, numbered onward from 43.

43. **`attempt_cross_node_resume`'s held-response retry loop bounds every internal
    await by the shrinking budget, not just the overall deadline check (high-severity
    fix, `cross_node_resume.rs`)**: the previous implementation checked
    `Instant::now() >= deadline` only between iterations, so a single slow step inside
    one iteration — most importantly `RemoteResumeAsker::ask_remote_detach`, whose
    underlying `RelayHandle` ask carries its own `mailbox_timeout`/`reply_timeout` plus
    a stale-ref refresh-and-retry-once (up to roughly double those timeouts) — could
    make one iteration alone run well past `handshake_budget`, defeating the bound the
    whole held-response window exists to guarantee. Every await in the loop
    (`load_persisted_snapshot`, the terminal `steal_for_resume` + `hydrate_reclaimed` +
    `claim_session` sequence, and `ask_remote_detach` itself) is now wrapped in
    `tokio::time::timeout` against the budget remaining at the moment that specific
    await begins, recomputed fresh each time. A timeout on `ask_remote_detach` is
    treated identically to `RemoteResumeAskOutcome::Unreachable` (falls through to the
    existing backoff-and-retry). A timeout on the terminal CAS sequence is deliberately
    **not** retried and **not** guessed at (it does not resolve to `NotFound` or
    `OwnerUnreachable`): the outcome is genuinely ambiguous (the CAS may have committed
    server-side with the reply lost to the budget), so it surfaces as
    `SmRegistryError::Internal` — consistent with the one-shot-CAS discipline (deviation
    38) that a lost/ambiguous CAS is never retried. Proven by a new paused-clock unit
    test (`cross_node_resume.rs`'s own `tests` module,
    `fix1_slow_asker_never_holds_past_the_handshake_budget`) using a `SlowAsker` double
    with an hour-long delay against a two-second budget.
44. **FIX 8(b)'s recipient-claim-move retry test covers the Phase-3-reachable half
    only, per this plan's own Non-goals**: "pending_delivery rows move with the claim"
    is trivially true in the dedicated test suite's two-registry simulation (both
    simulated nodes already share one Postgres `pending_delivery` table, exactly like
    every other cross-node simulation in this suite — there is no second, node-local
    copy to move). The genuinely new, Phase-3-reachable thing this council round's test
    (`recipient_claim_move_retry_dedups_pending_delivery_delete`,
    `xep0198_cross_node_resume.rs`) proves is that the SM-ack-keyed delete path
    (`PendingDeliveryStorage::delete_acked_through`) is idempotent under retry after a
    cross-node steal has moved the SM claim: a retried ack for the same `h` (the
    client's `<a h='N'/>` re-arriving, or the recovering session's own at-least-once
    redelivery) dedups to a clean zero-rows-removed no-op rather than erroring or
    double-processing. Reconstructing the flush itself via genuine cross-node stanza
    ROUTING (a live message arriving at node A, offline-queued, then delivered after a
    resume moves the recipient to node B) requires the routed cross-node stanza
    delivery this phase's own Non-goals assign to Phase 4 — that remainder is deferred,
    not silently dropped.
45. **FIX 8(c)'s "deferred-h/handoff coupling across the steal" scenario is inherited,
    unchanged single-node machinery — no new cross-node-specific code exists to test,
    so none was invented**: investigation traced the phase plan's "`<r/>` answered
    immediately with `h` excluding unresolved handoffs" text to the pre-existing
    `claimed_sessions`-writable-during-handoff contract
    (`session_registry/tests.rs::test_claimed_session_remains_writable_for_handoff_fanout`:
    `record_stanza_for_detached_resource` merges fanout into a session's unacked queue
    while it sits in `claimed_sessions`, between `claim_session` and `complete_claim`).
    `attempt_cross_node_resume`'s branch 1 reaches this exact same `claimed_sessions`
    state via the exact same `hydrate_reclaimed` → `claim_session` pair the local-only
    resume path already uses — the resulting entry is indistinguishable from a purely
    local claim, and no cross-node-specific `h`/handoff-coupling logic was ever added or
    needed. Per this plan's own instruction not to silently test nothing, a new test
    (`handoff_fanout_survives_a_cross_node_steal_unchanged`,
    `xep0198_cross_node_resume.rs`) empirically proves the existing mechanism carries
    over unchanged through an actual cross-node steal, rather than resting on code
    inspection alone.
46. **`RelayResumeSteal`'s handler delegates its reply via kameo 0.20's
    `Context::spawn`, rather than awaiting the local force-detach inline (high-severity
    fix, `relay.rs`)**: kameo processes one actor's mailbox strictly sequentially, so
    the previous `Message<RelayResumeSteal>::handle`'s inline
    `resume_bridge.request_forced_detach(..).await` — bounded by
    `LOCAL_FORCE_DETACH_ACK_TIMEOUT` (10s) — head-of-line-blocked every OTHER message
    this node's relay answers (`RelayPing`, `RelayEchoStanza`, any concurrent
    `RelayResumeSteal` for an unrelated session) for up to that same 10 seconds per ask.
    `Message<RelayResumeSteal>::Reply` is now `kameo::reply::DelegatedReply<RelayResumeStealReply>`,
    and `handle` calls `ctx.spawn(async move { .. })` to run the force-detach wait on a
    detached `tokio::spawn`ed task, returning control to the actor's own message loop
    immediately. This is kameo 0.20's own documented delegation mechanism (see
    `kameo::message::Context::{reply_sender, spawn}`'s doc comments/examples) and
    requires **no second actor and no second kademlia registration** — the
    O(1)-registrations-per-node claim this module's own doc comment makes is unaffected.
    Proven by a new local-actor test (`relay.rs`'s own `tests` module,
    `slow_force_detach_does_not_delay_a_concurrent_relay_ping`) that registers a
    connection whose force-detach ack is deliberately delayed, fires a `RelayResumeSteal`
    ask on a background task, and asserts a concurrent `RelayPing` on the same actor
    resolves in well under a second regardless; confirmed as a genuine regression guard
    by re-running it against the pre-fix inline-await code, where it fails (times out).
47. **`attempt_cross_node_resume` is now raced against this node's graceful-shutdown
    token at its call site (medium-severity fix, `stream_management.rs::handle_sm_resume`);
    corrected below in deviation 55 — this entry's original "no committed steal can be
    lost" framing was false as shipped.** The connection's own `tokio::select!` loop
    (`connection.rs`) cannot observe its `shutdown_token.cancelled()` arm while a selected
    arm's body — including a nested `handle_sm_resume` call — is still executing an
    `.await` chain, so a held cross-node resume attempt (deviation 43's now-bounded, but
    still potentially multi-second, budget) delayed this connection's own graceful
    -shutdown handling by up to that same budget. `handle_sm_resume` originally raced the
    whole `attempt_cross_node_resume` call against `state.deps.shutdown.stop_token().cancelled()`
    in a `biased` `tokio::select!` (a new local `CrossNodeAttemptOutcome::ShutdownAbandoned`
    variant marks the losing case), reasoning that on cancellation `tokio::select!` drops
    the losing future rather than polling it to completion, so no `steal_for_resume` the
    call issued could have committed after that point.
    **That reasoning only covers the CAS call itself — it does not cover the sequence
    after it.** `tokio::select!` can just as easily drop the future *between*
    `steal_for_resume` committing in Postgres and the subsequent `hydrate_reclaimed`/
    `claim_session` completing, leaving a self-owned, un-hydrated claim under a FRESH
    lease that the orphan reaper's `OwnerStale` predicate can never fire against (the
    "owner" is this very node, alive and heartbeating) — wedging resume for that session
    until this node's own lease naturally lapses or it self-fences. Deviation 55 below is
    the as-built correction: only the read-only pre-CAS phase is ever raced; the CAS →
    hydrate → claim sequence, once entered, always runs to completion. The original
    Postgres-gated regression test
    (`tests/stream_management.rs::fix3_shutdown_race::shutdown_mid_resume_abandons_the_attempt_without_waiting_out_the_budget`,
    using a `HangingAsker` that never resolves and a deliberately generous 30-second
    budget) still passes unmodified under the corrected design, because a `HangingAsker`
    never lets the attempt reach the CAS at all — it stays entirely in the now-still-raced
    pre-CAS phase, exactly proving deviation 55's cancellation boundary is drawn in the
    right place.
48. **The cross-node resume force-detach ack is now a typed outcome reflecting whether
    a snapshot was actually persisted, not an unconditional `Detached` (medium-severity
    fix, `cleanup.rs`/`connection.rs`/`outbound.rs`/`resume_bridge.rs`)**: previously,
    `connection.rs` sent `ForceDetachOutcome::Detached` to the asking node after
    `cleanup_connection_shutdown` ran, regardless of what that call actually did —
    including its several early-return paths (no cleanup-eligible JID, no registry
    ownership, non-owned entry, an ownership-race that promoted the queue instead of
    storing it) and its `store_session` storage-error fallback, none of which leave a
    persisted, resumable snapshot behind. `cleanup_connection_shutdown` now returns a
    new `pub(super) enum ConnectionShutdownOutcome { Detached, NotPersisted }`
    (`#[must_use]`), and `ForceDetachOutcome` gains a matching `NotPersisted` variant;
    `connection.rs` maps the two 1:1 before acking. `ResumeStealBridge::request_forced_detach`
    (`resume_bridge.rs`) maps `NotPersisted` the same as `NotLiveLocally` (re-check
    persistence and retry) — the asker never proceeds with `steal_for_resume` against a
    snapshot that was never actually written.
49. **`ResumeStealBridge::request_forced_detach`'s channel send is now `try_send`, not
    a blocking `send().await` (medium-severity fix, `resume_bridge.rs`/`outbound.rs`)**:
    a full or already-closed force-detach channel now answers `NotLiveLocally`
    immediately rather than awaiting capacity that may never free up, keeping this
    call's own budget meaningful (a blocking send had no bound of its own). This is
    belt-and-suspenders alongside, not a replacement for, deviation 46's mailbox fix —
    `ConnectionEntry`'s own doc comment on `force_detach_tx`/`FORCE_DETACH_CHANNEL_CAPACITY`
    is corrected to describe the capacity as belt-and-suspenders rather than the
    load-bearing wedge-avoidance mechanism the original design intended.
50. **The owner-unreachable held-response window now distinguishes its two terminal
    conditions instead of collapsing them onto one (medium-severity conformance fix,
    `ownership/mod.rs`/`in_process.rs`/`clustering/claims.rs`/`cross_node_resume.rs`)**:
    `ClaimSnapshot` gains a new `owner_lease_fresh: bool` field — for
    `PostgresClaimStore::current_claim`, an unlocked, advisory read of the exact same
    `NOT EXISTS`-over-`clustering_nodes` owner-stale predicate `steal_stale`'s
    `OwnerStale` arm already uses (committed `expired` flag only, never a raw heartbeat
    comparison); `InProcessClaimStore` always reports `true` (no node-liveness table in
    the single-node case, matching that store's existing simplifications elsewhere).
    `attempt_cross_node_resume` gains `classify_window_expiry`, called at every point the
    held-response window expires: `current_claim` is re-checked once (never retried
    further) and the claim being altogether gone, OR present but `owner_lease_fresh ==
    false`, now answers `<failed/>` `item-not-found` (the session is known gone) — only a
    present claim with a still-fresh owner lease answers `<failed/>` `resource-constraint`
    (genuinely unreachable, not gone) as before. Proven by a new in-process unit test
    (`cross_node_resume.rs`'s `tests` module,
    `fix6_window_expiry_with_claim_gone_reports_not_found`) and two Postgres-gated tests
    (`xep0198_cross_node_resume.rs`: the pre-existing
    `owner_unreachable_past_the_handshake_window_fails_with_owner_unreachable` now
    explicitly registers the owner's `clustering_nodes` liveness row so it still
    exercises the "genuinely fresh, just unreachable" case rather than accidentally
    falling into the new lease-expired case for lack of any liveness row at all; the new
    `owner_lease_expired_past_the_handshake_window_fails_with_not_found` commits that
    same row `expired = true` directly and asserts the new `NotFound` outcome).
51. **`SmSessionId` enforces a 128-byte wire bound at deserialization (medium-severity
    conformance fix, `pending_delivery/mod.rs`)**: `RelayResumeSteal` (the relay message
    carrying this type across the wire) is not a stanza, so it never passes through the
    bounded XML stanza codec (`clustering::codec`'s `MAX_REMOTE_XML_BYTES` et al.) —
    every non-stanza relay message needs its own field-level bound rather than relying
    on the stanza codec's, and this one had none, letting a malicious (or buggy)
    allowlisted peer ship an arbitrarily large id. `SmSessionId`'s `Deserialize` impl is
    now hand-written (no longer derived) and rejects any value over
    `SM_SESSION_ID_MAX_LEN` (128 bytes — production stream ids are 36-byte
    server-generated UUIDs) with a new typed `SmSessionIdTooLong` error.
    [`Self::new`] itself stays unvalidated: every production caller mints
    server-generated UUIDs, and the wire boundary — not every internal call site — is
    where an untrusted length actually arrives. Non-stanza relay messages in general
    carry their own field-level bounds rather than inheriting the stanza codec's; this
    is the first (and, as of this phase, only) such field.
52. **Testing-methodology tradeoff, recorded rather than left implicit: the "pure
    two-node detached-steal race" test drives the bare `ClaimStore::steal_for_resume`
    CAS directly, not two concurrent `attempt_cross_node_resume` calls**
    (`xep0198_cross_node_resume.rs::pure_two_node_detached_steal_race_exactly_one_winner`,
    already landed prior to this council round, documented here per this round's own
    "record every real tradeoff, don't leave it only in a code comment" instruction):
    the full `attempt_cross_node_resume` orchestration has several sequential `.await`
    points before its own CAS (the `current_claim` read, the persistence read), so two
    independently-scheduled top-level calls are not guaranteed by `tokio::join!` alone
    to observe the SAME pre-race epoch — a real two-node race requires the SAME
    `observed_epoch` bound into both CAS attempts to be a meaningful proof of "both nodes
    race against the same detached claim." The test instead captures `observed_epoch`
    once and races the bare CAS with it directly, then separately verifies the winner's
    full hydrate/`claim_session` pipeline using the same public API. The genuinely
    two-concurrent-top-level-call scenario (a client retransmit/second-connection race
    against the SAME resuming node) is covered by this council round's own addition,
    deviation 44's sibling test
    `resume_retransmit_race_against_the_same_new_node_dedups_without_double_hydration`.
53. **Harness clarification: `WADDLE_XMPP_SM_DATABASE_URL` must be set to the SAME
    shared Postgres URL as `WADDLE_DATABASE_URL` in the cross-node live-steal handshake
    harness (`clustering_cluster_e2e.rs::spawn_cluster_server`), documented here because
    the plan text never previously named this requirement explicitly**: `TestServer`'s
    own default SM-persistence URL is `sqlite::memory:`, a non-Postgres URL that
    `open_for_cluster_mode`'s `postgres://`/`postgresql://` filter silently treats as
    "not set," falling back to the portable, per-process, NON-shared SM store — which
    would defeat `cross_node_resume_live_steal_handshake` entirely (both harness nodes
    must read/write the SAME persisted `sm_sessions`/`sm_unacked` rows for a cross-node
    steal to be observable at all). The harness as-landed already sets
    `WADDLE_XMPP_SM_DATABASE_URL` to the shared `postgres_url` for exactly this reason;
    this deviation records that requirement in the plan text itself rather than leaving
    it discoverable only via the harness's own inline comment.
54. **Trust-model precision (council-adjudicated): the cross-node resume identity
    binding protects against malicious/buggy CLIENTS and honest-node mistakes only — it
    does not, and cannot, protect against a compromised allowlisted NODE.** A sentence
    to this effect is added both here and to `resume_bridge.rs`'s own module doc
    comment. `verify_resume_identity`'s bare-JID match (element 8) proves that the
    SASL-authenticated identity presented on THIS connection matches the snapshot's
    bound JID — a real defense against a client attempting to resume, or force a
    force-detach of, a session that is not its own, and against an honest node's own
    bug accidentally mismatching JIDs. It says nothing about whether the `requester_bare_jid`
    a REMOTE node places on the wire in a `RelayResumeSteal` ask is the JID that remote
    node's own client actually authenticated as — a compromised allowlisted node could
    forge that field arbitrarily and this mechanism would not detect it. This is fully
    consistent with, not a gap introduced by, this ADR's enrolled-node-fully-trusted
    cluster membership model (allowlisted nodes are trusted peers by construction,
    exactly as every other cross-node ask in this phase — `RelayPing`, `RelayEchoStanza`
    — already assumes); it is called out explicitly here so a future reader does not
    mistake the identity-check mechanism for a defense it was never designed to provide.
55. **`attempt_cross_node_resume` is split at its write boundary, and a post-win repair
    path plus a matching unclaimed-but-persisted resume branch are added (high-severity
    fix, correcting deviation 47 — `cross_node_resume.rs`, `stream_management.rs::handle_sm_resume`)**:
    a second adversarial convergence pass (after the deviation 37–46 round) found that
    deviation 47's shutdown race was unsound past the CAS itself, and traced a second,
    related hazard with no cancellation involved at all.
    - **The defect**: `handle_sm_resume` raced the ENTIRE `attempt_cross_node_resume` call
      — including the terminal `steal_for_resume` → `hydrate_reclaimed` → `claim_session`
      sequence — against the shutdown token. `tokio::select!` can drop that future between
      `steal_for_resume` committing in Postgres and `hydrate_reclaimed`/`claim_session`
      completing, leaving a self-owned, un-hydrated claim that wedges resume for that
      session until this node's lease goes stale and the orphan reaper reclaims it
      (deviation 33's sweep interval plus deviation 26's fencing headroom — tens of
      seconds in practice).
    - **The sibling** (same class, found while tracing the defect): FIX 1's per-step
      timeout (deviation 43) wraps the terminal CAS+hydrate+claim sequence — a timeout
      firing AFTER the CAS commits but during hydration drops the future identically, with
      no shutdown involved at all. Worse, ANY ordinary `hydrate_reclaimed`/`claim_session`
      failure after the CAS has won (a storage read error, a lost self-reacquire, any of
      `hydrate_reclaimed`'s several best-effort skip-and-continue paths) leaves the claim
      held by this live node under a FRESH lease — and `steal_stale`'s `OwnerStale`
      predicate can never fire against a live, heartbeating owner, so the orphan reaper
      that rescues the cancellation case never rescues this one. Combined with
      `current_claim` short-circuiting every subsequent client retry straight to `NotFound`
      (the pre-fix `attempt_cross_node_resume` had no branch for "claim absent, but a
      persisted snapshot exists anyway"), this was a genuinely **permanent** wedge — worse
      than the cancellation case, which at least self-heals via the reaper.
    - **The fix, three parts**:
      - **Cancellation boundary**: `attempt_cross_node_resume` is split into
        `prepare_cross_node_resume` (read-only: the initial claim snapshot read, the
        identity check, and the branch-2/3 ask/hold/backoff loop — returns a new
        `CrossNodeResumeStage`, either a terminal outcome or a `StealTicket`) and
        `finish_cross_node_steal` (the write: the CAS or, for the new branch below, a
        plain `ensure_claimed`, followed by `hydrate_reclaimed` and `claim_session`).
        `handle_sm_resume` races only `prepare_cross_node_resume` against the shutdown
        token; `finish_cross_node_steal` is always awaited to completion once reached,
        never wrapped in a timeout that could drop it. Every internal step inside
        `finish_cross_node_steal` still carries its own fixed, independent bound
        (`FINISH_STEAL_TIMEOUT`/`FINISH_HYDRATE_TIMEOUT`/`FINISH_CLAIM_TIMEOUT` —
        deliberately NOT derived from the caller's `handshake_budget`, so an exhausted
        budget at the moment of winning the CAS no longer aborts the sequence); a bound
        expiring routes to the repair below rather than surfacing as a bare dropped
        future.
      - **Post-win repair**: once the CAS (or the new direct-acquire) has won, any
        subsequent `hydrate_reclaimed`/`claim_session` failure — error OR an internal
        bound expiring — is repaired by `repair_failed_local_claim`: an epoch-gated
        `ClaimStore::release` of the claim just won, retried up to
        `REPAIR_RELEASE_MAX_ATTEMPTS` times with a short delay between attempts. The
        release is the PRIMARY recovery path, not a belt-and-suspenders fallback — a live
        node's own fresh lease means the orphan reaper's staleness predicate will never
        fire for it, so there is no other mechanism that would ever reclaim this entity if
        every release attempt fails; that terminal case is a loud `error!` log plus a
        typed `Err` naming both the original failure and the exhausted repair, so an
        operator sees the full picture in one message. On successful repair the call
        returns `Ok(NotFound)` (not an error), since the claim is once again
        unclaimed-but-persisted — exactly the state the next fix makes recoverable.
      - **The unclaimed-but-persisted branch**: `prepare_cross_node_resume` no longer
        treats "no `ClaimStore` claim on this entity" as automatically terminal. If a
        persisted snapshot exists anyway (this repair path's own release, or the Slice 5
        fail-open-detach gap noted in deviation 34), it verifies identity against the
        snapshot and hands back a `StealTicket` in `DirectAcquire` mode — no CAS, no
        witness, just a self-reacquire-idempotent `ensure_claimed` — then hydrates and
        self-claims exactly like the steal path, sharing the same post-win repair. Without
        this branch, a successfully-repaired claim would still dead-end every subsequent
        client retry at `NotFound` forever, since nothing else ever creates a fresh claim
        for an already-persisted, already-unclaimed entity.
    - **Tests** (all in `cross_node_resume.rs`'s existing in-process suite, `#[tokio::test(start_paused
      = true)]`, no Postgres needed — a paused clock makes every scenario exact and
      instant): `fix_a_budget_expiry_mid_finish_does_not_drop_the_sequence` proves the
      terminal sequence completes (`Claimed`) even when a persistence-read delay pushes
      virtual time 100x past the nominal `handshake_budget` mid-hydrate, closing exactly
      the gap deviation 47's own regression test could not exercise (a `HangingAsker`
      never reaches the CAS at all). `fix_b_post_win_hydrate_failure_repairs_and_fix_c_retry_succeeds`
      forces `hydrate_reclaimed`'s ordinary "already present" skip path immediately after
      a real CAS win, asserts the repair releases the claim (checked directly against the
      shared `ClaimStore`) and returns a clean `NotFound` rather than an error, then
      re-attempts the resume and asserts the retry actually recovers the persisted session
      (`Claimed`) via the new unclaimed-but-persisted branch — not merely "not wedged,"
      but genuinely successful. The existing FIX 3 shutdown-race test
      (`tests/stream_management.rs::fix3_shutdown_race`) and every other pre-existing test
      in this suite (`fix1`, `fix6`, and the full Postgres-gated `xep0198_cross_node_resume.rs`
      suite) pass unmodified against the split API, since `attempt_cross_node_resume`
      itself is kept as a thin `prepare` + `finish` wrapper for exactly this reason.
56. **Occupant-roster durability deferred out of Slice 7's landed scope
    (implementation-time scope cut, as-landed).** Element 7's prose names
    "occupant roster (real JID, nick, occupant-id)" alongside config/
    affiliations/subject as durable state the owning `RoomActor` persists.
    The locked spec quote this plan pins Slice 7 against, and the Tests
    paragraph, both name only configuration/affiliations/subject — never
    occupant roster. As landed, only those three are made durable
    (`clustering_muc_rooms`/`clustering_muc_room_affiliations`, see
    deviation 63). Occupant-roster persistence's sole consumer is a
    *different* node's occupants observing service-shutdown presence for
    occupants that node never itself saw join (element 7's "each
    occupant's local node synthesizes ... unavailable presence ... to
    every local occupant" bullet, and element 11's cross-node presence
    fan-out) — machinery this phase's Non-goals exclude (no cross-node
    stanza routing GA). Landing occupant-roster schema now, with no reader
    until that machinery exists, would be schema-only churn — the same
    "columns now, consumer later" tradeoff deviation 35 already accepts
    explicitly elsewhere in this plan, applied here as a deferral instead
    since even the schema shape depends on decisions (per-session vs.
    per-nick, how multi-resource occupancy is modeled durably) that
    Non-goals-excluded machinery should make deliberately. Deferred to
    whichever slice first wires cross-node presence fan-out.
57. **Scope correction:** the production pre-fanout path retains its
    mutable-cache `check_fenced_fanout` backstop. It is a live legacy
    consumer for groupchat dispatch/MAM until #1283 replaces it; #1355 does
    not make this general dispatch, archive, bot, system, or MAM path carry
    exact actor-incarnation fence authority.
58. **Scope correction:** MAM archive fencing and its transaction boundary
    are #1283 work, not a #1355 completion claim. The current MAM
    implementation performs its origin-ID dedup check inside the same fenced
    transaction as the archive write, not on the plain pool.
59. **`LocallyClaimedEntities::owned()` widened from a sync `fn` to an
    `async fn` (`self_fence.rs`).** Slice 5's `SmSessionLocalClaims::owned`
    reads a plain in-memory map (`InMemorySmSessionRegistry::
    live_session_ids`), which is why the trait method was sync. `RoomActor`
    ownership is bookkept inside `RoomRegistryActor`'s own actor state —
    reachable only through its mailbox (`RoomRegistry::list_rooms`, an
    actor ask) — so `RoomLocalClaims::owned` cannot implement a sync `fn`
    without either a second, actor-external cache (a new synchronization
    surface this plan's actor-model conventions avoid) or blocking on the
    ask (illegal inside a sync fn). Widening the trait method to `async fn`
    keeps the "local bookkeeping only, never a Postgres read" contract
    intact — the ask is still pure in-memory, just actor-mediated — and is
    a small, mechanical ripple: three call sites in `run_node_lease`
    (`self_fence.rs`) gained `.await`, and every existing implementor
    (`SmSessionLocalClaims`, `NoLocallyClaimedEntities`, the module's own
    `FakeLocalClaims` test double) was updated to match with no behavior
    change.
60. **`Entity`/`EntityType`/`ClaimEpoch` (`waddle-xmpp::ownership`) gained
    `Serialize`/`Deserialize` derives.** None of the three needed to cross
    a wire boundary before this slice. The Demote relay ask (element 7's
    two-part demotion protocol, part (a); deviation 62) carries
    `Entity`/`ClaimEpoch` as its typed payload, mirroring exactly how
    `RelayResumeSteal` (Slice 6) already carries `SmSessionId`/`BareJid`
    wire-typed rather than as bare strings — a derive-only addition, no
    behavior change to any existing `ClaimStore`/`EntityType` call site.
61. **RoomActor claim acquisition, and re-election from a dead owner via
    `steal_stale(OwnerStale)`, land in `RoomRegistryActor`'s
    `GetOrCreateRoom`/`CreateRoom`/`CreateInstantRoom` handlers — a new
    `RoomRegistryError::ClaimHeldByAnotherNode` variant covers the live-
    foreign-owner case.** `ClaimStore::ensure_claimed` runs before every
    spawn; on `AlreadyClaimed`, `current_claim` decides whether the
    existing owner's own node lease is stale — if so,
    `steal_stale(OwnerStale)` re-elects this node as owner (the concrete
    "re-election" the slice title promises for the dead-owner case); if the
    owner is genuinely live, `GetOrCreateRoom` refuses with
    `ClaimHeldByAnotherNode` rather than attempting any cross-node
    proxy/routing — cross-node MUC message/join proxying is the ADR's own
    text (element 7's first sentence) but is explicitly Phase 4 scope per
    this phase's Non-goals (no cross-node stanza routing GA). `RoomEntry`
    (a new private type wrapping `ActorRef<RoomActor>` plus the
    `ClaimEpoch` it was spawned/re-claimed under) replaces the bare
    `ActorRef` in `RoomRegistryActor.rooms` so `DestroyRoom` can release
    the exact epoch-scoped claim it holds (element 7's "graceful release").
    Steal-intent-based re-election (the wedged-but-not-dead-owner path) has
    no production reporter call site this phase, for the same "no
    cross-node proxy exists to report one" reason deviation 21 already
    names for the general steal-intent mechanism — the harness scenario
    (deviation 64) seeds the intent row directly, mirroring Slice 3's own
    dedicated tests.
62. **The Demote relay ask's receiving side required threading
    `Arc<RoomLocalClaims>` through `swarm::spawn`/`relay::spawn_supervised`/
    `RelayActor::new`, bundled into a new `swarm::RelayBridges` struct
    alongside `resume_bridge` (clippy `too_many_arguments` on `bring_up`
    otherwise).** `RelayActor` answers a received `Demote { entity,
    new_epoch }` ask by calling `room_local_claims.demote(&entity)`
    directly — the identical hard-kill discipline the reconciliation/
    veto path already uses, not a second demotion mechanism. `RoomLocalClaims`
    is constructed empty at `clustering::start_if_enabled` time (mirroring
    `local_claims`/`resume_bridge`'s exact fill-in-later-cell pattern: the
    MUC room registry does not exist yet at that point in startup) and
    wired later in `server/mod.rs` once the registry is spawned, via
    `RoomRegistry::wire_clustering_claims`'s sibling call
    `room_local_claims.wire(room_registry.clone())`. The Demote ask itself
    (`Demote`/`DemoteReply` in `clustering::relay`, plus `RelayHandle::
    demote`) mirrors `RelayResumeSteal`'s exact shape and cancellation-safe
    `RelayHandle` pattern (Slice 6) per the plan's own instruction; its
    reply is a bare `DemoteReply::Acked` (no multi-outcome enum like
    `RelayResumeStealReply`'s) since the recipient's local demote is
    unconditional/idempotent/infallible by contract — there is nothing for
    the asker to branch on, and the guaranteed correctness backstop is the
    fenced pre-fan-out check (deviation 57/58), never this ask's outcome.
63. **Confirmed reading: durable MUC room ownership is entirely
    clustering-gated, mirroring Slice 4's `SmPersistenceStorage` dispatcher
    — SQLite/single-node keeps today's purely in-memory `RoomActor`/
    `MucRoom` behavior byte-for-byte.** `RoomRegistryActor` always routes
    claim acquisition through `ClaimStore` (per Q1's "ordinary single-node
    code needs *some* `ClaimStore` in every build" rationale already
    established for other entity types) — defaulting to
    `InProcessClaimStore` + `NodeIdentity::local()`, which never denies a
    same-process `GetOrCreateRoom` and is exercised by every pre-existing
    `room_registry_actor` test unmodified — but the **durable** Postgres
    tables/restore/fenced-backstop/Demote-ask machinery is wired only when
    `clustering::start_if_enabled` produces a live claim pair (clustering
    enabled, Postgres, the `clustering` Cargo feature compiled in), exactly
    the same three-way gate `sm_persistence::open_for_cluster_mode`
    already uses. Schema (PROPOSED, no ADR-locked DDL exists — following
    the `clustering_` prefix + ensure-schema-on-the-store convention Slice
    1's `clustering_nodes`/`clustering_claims` already establish, since no
    proposed shape was given for Slice 7 specifically): `clustering_muc_rooms
    (room_jid PK, waddle_id, channel_id, config_json, subject_json,
    updated_at)` and `clustering_muc_room_affiliations (room_jid,
    member_jid, affiliation, reason, granted_at, PK(room_jid,
    member_jid))` — `RoomConfig`/`SubjectState` (already `Serialize`/
    `Deserialize`) JSON-encoded into `TEXT` columns; `Affiliation` (a
    closed five-variant enum) stored via a small `affiliation_to_db_str`/
    `affiliation_from_db_str` pair mirroring `EntityType::as_db_str`/
    `from_db_str`'s exact convention rather than a JSON blob.
64. **The deposed-owner-with-live-socket harness scenario's `RoomActor`
    variant lands in this slice (`tests/clustering_cluster_e2e.rs::
    deposed_owner_with_live_socket_room_actor_scenario`), per the binding
    this plan's own text already commits to (deviation 34: "the `RoomActor`
    variant of this same scenario is NOT carried [to Phase 4] — it already
    lands in Slice 7").** "Genuinely wedged" is produced by wiring a
    `MucDurableStore` fake whose `load_room_state` future never resolves:
    since `RestoreDurableRoomState` is the first message the room registry
    enqueues into a freshly spawned actor's FIFO mailbox (this slice's own
    restore-before-join ordering mechanism), the actor's mailbox loop is
    genuinely, durably stuck processing it — reusing this slice's own
    mechanism as the wedge point rather than inventing a synthetic one.
    "Genuinely claimed" is a real `PostgresClaimStore`-backed claim, not a
    fake. The steal-intent row is seeded directly via
    `NodeLeaseStore::report_steal_intent`, not through a production
    reporter call site — none exists this phase (deviation 61's own note),
    mirroring Slice 3's own dedicated tests' identical shortcut. The test
    asserts the wedged `RoomActor` is hard-killed (`ActorRef::is_alive() ==
    false`) within `RoomLocalClaims::health_check`'s own bounded ask window
    (`ROOM_HEALTH_CHECK_TIMEOUT`, 5s) plus one heartbeat interval of
    scheduling slack — not literally "one heartbeat interval" wall-clock,
    since the veto scan's health-check-fails branch is dominated by that
    per-ask timeout, not by how often the scan itself runs.

### Council-adjudicated FIX 1–9 pass over the landed Slice 7 implementation

The following deviations (65–73) record a second council-adjudicated review
pass over Slice 7 as landed (deviations 56–64 above). Deviation 65's former
MAM/archive claims are explicitly out of scope for #1355; deviations 66–73
record the retained work.

65. **Scope correction:** the former exact-fence claims for MAM archive,
    `ArchiveGroupchat`, system messages, bots, and general dispatch were
    removed from #1355. Those effects continue to use the published cache's
    legacy pre-fanout backstop until #1283 supplies their replacement. This
    ADR makes no completion claim for that future work. The retained backstop
    still fails closed on definitive local non-serving state: a missing
    published cache fence, a missing exact database tuple, or local identity
    rotation returns `Ok(false)`, while backend uncertainty remains typed as
    an error for the documented legacy fail-open path. Dispatch handles
    `Ok(false)` with `DemoteRoomIfExactActor` against the actor it already
    snapshotted, so a same-JID successor published while the database check is
    in flight cannot be evicted by the stale dispatch. The concurrent
    regression holds two old-actor dispatches at this boundary, publishes a
    successor, then proves both messages bounce with no archive or occupant
    reflection and the successor remains registered.
66. **FIX 2 (critical) — every durable-relevant `RoomActor` mutation
    handler now runs a two-stage gate: verify ownership BEFORE mutating,
    surface a non-ownership persist failure typed AFTER mutating.**
    `RoomActor::gate_mutation()` runs `check_exact_claim_fence` with the
    immutable fence installed on that actor incarnation; `Ok(true)` (or no
    durable store configured) lets the mutation proceed unchanged,
    `Ok(false)` returns `NotOwner` BEFORE any in-memory mutation runs, and
    any backend uncertainty returns `OwnershipUnavailable` BEFORE mutation.
    `persist_config` and post-apply affiliation persistence return
    `Result<(), DurablePersistError>` instead of silently swallowing a
    failure. Subject persistence is intentionally before apply, so its typed
    result classifies failure before the `SubjectState` is installed.

    All nine listed handlers (`UpdateConfig`, `RollbackConfigIfRevision`,
    `UpdateGroupDmConfigByMember`, `SetSubject`, `ChangeAffiliation`,
    `ApplyAdminItems`, `ApplyAffiliationChange`,
    `EnforceMembersOnlyAffiliations`, `ReconcileChannelBackedRoom`) call
    `gate_mutation()` first and propagate `PersistFailed`/`NotOwner`
    typed. Three already returned a per-message `Result<_, E>`
    (`AdminApplyError` for `ApplyAdminItems`/`ApplyAffiliationChange`,
    `UpdateGroupDmConfigByMemberError` for `UpdateGroupDmConfigByMember`)
    and gained `NotOwner`/`PersistFailed` variants plus
    `From<RoomMutationError>`/`From<DurablePersistError>` impls so the
    handler bodies use plain `?`. `SetSubject` has a phase-specific
    `SetSubjectError` whose persistence failures are always pre-apply. The
    other five handlers (previously bare
    `u64`/`bool`/`()`/`Vec<(FullJid, Presence)>` replies) had their
    `Reply` type wrapped in `Result<_, RoomMutationError>`. Two batch
    handlers (`ApplyAdminItems`'s affiliation-set branch,
    `EnforceMembersOnlyAffiliations`) do NOT abort mid-loop on a persist
    failure (earlier items in the same batch have already mutated
    `self.room`, so aborting would silently drop the presence
    notifications describing already-applied kicks/bans) — they finish
    the batch, then return the first failure typed; this means the
    batch's presence-update payload is lost from the immediate reply in
    that rare path (a narrow, accepted tradeoff of the existing
    either/or `Result` reply shape, not something this fix redesigns
    further).

    Call-site coverage (council-adjudicated scope boundary, honestly
    recorded): every actor-side handler is gated. On the `waddle-server`
    call-site side, the subject-change interpreter now distinguishes the
    flattened Kameo handler errors: `NotOwner` exact-demotes only the actor
    that rejected the request, `OwnershipUnavailable` and actor-transport
    uncertainty bounce without demotion, and both cases suppress all later
    archive/inbox/fan-out effects in that dispatch batch. Subject persistence
    runs before installing the new `SubjectState`, so a
    `PersistFailedBeforeApply` result follows the same bounce-and-halt path
    without changing the local subject.

    ONE additional representative site
    (`admin/channels.rs::group_dm_rename_update_error`, the
    `UpdateGroupDmConfigByMember` rename path) is fully wired end-to-end:
    `NotOwner` and `OwnershipLostAfterApply` exact-demote the actor; the
    latter does not claim the mutation was never applied. The remaining
    call sites (including those for `ChangeAffiliation`
    across `admin/channels.rs`, `session_janitors.rs`,
    `group_dm_invite.rs`, `muc_admin.rs`; the `UpdateConfig`/
    `RollbackConfigIfRevision`/`EnforceMembersOnlyAffiliations`/
    `ApplyAdminItems`/`ApplyAffiliationChange` sites) route through their
    existing typed error mappers but do NOT additionally exact-demote at
    those sites. Wiring exact demotion into all remaining callers is
    follow-up hardening; actor-side fail-closed gating already prevents the
    stale mutation itself.

    Tests use a `FakeDurableStore` that controls
    `check_exact_claim_fence` and fenced-write results to prove
    `UpdateConfig`/`ChangeAffiliation`/`SetSubject` block on `NotOwner`
    with no in-memory mutation, `UpdateConfig` applies normally when
    owned, fails CLOSED with `OwnershipUnavailable` on a transient
    fencing error, surfaces `PersistFailed` typed while still applying
    in-memory, and seals the actor when ownership is lost during the fenced
    config write.
67. **FIX 3 (high) — orphaned claim on actor panic/death, and the
    corresponding health-check blind spot, are both closed.**
    `RoomRegistryActor::live_room` is now `async` (was sync): its
    dead-actor branch captures the entry's `ClaimEpoch` BEFORE
    `self.rooms.remove`, then calls the same `release_room_claim`
    `DestroyRoom`'s handler already uses — previously the dead-actor
    branch removed the entry with NO release at all, permanently
    orphaning the Postgres claim (the registry's own record of the epoch
    needed to release it was gone the instant `rooms.remove` ran). The
    six call sites (`GetRoom`, `GetOrCreateRoom`, `CreateInstantRoom`,
    `CreateRoom`, `RoomExists`, `ListRooms`) gained `.await`.

    `RoomLocalClaims::health_check` (`waddle-server::clustering::
    local_claims`) now reports UNHEALTHY, never healthy, when
    `registry.get_room` returns `Ok(None)` or `Err(_)` — previously both
    were treated as "nothing to report unhealthy about" (`true`), which
    meant a wedged/gone room's owner-veto entry could indefinitely block
    another node's legitimate steal-intent for an entity this process can
    no longer act on at all.

    Tests: `room_registry_actor::tests::ownership_claims_tests::
    dead_actor_detection_releases_the_orphaned_claim` (kill the actor,
    trigger `live_room` via `GetRoom`, assert the claim store shows the
    claim released AND that a different node identity can now acquire
    it); three new `local_claims::tests` cases prove `health_check` is
    unhealthy with no live actor, healthy for a live one, and that
    `demote` hard-kills the actor (polled via `ActorRef::is_alive()`,
    since `kill()`'s `abort_handle.abort()` takes effect at the task's
    next yield point, not synchronously).
68. **FIX 4 (high) — `RestoreDurableRoomState`'s genuine load failure now
    fails closed: joins are refused until a retry succeeds, never served
    against caller-supplied defaults.** New `RoomActor` field
    `restore_state: DurableRestoreState` (`Ready` | `Pending` |
    `OwnershipLost`, private, default `Ready`). A non-ownership backend
    failure from `RestoreDurableRoomState` (distinct from `Ok(None)`'s
    legitimate "brand new room" case) sets `Pending` instead of
    logging-and-continuing with caller-supplied initial config.
    `JoinWithAffiliation` calls `ensure_restored_before_join()` first:
    `Ready` is a no-op; `Pending` runs ONE bounded inline retry against the
    same store — success applies the restored state and flips back to
    `Ready`, letting THIS join proceed immediately; a repeated uncertain
    failure returns `RoomActorError::RestorePending` and leaves `Pending`
    for the next join attempt to retry again. A definitive
    `XmppError::OwnershipLost` from either the initial load or that retry
    instead sets terminal `OwnershipLost`, seals the actor, and returns
    `RoomSealed`: it never retries the stale fence. The registry does not
    publish that actor, enqueue retry work, or release the already-lost
    stale claim tuple. A local node-identity rotation is not proof that the
    database row moved: it remains typed as `OwnershipUnavailable`, so demand
    cleanup releases the old exact tuple and reclaimed cleanup retains that
    release responsibility across retries. New disco/join-error mapping in
    `handlers/presence/muc.rs`: `RestorePending` bounces `<error
    type='wait'><resource-constraint/></error>`, matching FIX 6's shape.

    `SetSubject` uses the phase-specific pre-mutation ownership gate before
    constructing, persisting, or applying the subject: definitive loss maps
    to `NotOwner`, uncertainty maps to `OwnershipUnavailable`, and a normal
    durable write failure — including a missing complete room parent before
    #1352 — maps to `PersistFailedBeforeApply`. This keeps its public failure
    contract pre-apply. Shared `DurablePersistError::PersistFailed`
    is deliberately phase-neutral because it is also wrapped by mediated
    invite grant and rollback errors whose public variants encode that their
    affiliation writes failed before applying memory.

    Test: `room_actor::tests::
    join_is_refused_while_restore_is_pending_then_succeeds_once_recovered_with_no_ban_lost`
    — a `FlakyThenRecoveringStore` fails the initial load and the first
    retry, then succeeds on the second retry with a durable state
    carrying one `Outcast` (ban) affiliation entry; asserts the first two
    join attempts are refused, the THIRD `load_room_state` call (the one
    that recovers) is the same join attempt that both admits normal
    senders and correctly enforces the restored ban — proving no
    ban-list loss across the two earlier failures, not merely "no
    longer wedged."
69. **FIX 5 (high) — `bootstrap_fresh_xmpp_topology`'s room-creation loop
    no longer aborts the entire seed (and, via its own caller's `?`, the
    `seed_spaces_admin_affiliations` step too) on the first
    already-claimed-elsewhere room.** `seed_initial_xmpp_topology`'s
    `GetOrCreateRoom` loop (`server/topology.rs`) now matches
    `RoomRegistryError::ClaimHeldByAnotherNode` specifically and
    `continue`s (logs at `info!`, treats it as "owned elsewhere, normal
    steady state" — exactly the condition every node after the first to
    seed topology hits for every already-seeded room) instead of
    propagating it via `?`; every other error still fails startup as
    before. The bookmarks-backfill loop later in the same function, and
    `seed_spaces_admin_affiliations` in the caller
    (`bootstrap_fresh_xmpp_topology`), both still run regardless.

    Test:
    `topology::tests::bootstrap_completes_the_rest_of_the_seed_when_one_room_is_claimed_elsewhere`
    pre-claims the "chat" managed channel's room under a foreign
    `NodeIdentity` (via a directly-wired `InProcessClaimStore`, no
    Postgres needed) before calling `bootstrap_fresh_xmpp_topology`,
    then asserts it returns `Ok(())`, a DIFFERENT channel's room
    ("github-actions") was still created, and the claimed-elsewhere
    room's claim is untouched (still the foreign node's).
70. **FIX 6 (high) — the MUC join path no longer silently drops joins
    with no presence reply at all.** `get_or_create_room_actor`
    (`server/routes/websocket/cleanup.rs`) now returns
    `Result<ActorRef<RoomActor>, RoomRegistryError>` instead of
    collapsing every failure (including `ClaimHeldByAnotherNode`) into
    `None` — every existing call site (all but one are test call sites
    using `.expect(...)`, which resolves identically against both
    `Option::expect`/`Result::expect`) needed no changes. The production
    join path (`handlers/presence/muc.rs::handle_muc_join_unlocked`)
    replaces its previous `let Some(actor) = ... else { return vec![] }`
    (a silent drop) with a match: `ClaimHeldByAnotherNode` bounces
    `<error type='wait'><resource-constraint/></error>` (this phase does
    not wire cross-node MUC proxying — Phase 4 — so this is the
    conformant, retry-able response); any other registry error bounces
    `<internal-server-error/>` typed rather than silently dropping. The
    join-error match's own previous catch-all (`warn!(...); return
    vec![]`) is likewise replaced with a typed `<internal-server-error/>`
    bounce — no remaining `RoomActorError`/`SendError` variant may
    silently drop a join, matching this fix's "no silent join drops"
    intent broadly, not just for the two originally-named cases.
    Test: the bounce logic is code-verified; the presence-error WIRE
    shape (`type='wait'` + `<resource-constraint/>`) is asserted over
    real processes in Slice 11's harness capstone (a foreign-owned-room
    join against a two-node cluster), where it proves strictly more than
    a doubles-based unit test could — carried there deliberately, not
    dropped.
71. **FIX 7 (medium) — backstop coverage traced, not additionally coded:
    every named fan-out site is already gated by FIX 2's mutation
    gating, with no independent fan-out found.** Kick/ban role-change
    presence (`admin/channels.rs`'s `run_kick`/
    `sync_private_kick_affiliation_revocation`/the admin-V2
    set-affiliation path, at the plan's named `~3607/3637/3822` sites)
    all read their presence-update payload (`applied.presence_updates`)
    from the `Ok(applied)` arm of an `ApplyAdminItems`/
    `ApplyAffiliationChange` ask — every `Err` path (including the new
    `NotOwner`) returns/propagates before that binding exists, so the
    fan-out cannot fire when the gated mutation was refused. The
    members-only-eviction presence loop
    (`handlers/iq/muc_owner_config.rs` `~262`) reads its `updates` from
    either `EnforceMembersOnlyAffiliations` (gated) or `EnforceMembersOnly`
    (NOT in FIX 2's nine-handler list — it only evicts occupants based on
    already-in-memory affiliations and never itself writes durable state,
    so it was correctly out of scope) via `?`, so the loop is
    unreachable on either ask's failure. No additional fenced-check helper
    was needed at any of the three named sites. The independent legacy
    pre-fanout cache check remains in production until #1283 replaces it.
72. **FIX 8 (medium) — `PostgresMucRoomStore::open` failing at
    `clustering::start_if_enabled` time is now FATAL to clustering
    startup, not logged-and-continue.** New
    `ClusteringError::MucDurableStoreInit(XmppError)` variant;
    `start_if_enabled`'s durable-store construction changed from
    `match ... { Ok(store) => Some(Arc::new(store)), Err(error) => {
    warn!(...); None } }` to `.map_err(ClusteringError::MucDurableStoreInit)?`.
    Rationale: claims live without this store means a `RoomActor`
    ownership move silently resets config/affiliations/subject to
    defaults on the next steal — an unprotected deposal, not a
    degraded-but-safe fallback — mirroring the co-location discipline
    `sm_persistence`/`pending_delivery`'s own `open_for_cluster_mode`
    already enforce at startup for their own durability stores. No
    dedicated test added (triggering a genuine `ensure_schema` failure
    against a live, correctly-permissioned throwaway Postgres is not
    practical without a deliberately broken/read-only role fixture);
    the type-level `?` propagation is the same level of verification
    the sibling `ClusteringError::NodeLease` fatal-startup path already
    relies on (also untested at this granularity).
73. **FIX 9 (medium) — `Entity::id` gained the same hand-written,
    length-bounded `Deserialize` `SmSessionId` already has.** New
    `ownership::ENTITY_ID_MAX_LEN = 128` (mirroring
    `SM_SESSION_ID_MAX_LEN`'s identical rationale: the MUC Demote relay
    ask's `Entity` payload is not a stanza, so it never passes through
    the bounded XML stanza codec) and `EntityIdTooLong` error type.
    `Entity`'s derived `Deserialize` is replaced with a hand-written impl
    that deserializes through a private `RawEntity` shadow struct (to
    reuse the derived `EntityType` deserialization) and rejects an
    over-length `id` with `EntityIdTooLong`; `Entity::new` itself stays
    unvalidated, matching `SmSessionId::new`'s identical "the wire
    boundary is where an untrusted length actually arrives" rationale.
    Three boundary tests mirror `SmSessionId`'s exactly (accepts exactly
    at the cap, rejects one byte over, rejects a malicious multi-KB
    value). This makes deviation 60's "`Entity`... gained
    `Serialize`/`Deserialize`... mirroring exactly how `RelayResumeSteal`
    already carries `SmSessionId`/`BareJid` wire-typed" parity claim
    actually true — previously `Entity::id` had no bound at all, unlike
    the `SmSessionId` it was compared to.
74. **Code-research correction (Slice 8 implementation, changes the
    premise): no SASL2 (XEP-0388, `urn:xmpp:sasl:2`) implementation exists
    anywhere in this codebase — not partial, not misplaced, genuinely
    absent.** Slice 8's own text flagged locating "the SASL2/ISR
    wire-handling call site" as "an open item to locate during Slice 8
    implementation, not a blocking gap" — that framing assumed a SASL2
    envelope already existed somewhere to be found. A full-repo grep
    (excluding `target/`/`xeps/`) for `sasl:2`, `Sasl2`, `SASL2`,
    `bind2`, `authenticate` turned up zero hits outside this slice's own
    new code; the only pre-existing auth surface is SASL1
    (`urn:ietf:params:xml:ns:xmpp-sasl`: PLAIN/SCRAM-SHA-256/OAUTHBEARER,
    `waddle-xmpp/src/auth/mod.rs` +
    `waddle-server/.../websocket/sasl.rs`). Building general XEP-0388
    (mechanism negotiation via an `<authentication/>` stream feature,
    `<challenge/>`/`<response/>` multi-round exchanges, Bind 2/CSI inline
    negotiation, channel binding) is a multi-day undertaking of its own
    and out of proportion to "wire up ISR" — see deviation 75 for the
    narrowed scope this plan actually ships.
75. **Deliberately narrow, ISR-only SASL2 surface, not general XEP-0388**
    (scope-narrowing deviation, forced by 74). This slice implements
    exactly one `<authenticate xmlns='urn:xmpp:sasl:2'
    mechanism='PLAIN'>` shape: an `<initial-response>` (SASL PLAIN,
    `\0<bare-jid>\0<ISR-token>` — the token stands in for the password,
    per XEP-0397's own "pinned mechanism" design) plus an inline
    `<inst-resume with-isr-token='true'>` wrapping a XEP-0198
    `<resume/>`. `PLAIN` is the only pinned mechanism this implementation
    supports (not the XEP's own recommended `HT-SHA-256-ENDP`, which this
    codebase has no implementation of at all — PLAIN is the only
    supported SASL1 mechanism whose wire shape carries a bare password
    field an opaque token can stand in for). `with-isr-token='false'`
    (traditional-credential ISR) and any `<authenticate/>` without an
    inline `<inst-resume/>` (general SASL2 login) are rejected at the
    parser layer (`waddle_xmpp::protocol::frame::parse_frame`'s new
    `"authenticate"` root, `ParseError::MalformedIsrAuthenticate`) before
    ever reaching a handler. `<success/>` still carries
    XEP-0388's `<authorization-identifier/>` for conformance, even though
    XEP-0397's own (SASL1-vintage) examples omit it. **Addendum
    (council-adjudicated, FIX 8c)**: XEP-0388 itself says implementations
    "SHOULD NOT" support PLAIN as a general SASL2 mechanism — this slice's
    use of PLAIN is not that; the "password" field this mechanism carries
    is never a real, reusable password, it is a single-use, 256-bit-entropy
    ISR token, generated fresh per `<isr-enable/>`, destroyed on first use
    (match or mismatch) and rotated on every successful resume, and the
    exchange only ever happens over an already-TLS-protected transport.
    None of PLAIN's usual risk profile (a long-lived, reusable, human
    -chosen secret transiting in the clear if TLS is absent) applies here.
    Separately, this slice deliberately does **not** advertise a general
    `urn:xmpp:sasl:2` authentication stream feature at all — only the
    narrow `<authenticate mechanism='PLAIN'>` + inline `<inst-resume/>`
    shape `waddle_xmpp::protocol::frame::parse_frame` recognizes is
    reachable. This is intentional, not an oversight: ISR's entire purpose
    is to let a fresh connection skip the ordinary SASL negotiation
    round-trip, so advertising a general SASL2 authentication capability
    this codebase doesn't otherwise implement would invite exactly the
    "build a second, incomplete auth stack" scope creep deviation 74's own
    finding warns against.
76. **Cross-node ISR resume: wired, not deferred (council-adjudicated FIX
    2 — retracts and replaces this deviation's original text)**. The
    original draft of this deviation recorded cross-node ISR resume as
    out of scope for this slice, collapsing a local-claim miss straight to
    the failed-token failure path. FIX 2 wires it: `isr_resume.rs`'s
    `Ok(None)` branch now calls the SAME cancellable
    `prepare_cross_node_resume`/`finish_cross_node_steal` machinery
    `handle_sm_resume` uses (Slice 6), via a new shared helper,
    `stream_management::attempt_cross_node_resume_raced` — extracted from
    `handle_sm_resume`'s own inline cancellation-safe race so both resume
    paths share one implementation of that invariant rather than risking a
    second, subtly different one. The consume's own epoch fence (bound to
    whichever node just won the cross-node steal) requires no change: it
    already binds to "whichever claim/epoch this call currently holds,"
    which is exactly the freshly-won claim after a successful steal. The
    phase plan's own Slice 8 "Dependencies" line ("Slice 6 (resume
    machinery ISR piggybacks on)") is now fully realized.
77. **`consume`'s token-row `SELECT` uses `FOR UPDATE`, not a bare read**
    (implementation-time finding via this slice's own concurrent
    -double-consume test, which failed under real parallel execution
    before this fix and passed after). The locked spec's `FOR SHARE`
    lock is on `clustering_claims` (the SM-session claim fence) —
    `FOR SHARE` is a *shared* lock and does not by itself serialize two
    concurrent `consume` calls against the same `clustering_isr_tokens`
    row (both could read the same not-yet-deleted row before either
    commits). `SELECT token, mechanism FROM clustering_isr_tokens WHERE
    sm_id = ? FOR UPDATE` closes this: the second concurrent transaction
    blocks until the first commits (or rolls back) and then observes the
    row's post-commit state. This does not touch the "never match the
    token in a SQL WHERE clause" ban — `FOR UPDATE` only changes locking
    semantics for the same `sm_id`-keyed predicate; the token itself is
    still compared only in Rust, via `subtle::ConstantTimeEq`.
78. **`IsrTokenStore` trait + `InMemoryIsrTokenStore`
    (`waddle-xmpp/src/isr/store.rs`) / `PostgresIsrTokenStore`
    (`waddle-server/src/clustering/isr.rs`)** executed exactly per Q8's
    revised decision, mirroring the `ClaimStore` split. `issue`/`consume`
    are the only two trait methods (no separate `validate`/`refresh`/
    `revoke` surface from the pre-Slice-8 `IsrTokenStore` struct — none
    of that surface had any conformant caller to serve).
79. **IQ-issuance retirement executed exactly per the inventory**:
    `handlers/iq/isr_token.rs` deleted; its `mod`/`use`/dispatch entries
    in `handlers/iq/mod.rs` deleted; `build_isr_token_error`/
    `build_isr_token_result`/`is_isr_token_request`/the old `IsrToken`
    struct and its `format!`-based XML helpers all deleted (not
    repurposed — nothing conformant needed their shape).
    `websocket_isr_token_request_returns_token`
    (`tests/xep0054_0049_0191_ws.rs`) replaced with
    `websocket_legacy_isr_token_request_iq_is_gone`, asserting the old
    `token-request` IQ now falls through to the unhandled-IQ catch-all
    (`feature-not-implemented`), matching this repo's actual catch-all
    condition rather than `service-unavailable`.
80. **Namespace corrected in place**: `waddle_xmpp::ns::ISR` (and its
    `waddle_xmpp::isr::ISR_NS` re-export) changed from the pre-Slice-8
    `urn:xmpp:isr:0` to the XEP-fact-checked `https://xmpp.org/extensions/isr/0`.
    No back-compat alias — the old value was never a form XEP-0397
    actually specifies (an `htpps` vendored-source typo, not an alternate
    valid spelling).
81. **`ClusteringHandles::isr_token_store()` doubles as the single
    availability predicate** for both the stream-features/disco
    advertisement gate and the `<isr-enable/>`/ISR-resume handlers'
    "is ISR actually usable right now" check — not a separate
    `clustering.enabled && Postgres` config read. This means the
    advertised capability and the wired behavior are structurally
    incapable of drifting apart (the same accessor gates both), at the
    cost of the predicate's name not literally mentioning Postgres.
82. **`<isr-enable/>`/`<isr-enabled/>` land as new optional fields on the
    existing `SmEnable`/`SmEnabled` stanza types**
    (`isr_enable_mechanism: Option<String>` /
    `isr_token: Option<String>` + `with_isr_token`), not a new nonza
    type — `<isr-enable/>` is genuinely an inline child of `<enable/>`
    per the XEP, not a sibling top-level element.
83. **Resume-impossible branch simplification**: `handle_sm_resume`'s own
    "handled count too high" case is a hard protocol violation that
    closes the connection outright; the ISR resume path instead maps
    *both* "handled count too high" and "replay window gone" onto
    XEP-0397's single `<success/>`+`<inst-resume-failed/>` branch
    (different XEP-0198 `<failed/>` conditions:
    `unexpected-request` vs. `resource-constraint`), releasing (not
    destroying) the claim either way. This matches the XEP's own text
    ("the initiating entity MAY continue with normal session
    establishment") more closely than hard-closing the connection would,
    and keeps the ISR resume path from ever needing a
    connection-closing exit once the token has already been validated.
84. **Council-adjudicated FIX 1 — `consume`'s delete is now gated on
    `stored.is_some()`, closing a concurrent phantom-delete (critical,
    verified against a live Postgres)**. The pre-fix `consume` ran its
    `DELETE FROM clustering_isr_tokens WHERE sm_id = ?` unconditionally,
    immediately after the `SELECT ... FOR UPDATE` and before ever checking
    whether that `SELECT` found a row. Under a genuine concurrent double
    -consume (two callers presenting the same, still-valid token), the
    losing transaction's `FOR UPDATE` blocks on the winner's row lock;
    once the winner commits its `DELETE` (rotation is delete-then-insert,
    never an `UPDATE` of the same row), Postgres's own documented
    row-locking semantics mean the loser's blocked read observes **no
    row at all** once unblocked — even though a new row sharing the same
    `sm_id` primary key value now exists, because that row is a distinct
    tuple the loser's already-in-flight query was never watching. The
    pre-fix code's unconditional delete, reached via this exact `None`
    path, still issued the same `DELETE ... WHERE sm_id = ?` — which,
    since the winner's freshly-rotated row matches that predicate,
    deleted the WINNER'S row. The loser's caller (`isr_resume.rs`) would
    then treat its own (formerly) `Mismatched` outcome as a genuine
    wrong-token attack and destroy the winner's just-resumed session —
    reproduced against a live Postgres before this fix. The fix: if
    `stored.is_none()`, commit read-only and return immediately (FIX 3's
    new `NoSuchToken`, below) without touching the table; only when
    `stored.is_some()` does the delete/compare/conditional-rotation
    sequence run, unchanged from before. This is a partial, not total,
    reconciliation with element 10's literal "compare... and only then
    performs the delete" text (conformance finding 8): the delete is now
    conditioned on a compare-worthy precondition existing, but the actual
    `DELETE` statement still precedes the Rust-side comparison in program
    order (deliberately — see `clustering/isr.rs`'s own doc comment for
    why splitting the delete into two call sites gains nothing).
85. **Council-adjudicated FIX 3 — `IsrConsumeOutcome` gains a third
    variant, `NoSuchToken`, narrowing the destroy blast radius to genuine
    wrong-token attempts.** Before this fix, `Mismatched` covered three
    distinct cases indiscriminately: no token row ever existed (never
    opted into ISR, or already consumed), a real row existed but the
    presented token didn't match it, or the pinned mechanism didn't
    match — and `isr_resume.rs` destroyed the SM session's claimed state
    (`complete_claim`) for all three. XEP-0397's anti-brute-force MUST
    only actually requires destruction for the middle case (a genuine
    failed-token attempt against a real ISR-enabled session); the other
    two describe a session that was never meaningfully "ISR-authenticated
    against" at all. `NoSuchToken` now covers the no-row case
    specifically (both fixed stores — `InMemoryIsrTokenStore` and
    `PostgresIsrTokenStore` — mirror the same distinction); `Mismatched`
    is reserved for a row that genuinely existed and didn't match.
    `isr_resume.rs` destroys on `Mismatched` only, and on `NoSuchToken`
    fails with the same `not-authorized` SASL2 condition but releases the
    claim back to the resumable pool instead, exactly like its own
    identity-mismatch rejection. This also happens to be the correct
    outcome for FIX 1's concurrent loser (which now genuinely observes no
    row, not a stale read of the winner's replacement).
86. **Council-adjudicated FIX 4 — a bounded TTL sweep, not a cross-crate
    cascade, reaps orphaned `clustering_isr_tokens` rows.** A row is
    minted per `<isr-enable/>` and, absent this fix, was never reaped
    except by an ordinary `consume` (match or mismatch) — a token issued
    but never resumed, or whose SM session claim is later
    expired/reaped elsewhere with no hook back to this table, would sit
    forever. A cascade off the SM session claim's own release/reap paths
    was considered and rejected: those paths live in
    `waddle_xmpp::stream_management` (cross-node-generic, no clustering
    dependency), while `IsrTokenStore` is `waddle-server`-local and
    Postgres-only — coupling them would break the same crate-boundary
    separation `ClaimStore`/`IsrTokenStore`'s own trait split exists to
    preserve. Instead: `IsrTokenStore` gains a `sweep_expired(max_age)`
    trait method (a no-op returning `0` for `InMemoryIsrTokenStore`, which
    tracks no issuance timestamp and is never exercised in production
    anyway); `PostgresIsrTokenStore::sweep_expired` deletes rows whose
    existing `created_at` column predates `max_age`, mirroring
    `lease.rs`'s own `(? || ' milliseconds')::interval` TTL idiom; and
    `session_janitors.rs`'s existing orphan-reaper sweep calls it once per
    tick, bounded by its own timeout, alongside the claim-steal sweep it
    already runs.
87. **LANDED — the DashMap-selection→actor migration, 17 sites.** The two
    inherent `ConnectionRegistry` selection methods
    (`select_routable_resources_for_user`, `get_resources_for_user`) are
    deleted; every call site now goes through the new crate-level async free
    functions in `waddle-xmpp/src/registry/selection.rs`, which ask the
    resolved `UserActor` for `SelectRoutableResources`/`GetResources` instead
    of reading the DashMap. See the Slice 9 "LANDED" migration map for the
    full 17-site breakdown (`routing.rs` ×2, `admin/channels.rs`,
    `presence/subscription/directed.rs`, `presence/subscription/delivery.rs`,
    `presence/probe.rs` ×3, `message/group_dm_invite.rs`, `pubsub_fanout.rs`
    ×2, `interpret/offline_delivery.rs`, `interpret/groupchat_archive.rs`,
    `sm_promotion/live.rs`, `sm_promotion/pending.rs`, plus the downstream
    threading sites in deviation 92 below).
88. **The Slice-1 DashMap-liveness intersection filter
    (`select_bare_jid_live_targets`'s `filter_deliverable`) is retired**,
    with the headless-persistence gate (`run_headless_recipient_pass` on an
    all-`DroppedClosed` bare-JID delivery) substituted alongside it, per the
    Phase 1 completion note's own stated dependency ("only needed once the
    liveness filter is retired"). See deviation 90 below for the accepted
    behaviour change this retirement introduces and the precondition
    correction.
89. **`DeliveryDisposition` (`DeliveredLive`/`QueuedDetached`/`Dropped`)
    typed disposition tracking** replaces the prior boolean
    delivered/not-delivered filter. `deliver_peer_to_full`,
    `deliver_direct_to_full`, `deliver_to_detached`, and
    `deliver_one_via_actor` all now return this typed disposition, which the
    bare-JID arm uses to decide whether anything landed across the live +
    detached delivery set and, when nothing did, to trigger the deviation 88
    headless-persistence gate.
90. **ACCEPTED BEHAVIOUR DEVIATION (council-adjudicated) — a stale
    top-priority ghost resource can now cause a brief misroute to
    offline/detached storage.** With the deviation 88 filter retired, a
    stale extra holding a unique top priority whose channel has already
    closed is still ranked top by the actor (the max-priority collapse no
    longer runs after a DashMap intersection), so on that first delivery the
    live *lower*-priority resources are not live-delivered. This self-heals
    via `DroppedClosed` eviction on the very next bare-JID delivery, which
    then converges on the true live top-priority resource. There is **no
    message loss**: the message is persisted (the shared fan-out recipient
    pass archives regardless of live-send outcome; for non-fan-out paths the
    deviation 88 headless gate persists it). This corrects the plan's
    earlier framing (see deviation 88 and the corrected quotation in the
    Slice 9 body text above): **neither** of the Phase 1 completion note's two
    named preconditions for retiring the filter — teardown flipping the
    shared ownership atomic, or the empty-actor reaper eagerly reaping stale
    extras — was actually implemented. The headless-persistence gate is a
    third, substitute mechanism the note never named; this slice does not
    "complete" the note's deferred work as originally described, it replaces
    it with a different mechanism that produces a weaker (but still
    loss-free) guarantee. **Scope correction (coordinator ruling):** this
    self-healing-via-`DroppedClosed` behaviour applies to the
    `route_to_connection.rs` delivery path only. `sm_promotion::promote_one`
    still delivers via `ConnectionRegistry::send_to` and does **not**
    self-evict ghosts on that path — but it is bounded by its own existing
    pending-storage (Archived/Transient) fallback, so this deviation
    introduces no new loss there either. Earlier doc phrasing implying the
    self-healing behaviour applies uniformly to the promotion path is
    corrected by this entry.
91. **`DeliveryHandles<'a>` / `DisplacedPromotionDeps` parameter bundles**,
    added to `sm_promotion/pending.rs` and `sm_promotion.rs` respectively to
    stay under clippy's `too_many_arguments` cap once the migrated
    DashMap-selection callers were threaded through.
92. **`push_inbox_update`'s new `Option<&ActorRef<UserRegistryActor>>`
    parameter**, threaded through `GroupchatInboxProjectionInputs`,
    `groupchat_inbox.rs`, `displayed_marker.rs`, and
    `iq/archive_inbox_upload.rs` — the downstream wiring sites that, together
    with `http.rs` and the 3 `tests/iq.rs` call sites for `admin/
    channels.rs`, account for the gap between the ~14 directly-confirmed
    former-DashMap call sites and the 17-site migration count in deviation
    87.

### Slice 10 as-landed corrigenda (implementation-time findings)

93. **The acquire-side draining gate — the `ClaimStore::release_many` trait
    doc comment's own forward reference ("`NodeLeaseStore::mark_draining`
    stops this node from *acquiring* new claims once draining") was
    aspirational, not yet true, at Slice 10 implementation time** (research
    confirmed zero references to `draining` in `acquire`/`ensure_claimed`/
    `steal_stale`). Landed as: a new `ClaimError::Draining` variant, plus an
    atomic `INSERT ... SELECT ... WHERE NOT EXISTS (... draining ...)`
    reshape of `acquire`'s CAS (a follow-up read on the cold 0-affected path
    distinguishes "genuinely already claimed" from "this node's own
    draining gate blocked it," never a second round-trip on the hot
    uncontended path) and an added `AND NOT EXISTS (... draining ...)`
    clause on both `steal_stale` arms (folded into the pre-existing
    `ClaimError::Conflict` catch-all, no new variant needed there).
    `ensure_claimed`'s conflict-fallback was widened to treat
    `AlreadyClaimed`/`Draining` identically when attempting the
    self-reacquire read, so "keep serving already-owned draining entities"
    (element 4) composes correctly with the new gate: a self-reacquire on
    an entity this exact node/epoch already holds still succeeds while
    draining; only a genuinely new acquisition is refused.
94. **Q5's "most-recently-registered live node" mechanism required a new
    `clustering_nodes.first_seen` column**, not specified in Slice 1's
    original DDL (which only carries `heartbeat`, continuously refreshed on
    every renewal — unsuitable as a registration-order proxy).
    `first_seen` defaults to `now()` on INSERT and is deliberately omitted
    from `register`'s `ON CONFLICT (node_id) DO UPDATE SET` list, so a
    re-registration under the *same* `node_id` never refreshes it, while a
    fresh `node_id` (post-fence re-registration, per Q7/element 4) gets its
    own fresh `first_seen` — matching Q5's own text that a rollback
    "re-registers newer rows than the half-rolled-out generation it is
    replacing" and is, by the operational definition, automatically
    current. `NodeLeaseStore::current_generation() -> Option<String>` reads
    `pod_template_hash` off the non-expired row with the greatest
    `first_seen`; deliberately does NOT exclude `draining` rows (unlike
    `count_other_live_nodes`'s isolation filter — see that method's own
    reasoning about why a draining node's registration event is still a
    valid rollout-generation signal).
95. **Rollout-aware backoff wiring required two separate call-site
    strategies, not one**, because the two production "steal a
    dead/orphaned claim" sites straddle the `waddle-xmpp`/`waddle-server`
    crate boundary the ADR's Q1 unconditional-compilation rule protects:
    the orphan reaper (`session_janitors.rs`, SM sessions) lives in
    `waddle-server` and reads `NodeLeaseStore::current_generation` plus
    `ClusteringHandles::pod_template_hash` (a new field, threaded from
    `start_if_enabled`'s local `pod_template_hash`) directly. `RoomRegistry`
    's re-election path (`steal_from_dead_owner`) lives in `waddle-xmpp`,
    which cannot depend on the `clustering`-feature-gated `NodeLeaseStore`
    at all — so a new, unconditionally-compiled `waddle_xmpp::ownership::
    RolloutBackoff` trait (`async fn acquire_delay(&self) -> Duration`) was
    added, injected into `RoomRegistryActor` via a new optional field on
    `WireClusteringClaims` (`None` by default — a no-op, correct for every
    single-node deployment and pre-existing test), with the concrete
    `clustering::drain::PostgresRolloutBackoff` implementation constructed
    and wired at `server/mod.rs` alongside the room registry's other
    clustering handles. Both paths share one pure, unit-tested decision
    function, `clustering::drain::rollout_backoff_delay`.
96. **The generic per-entity drain (`clustering::drain::run_shutdown_drain`)
    is scoped to `EntityType::RoomActor` only — it deliberately does NOT
    also drive `SmSession` entities**, even though `LocallyClaimedEntities::
    owned()` (via `CombinedLocalClaims`) reports both. The existing Q6 SM
    drain (`session_janitors::spawn_graceful_shutdown_drain`,
    `InMemorySmSessionRegistry::confirm_drained`) already correctly
    implements "final write, then release" for SM sessions on an
    independent task racing the same shutdown token; running the generic
    loop over the same entities concurrently would be exactly the
    double-drain hazard the plan's own "interaction with existing drain"
    paragraph warns against (one path could release a claim the other
    path's promotion is still mid-write under). The two mechanisms compose
    via a shared observability surface (deviation 97) rather than a shared
    code path. `EntityType::UserActor` is also skipped, defensively — no
    production claim-acquisition call site exists yet for it (deviation
    34).
97. **`InMemorySmSessionRegistry::confirm_drained`'s signature changed from
    `-> ()` to `-> bool`** (`true` iff the durable row was deleted and the
    claim-release was attempted), so `spawn_graceful_shutdown_drain` can
    feed the Q6 drain's own outcomes into the SAME
    `claims_released_on_drain`/`claims_abandoned_on_drain` counters the
    generic room drain uses — one shared observability surface across both
    independent drain mechanisms, per the "trace and integrate, don't bolt
    on" instruction. Not marked `#[must_use]` (existing call sites in
    `sm_promotion.rs`'s SM-expiry-janitor path and every pre-existing test
    intentionally keep discarding it — that promotion path is routine
    session expiry, not a graceful-shutdown-drain event, and does not feed
    these counters). `spawn_graceful_shutdown_drain` was also instrumented
    at its other two "session left claimed" branches (blocklist-load
    failure, storage-failure) and at the drain-deadline-timeout break
    (counting whatever `live_session_ids()` still reports), plus a
    `drain_duration_ms` histogram record for the Q6 portion's own
    wall-clock — all landing on the same `waddle.clustering.*` instruments
    the generic drain records into.
98. **`clustering::start_if_enabled`'s return type changed from
    `Result<ClusteringHandles, ClusteringError>` to
    `Result<(ClusteringHandles, ClusteringShutdown), ClusteringError>`**
    (one call site, `server/mod.rs`; `ClusteringShutdown` is a small
    `Option<JoinHandle<()>>` newtype, not a `ClusteringHandles` field,
    because `JoinHandle` is neither `Clone` nor meaningfully `Default` and
    `ClusteringHandles` derives both). Before this slice, `run_node_lease`'s
    task was pure fire-and-forget (`tokio::spawn` with the handle dropped);
    element 4's drain sequence explicitly requires the per-entity drain
    "completing before process exit," not merely racing it in the
    background. `server/mod.rs` now calls
    `clustering_shutdown.await_drain(claim_release_budget + 2s margin)`
    after `http_handle.await` returns (the existing Q6/HTTP exit point) and
    before the Ecdysis lifecycle task teardown — bounded, so a wedged drain
    task cannot hang process shutdown forever (a timeout here is logged and
    the process proceeds to exit; any un-released claims are fenced-safe
    and reclaimed later).
99. **`claimReleaseBudget` landed as `ClusteringNodeLeaseConfig::
    claim_release_budget`** (`WADDLE_CLUSTERING_CLAIM_RELEASE_BUDGET_MS`,
    default 5s, same typed-`config.rs`-pipeline pattern as every other
    clustering timing value — never `WADDLE_DRAIN_TIMEOUT_SECS`'s raw-
    `std::env::var` shortcut, which predates the typed-config convention
    and is not clustering-specific). Helm-surfaced as
    `clustering.claimReleaseBudgetSeconds` (default 5), wired
    unconditionally into `configmap.yaml`
    (`WADDLE_CLUSTERING_CLAIM_RELEASE_BUDGET_MS`) and into
    `validations.yaml`'s `terminationGracePeriodSeconds` shutdown-floor
    formula (`preStopSleepSeconds + config.drainTimeoutSeconds +
    clustering.claimReleaseBudgetSeconds + kill margin`) even while
    `clustering.enabled` stays hard-locked `false` by the chart's own Phase
    0 guard — mirroring `WADDLE_CLUSTERING_POD_TEMPLATE_HASH`'s identical
    "ships ahead of Phase 4's chart unlock rather than needing a second
    chart change then" precedent. `terminationGracePeriodSeconds`'s chart
    default bumped 40 → 45 to keep the default values passing the new
    floor.
100. **The ADR's own "drain-at-modeled-scale measurement... as an exit
     criterion" landed as a Postgres-gated, single-process test**
     (`clustering::drain::tests::drain_at_modeled_scale_fits_the_claim_release_budget`,
     2,000 `RoomActor` claims against a real `PostgresClaimStore`, real
     `release_many` batching, asserting wall clock against a real
     `claimReleaseBudget`) rather than inside the multi-process
     `clustering_cluster_e2e.rs` harness the ADR's own Phase 3 text
     describes. The property under test — one `release_many` round-trip
     fitting the budget regardless of entity count — does not require
     multiple processes to observe; multi-process cross-node concerns for
     this same mechanism (a different node winning a claim mid-drain) are
     covered separately by the draining-gate tests (deviation 93) and the
     ABA-window tests (deviation 101). Extending the multi-process harness
     with a true multi-process drain scenario is carried forward as
     follow-up scope, not landed this slice.
101. **`ClaimStore::release_many`'s "plan-sanctioned ABA window" doc
     comment was verified, not merely trusted, against the actual landed
     SQL** — `clustering::claims::tests::
     release_many_epoch_blind_window_deletes_a_fresh_same_node_resume`
     reproduces the documented window literally: an entity queued for
     release, then legitimately re-claimed by the SAME node/epoch at a
     HIGHER epoch via `steal_for_resume` (the one CAS variant the new
     draining gate does not cover, by design — a draining node must keep
     accepting resumes for SM sessions it already owns), is deleted by the
     stale batch entry regardless, exactly as documented. Separately,
     `clustering::drain::tests::
     drain_prevents_a_draining_node_from_reclaiming_its_own_just_released_room`
     proves the window is closed in practice for `RoomActor` entities — the
     only entity type the generic drain ever batches into `release_many` —
     because rooms have no `steal_for_resume`-equivalent consent path and
     the new draining gate (deviation 93) refuses this node's own
     re-acquire attempts outright. Net: the window is real at the store
     level (proven), and unreachable through the production drain path for
     every entity type that path actually touches (also proven) — narrowed
     to fully closed in practice, not merely by argument, satisfying the
     Slice 3 forward reference's "prove the mitigations narrow the window
     in practice, not merely in the doc comment's argument."

### Slice 10 council-adjudicated fixes pass (deviations 102–105)

102. **FIX 4: the "drain-interrupted relay ask increments
     `claims_abandoned_on_drain`" interleaving named in this section's own
     Tests paragraph (and major fix 12's follow-through text above) is
     unreachable this slice, and is deliberately NOT fabricated as a
     test.** No production call site issues a relay ask against a
     locally-**owned** entity from inside this node's own drain window: the
     drain (`clustering::drain::run_shutdown_drain`) *releases* this node's
     rooms, it does not *ask* anything about them; the MUC `Demote` relay
     ask fires when a room is stolen FROM this node by another node, not
     during this node's own drain; and the SM live-steal handshake (Slice
     6) is issued BY the resuming node, never by the draining node about
     its own claims. Every actual drain-window cancellation path this
     slice exercises is covered by the existing `claims_abandoned_on_drain`
     tests instead (a failed/timed-out `seal_before_release` —
     `clustering::drain::tests::drain_abandons_a_failed_seal_and_leaves_the_claim_held`,
     `drain_abandons_a_hung_seal_once_the_budget_is_exhausted`, deviation
     103 below). Carried forward, unreachable-this-slice, to Phase 4's
     cross-node routing work, where a drain-window relay ask against an
     owned entity first becomes possible (a node mid-drain fielding a
     cross-node request FOR one of its own still-owned entities — no such
     request exists yet, since Phase 3 does not wire cross-node stanza
     routing/proxying at all).
103. **FIX 5(b): the Tests paragraph's "forced early SIGKILL simulation" is
     landed as `clustering::drain::tests::
     drain_abandons_a_hung_seal_once_the_budget_is_exhausted`** (a hung
     `seal_before_release` that outlasts `claim_release_budget`) rather
     than an actual process-level SIGKILL. Functionally equivalent for what
     this test needs to prove: from `run_shutdown_drain`'s own perspective,
     a budget overrun and a SIGKILL both produce the identical observable
     outcome — the entity's claim is never queued into `release_many`'s
     batch, so it remains held, fenced-safe, un-released — proving the
     same "budget overrun leaves the claim held, never released with an
     in-flight write" invariant a real SIGKILL would, without the harness
     complexity a genuine process-kill simulation would add for no
     additional coverage.
104. **FIX 1 (council-adjudicated): the seal-before-release barrier the
     Locked spec's "complete its final fenced write while holding the
     claim, THEN release" ordering depends on is now genuinely
     implemented**, not merely inherited from
     [`LocallyClaimedEntities::seal_before_release`]'s trivial `true`
     default. `RoomLocalClaims::seal_before_release`
     (`clustering::local_claims`) issues the SAME `HealthCheck` ask
     `RoomLocalClaims::health_check` (Slice 3) already uses, as a genuine
     mailbox-ordering barrier: kameo serializes each `RoomActor`'s mailbox
     strictly in order, and every mutation handler (`UpdateConfig`,
     `RollbackConfigIfRevision`, `UpdateGroupDmConfigByMember`,
     `SetSubject`, `ChangeAffiliation`) synchronously `.await`s its own
     `gate_mutation()` check and durable persist call before returning —
     so a successful `HealthCheck` reply, issued after the drain's
     `owned()` snapshot, proves every mutation queued ahead of it has
     already committed its durable write. A room with no live local actor,
     or whose ask fails/times out, reports unsealed (`false`), leaving the
     claim held rather than released — `CombinedLocalClaims::
     seal_before_release` dispatches to it by `EntityType`
     (`SmSession`/`UserActor` keep the trait's `true` default: Q6 already
     owns SM-session drain, and no production `UserActor`
     claim-acquisition call site exists yet). Tested directly:
     `clustering::local_claims::tests::
     seal_before_release_proves_a_queued_ahead_mutation_already_committed`
     (a mutation fired but not yet awaited when the seal ask is issued must
     have its durable write committed before the seal ask returns, proven
     deterministically via a `tokio::sync::Notify` rendezvous rather than a
     sleep-based race) and the
     `room_seal_before_release_is_{unsealed,sealed}_*` trio (no-live-actor
     / dead actor / healthy actor), mirroring `health_check`'s own existing
     trio.
105. **FIX 2 (council-adjudicated): a draining node now keeps renewing its
     own node-lease heartbeat for the FULL duration of its own graceful
     drain, rather than freezing heartbeat renewal the instant shutdown
     fires.** Previously every `stop_token.cancelled()` branch in
     `run_node_lease`'s `'tick` loop called `clustering::drain::
     run_shutdown_drain(..).await; return;` directly — heartbeat renewal
     stopped dead the moment shutdown began, so this node's
     `clustering_nodes` row could go heartbeat-stale (`node_lease_ttl`
     after that last renewal) while the node was still legitimately
     sealing/writing its own owned claims, opening a window for another
     node's orphan reaper to `expire()`/`steal_stale(OwnerStale)` a claim
     out from under a still-draining owner — split-brain. Landed as
     `self_fence::run_shutdown_drain_with_heartbeat`: the drain future and
     a heartbeat-renewal ticker (period `lease_ttl / 2`, no artificial
     floor anywhere near `lease_ttl` itself — an early draft of this fix
     floored the ticker period at a flat 1s, which silently INVERTED the
     fix for any `lease_ttl` configuration below ~2s by renewing slower
     than the row could go stale, caught by this deviation's own
     Postgres-gated ordering test before landing) are polled concurrently
     in the SAME task/stack frame via `tokio::select!` — no
     `tokio::spawn`, so no `Clone`/`'static` bound is needed on the generic
     `NodeLeaseStore` type parameter. `run_node_lease` does not return
     until the drain future itself resolves. Defense-in-depth:
     `config.rs`'s `ClusteringConfig::from_vars` now rejects a config where
     `node_lease_ttl` does not exceed `claim_release_budget * 3` (mirroring
     this same function's other "conservative multiple of the faster
     timer" floors), and both `ClusteringNodeLeaseConfig::
     claim_release_budget`'s doc comment and the Helm chart's
     `validations.yaml` document (but do not additionally validate, since
     `WADDLE_CLUSTERING_NODE_LEASE_TTL_MS` is not itself a chart value) the
     further relationship to the separate, raw-env-var
     `WADDLE_DRAIN_TIMEOUT_SECS` Q6 SM-session drain budget, which this
     heartbeat-renewal mechanism does NOT itself cover (it only renews
     through this node's OWN `claim_release_budget` window, not through
     Q6's independent, potentially much longer one racing the same
     shutdown token). Tested: `clustering::drain::tests::
     drain_keeps_the_node_lease_row_fresh_through_a_drain_that_outlives_the_old_heartbeat_interval`
     (Postgres-gated — a drain whose seal genuinely outlasts several
     `lease_ttl` intervals must never let a concurrent `NodeLeaseStore::
     expire` poll commit `expired = true` until the drain completes) plus
     `config::tests::clustering_rejects_node_lease_ttl_too_close_to_claim_release_budget`
     / `clustering_accepts_node_lease_ttl_with_comfortable_claim_release_budget_margin`
     / `clustering_defaults_satisfy_the_claim_release_budget_margin`.

### Slice 11 as-landed corrigenda (deviations 106–111)

106. **The multi-process kill-one claim-scoped hydration capstone
     (`clustering_cluster_e2e.rs::
     orphan_reaper_kills_one_node_and_hydrates_only_its_orphaned_sessions`)
     seeds one precondition directly, mirroring the already-accepted
     single-process convention, because no production caller exists for
     it.** Tracing every production caller of `NodeLeaseStore::expire`
     turns up exactly two: the orphan reaper's own per-candidate
     reaffirmation (gated on a row already being a candidate — circular for
     a row that isn't yet, since `list_orphaned_sm_session_claims` and
     `steal_stale(OwnerStale)` both read only the *committed* `expired`
     flag, never a raw heartbeat comparison), and
     `self_fence::expire_bounded`, called only on a node's own
     just-superseded identity during its own post-fence re-registration.
     No production code path ever commits `expired = true` on another,
     silently (unclean-SIGKILL) dead node's row — there is no stale-node
     watchdog sweep. A bare `SIGKILL` alone therefore never makes a dead
     node's `sm_session` claims reaper-visible in production today; the
     real-world recovery path for a still-wanted session is almost always
     a client's own XEP-0198 resume hitting the Slice 6 cross-node
     live-steal handshake instead (authorized via `ResumeIdentityProof`,
     never touching the `expired` flag) — the orphan reaper is the
     backstop for sessions that are never resumed at all. Committing that
     one flag for a genuinely-dead peer is a production-code change (a new
     watchdog caller of `NodeLeaseStore::expire`), which this slice's HARD
     RULES forbid making speculatively during a harness-only slice —
     flagged here as a real finding, not silently patched. The
     already-landed single-process
     `session_janitors::orphan_reaper_sweep_tests` established the
     accepted harness convention for this exact gap: its own
     `register_and_pre_expire_dead_owner` helper commits the expire CAS
     directly, through the same public `ClaimStore::expire` method the
     reaper itself calls, rather than fabricating a nonexistent production
     caller — the multi-process test mirrors that precedent (also
     mirroring this same file's `deposed_owner_with_live_socket_room_actor_scenario`,
     which seeds `report_steal_intent` directly for the identical "no
     production reporter call site exists" reason). Everything downstream
     of that one seeded fact — the real subprocess's own, unmodified
     `spawn_orphan_reaper_janitor` loop discovering the candidate, the
     steal CAS, and the targeted `hydrate_reclaimed` hydration — runs for
     real, observed only through Postgres (the janitor's sweep function is
     `waddle-server`-crate-private, so the harness cannot call it
     directly). At this deviation's original time of writing the janitor's
     cadence (`ORPHAN_REAPER_INTERVAL`) had no env override, so this test
     waited out the real ~120s production interval; deviation 111 below
     closes that specific gap (a minimal, env-only production change) so
     this test now observes the same real, unmodified sweep logic in
     seconds instead. **Recommended follow-up (not actioned this
     slice):** consider a bounded periodic "stale-node watchdog" that
     proactively calls `NodeLeaseStore::expire` against every
     heartbeat-stale `clustering_nodes` row cluster-wide (not just
     already-surfaced claim candidates), closing this gap for genuinely
     abandoned nodes/claims in production, not merely in this harness — see
     also the prominent "Carried risks / deferred to Phase 4" bullet this
     deviation is cross-referenced from.
107. **`partial_partition_degrades_without_fencing` (landed as
     `whole_node_isolation_fences_then_self_heals_without_operator_intervention`)
     cannot construct a
     genuine single-pair dead link — only a whole-node, symmetric
     isolation primitive exists.** This harness's sole
     connectivity-affecting primitive reachable from a test other than
     `cluster_exit_criteria_end_to_end` is `clustering_peer_allowlist`
     revocation: one global `HashSet<PeerId>`, read identically by every
     node's own refresh — revoking a peer_id cuts it off from the whole
     mesh, never from one specific counterpart; there is no per-pair
     column or gate anywhere in `clustering/allowlist.rs`. The relay
     actor's fault-injection messages (`RelayCrash`/`RelaySleep`,
     `clustering/relay.rs`) are the only other candidate primitive, but
     triggering either requires a `RelayHandle`, which requires the
     issuing process to itself join the swarm via `swarm::spawn` — and
     this file's own module doc states kameo's `init_global` is a process
     singleton, so only one test in the whole binary may ever do that;
     `cluster_exit_criteria_end_to_end` already claims that slot for the
     Phase 2 suite. Building a genuine per-edge partition primitive (e.g.
     a per-peer connection-gating config knob) is a production-code
     change, out of scope for this harness-only slice per its HARD RULES —
     flagged here rather than added speculatively. As-landed substitute:
     full, symmetric isolation of one of three nodes (the same primitive
     `lone_survivor_and_isolation_fencing`'s Part 2 uses), EXTENDED with an
     assertion Part 2 does not make — genuine, unattended self-recovery
     (re-registration under a fresh `NodeIdentity`, `self_fence.rs`'s
     `can_reacquire_claims`/`readiness.set_ready(true)` re-arm path, proven
     via a Postgres-observed fresh live row plus the original row's
     committed `expired = true`) once connectivity actually returns, with
     no process restart — the most faithful available proof that a
     connectivity degradation "degrades... without [permanently] fencing"
     rather than requiring operator intervention to recover.
108. **(Reworded, Slice 11 corrigenda — the original wording above implied
     unobservability; the precise claim is non-diagnosticity.) The
     durable-`pending_delivery`-fallback half of
     `whole_node_isolation_fences_then_self_heals_without_operator_intervention`'s
     exit criterion would be non-diagnostic if asserted inside THIS test,
     confirmed by tracing every write path — it is not, on that basis, left
     unasserted.** `OfflineDeliveryHandler`/`queue_offline_delivery`
     (`server/routes/interpret/offline_delivery.rs`) fires on the
     purely-local headless-recipient pass
     (`route_to_connection.rs::select_bare_jid_live_targets`, which
     consults only the in-process `UserRegistryActor` — no
     clustering/claims lookup anywhere on that path), and
     `clustering/relay.rs`'s own module doc states plainly the relay stays
     "discovery/handshake-only," "not wired into the stanza delivery path
     (Phase 4)." That write path is real and harness-observable in
     isolation — a bare-JID message to a recipient whose only live session
     is on ANOTHER node gets written to `pending_delivery` on the sending
     node **regardless of whether the two nodes' link is healthy or
     severed**, because cross-node liveness is never consulted at all
     (Phase 4 scope). Asserting that write inside a partition test would
     therefore prove nothing about partition-triggered fallback behavior
     specifically — it would pass identically in a fully healthy two-node
     cluster, re-proving deviation 14's already-recorded Non-goal ("No
     cross-node janitor-flush test... requires the GA cross-node stanza
     routing this phase explicitly excludes... deferred to Phase 4") under
     a partition label it does not need. Not re-litigated here; the landed
     test proves only what a partition scenario can actually distinguish
     (targeted isolation-fencing plus genuine self-healing recovery),
     consistent with that already-settled scope boundary.
109. **(New, Slice 11 corrigenda — closes a criterion-2 coverage gap found
     on post-landing review.) `self_fence.rs`'s terminal "self-fenced
     (either trigger): demote ALL local claims" branch (`for entity in
     local_claims.owned().await { local_claims.demote(&entity).await }`,
     reached once the `'tick` loop breaks on either fencing trigger) was
     never exercised with a genuine fencing-loss trigger AND a populated
     `LocallyClaimedEntities` in the same test.** Every populated-
     `FakeLocalClaims` test in `self_fence.rs`'s own suite used a
     `FakeLease` whose heartbeat CAS always returns `Ok(true)` (never loses
     fencing — those tests exercise the owner-veto scan instead); every
     test that genuinely loses fencing (`FakeLease::new(|| Ok(false))`)
     used `NoLocallyClaimedEntities` (an always-empty `owned()`). Neither
     combination alone proves the demote-all loop demotes every owned
     claim, which is exactly what exit criterion 2 requires. Closed by
     `clustering::self_fence::tests::node_lease_demotes_every_owned_claim_on_fencing_loss`:
     `FakeLease::new(|| Ok(false))` (fencing loss on every heartbeat) driven
     against a `FakeLocalClaims` populated with two owned entities, asserting
     both were demoted and readiness flipped not-ready. **The demote-all
     code itself was already correct — this test passed on first write,
     with no production code change required.** Only the test coverage was
     missing; flagged and closed in the same slice, not carried forward.
110. **(New, Slice 11 corrigenda — closes a criterion-4 coverage gap found
     on post-landing review.) Cross-node XEP-0198 resume's branch 3
     (owner-unreachable → HOLD + retry, per element 8) had only
     failure/timeout coverage — no test ever drove a retry that actually
     SUCCEEDS on a later attempt.** Every `SlowAsker`/`UnreachableAsker`-
     shaped `RemoteResumeAsker` test double anywhere in this crate returns
     `Unreachable` (or times out) on EVERY call, so
     `prepare_cross_node_resume`'s bounded backoff-and-retry loop was only
     ever proven to hold correctly within budget and then report
     `OwnerUnreachable` — never proven to find a persisted snapshot on a
     LATER iteration and proceed to steal it. Closed by
     `stream_management::session_registry::cross_node_resume::tests::fix_b_flaky_asker_holds_then_succeeds_on_a_later_attempt`:
     a `FlakyAsker` reports `Unreachable` for its first two asks (forcing
     at least two full backoff iterations), then writes the persisted
     snapshot directly and reports `Detached` on its third ask — standing
     in for "the remote owner finally force-detached and persisted." The
     retry loop's own next iteration finds that snapshot at branch 1's
     top-of-loop re-check and steals it, landing `CrossNodeResumeOutcome::Claimed`,
     with `asks_seen >= 2` proving the hold-and-retry loop genuinely
     iterated rather than succeeding on the first attempt. No production
     code change; test-coverage-only, same as deviation 109.
111. **(New, Slice 11 corrigenda — the one sanctioned production change in
     this otherwise harness/test-only slice.) The orphan reaper's cadence
     (`session_janitors::ORPHAN_REAPER_INTERVAL`) was the only cluster
     timer in this codebase with no environment override.** Every sibling
     cluster timer (`WADDLE_CLUSTERING_{HEARTBEAT_INTERVAL,LEASE_TTL,
     NODE_LEASE_HEARTBEAT_INTERVAL,NODE_LEASE_TTL,REREGISTER_BACKOFF_BASE,
     REREGISTER_BACKOFF_MAX,RESUME_HANDSHAKE_TIMEOUT,CLAIM_RELEASE_BUDGET}_MS`,
     etc.) is parsed through the typed `ClusteringConfig::from_vars`
     pipeline with a default plus validation; the orphan reaper's interval
     alone was a hardcoded `const Duration::from_secs(120)` inside
     `session_janitors.rs`, invisible to that pipeline. This is what forced
     deviation 106's harness test to wait out the real ~120s production
     cadence per attempt. Closed with a new `ClusteringConfig` field,
     `orphan_reaper_interval: Duration` (default `120s`, matching the prior
     hardcoded value byte-for-byte), parsed from
     `WADDLE_CLUSTERING_ORPHAN_REAPER_INTERVAL_MS` and validated `> 0`
     identically to every sibling timer (`config::tests::clustering_parses_orphan_reaper_interval_and_rejects_zero`);
     `spawn_orphan_reaper_janitor` now takes the interval as a parameter
     instead of reading the module constant, and `server/http.rs`'s call
     site threads `server_config.clustering.orphan_reaper_interval` through
     — the reaper's SWEEP LOGIC is completely unchanged, only its timer's
     period is now configurable. `clustering_cluster_e2e.rs::spawn_cluster_server`
     sets `WADDLE_CLUSTERING_ORPHAN_REAPER_INTERVAL_MS=500` for every
     subprocess it spawns, so
     `orphan_reaper_kills_one_node_and_hydrates_only_its_orphaned_sessions`
     now observes a real sweep in seconds. Explicitly recorded as a
     deviation from this slice's own "harness-only, zero production
     changes" framing (see the Slice 11 section's "As-landed" text above)
     precisely because it IS a production change, minimal and justified as
     it is.
