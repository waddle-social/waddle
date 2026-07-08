# ADR-0017 Phase 4 Plan - Cross-Node Routing GA and Helm Unlock

Companion execution note to [ADR-0017](./0017-horizontal-scaling-remote-actors.md),
the [Phase 2 plan](./0017-phase2-plan-libp2p-swarm.md), and the
[Phase 3 plan](./0017-phase3-plan-ownership-claims.md). Status: **ratified;
implementation started with Slice 1**.

Tracking: issue #1195 (ADR-0017 horizontal-scaling epic). Phase 4 is the final
unchecked section: cross-node stanza routing over the ordered relay channel, then
chart/operational unlock for `clustering.enabled` and `replicaCount > 1`.

## Sources reviewed before planning

- ADR-0017, especially elements 3-7, 10, 12, the Phase 4 implementation-plan
  paragraph, consequences, and XEP conformance notes.
- ADR-0017 Phase 2 as-landed plan: current swarm, relay, codec, allowlist,
  keypair-slot lease, and multi-process harness boundaries.
- ADR-0017 Phase 3 as-landed plan: Slice 9 DashMap selection retirement, Slice
  10 drain, Slice 11 harness capstone, and **Carried risks / deferred to Phase
  4**.
- Repo-local `./xeps`: XEP-0030, XEP-0045, XEP-0184, XEP-0198, XEP-0313, and
  XEP-0397.

## Goal

Make `clustering.enabled` usable by routing all cross-node XMPP work through
the existing libp2p/kameo relay fabric without changing the wire protocol seen
by clients:

- 1:1 message, presence, and IQ delivery to resources whose live socket is on a
  different node.
- MUC proxying to the owning `RoomActor`, with remote-safe typed messages,
  per-node fan-out aggregation, and recipient-list chunking.
- Presence fan-out and anti-entropy, including synthesized unavailable presence
  when node ownership changes.
- Durable fallback for failed cross-node handoff and the multi-node janitor
  flush case deferred from Phase 3.
- Helm unlock for `clustering.enabled` and `replicaCount > 1`, including
  storage, key enrollment, NetworkPolicy, PDB, probes, dashboards, and alerts.

## Non-goals

- No out-of-band control API. Cross-node control and delivery remain XMPP-native
  internally over the ADR-selected relay; browser telemetry remains operational
  only.
- No use of official XEP namespaces for Waddle-specific semantics.
- No backwards-compatibility shim for the single-replica RWO-PVC upload path.
  The ADR records the Phase 4 S3/object-store requirement as a breaking change.
- No per-entity DHT registration. Kademlia remains node discovery only.
- No self-merge of a production revert. Rollout regressions are reported with
  evidence first.

## XEP constraints locked into the plan

- **XEP-0030:** the legacy/non-conformant `urn:xmpp:isr:0` feature is never
  advertised. The conformant XEP-0397 ISR stream feature
  (`https://xmpp.org/extensions/isr/0`) appears only when the conformant store
  is actually active and the stream is eligible. In non-clustering production
  it remains absent. Disco results advertise only implemented features.
- **XEP-0045:** MUC ownership changes synthesize unavailable presence with
  status `332`, and self-presence also carries `110`. Room state is restored
  before accepting joins; outcast and members-only checks survive ownership
  steal. `RoomFull` remains `service-unavailable`.
- **XEP-0184:** receipts are client-level delivery receipts, not server
  reliability. Cross-node routing passes receipt requests and receipts through;
  archived/MAM replay must not synthesize receipts.
- **XEP-0198:** inbound `h` advances only after this server has taken
  responsibility: remote handoff acked or durable fallback committed. `<r/>`
  responses are never held behind cross-node delivery.
- **XEP-0313 / XEP-0160 / XEP-0203:** durable fallback and janitor flush stamp
  delayed delivery using the original ingress timestamp, with MAM wire shape
  unchanged.
- **XEP-0397 / XEP-0388:** ISR remains conformant: `<isr/>` includes
  `<mechanisms/>`; tokens are inline on SM enable; successful consume destroys
  and rotates the token; failed token auth destroys detached state and claim.

## Slice breakdown

Each slice must be implemented in a worker-owned patch, reviewed by two
adversarial reviewers (correctness/distributed-systems and XEP conformance),
fixed, convergence-checked, locally gated, committed, pushed, and observed in
CI before the next slice starts.

### Slice 0 - plan ratification and branch setup

**Scope**: this document, draft PR, and no runtime code.

**Exit criteria**:

- Draft PR exists with this plan in the description.
- PR title/description clearly say Phase 4 is not implemented yet.
- User ratifies the slice order before Slice 1 begins.

**As-landed**: Ratified by user direction on 2026-07-07 ("Let's focus on track
B and get clustering into production"). Runtime work starts at Slice 1.

### Slice 1 - routing authority substrate and Phase 3 carryovers

**Scope**: close the production-safety gaps that must exist before routing live
traffic across nodes.

- Add a bounded stale-node watchdog that proactively expires heartbeat-stale
  `clustering_nodes` rows using `NodeLeaseStore::expire`, not raw heartbeat
  reads in claim CAS.
- Wire production `UserActor` claim acquisition so cross-node DM/presence has
  the same Postgres-authoritative entity ownership as SM sessions and rooms.
- Land the deferred UserActor deposed-owner-with-live-socket veto scenario.
- Populate and exercise the Phase 3 nullable ordering/dedup columns
  (`origin_stream_id`, `inbound_seq`, `pair_sequence`) before any hot-path
  remote delivery depends on them.
- Add claim-scoped `pending_delivery` sweeps for owned bare JIDs so durable
  fallback can drain without reverting to unsafe unscoped ownership.
- Add the fail-open-detach reconciliation design or a durable marker so
  persisted rows without claim rows are discoverable without unsafe unscoped
  ownership.
- Split schema provisioning from runtime access so the hardened runtime role
  can be `SELECT`-only on allowlist/enrollment tables and non-DDL on runtime
  tables.
- Establish an authenticated origin context for inbound relay messages. The
  handler must not trust a sender-provided `PeerId`; it must validate a
  transport-derived or registry-bound origin against the current allowlist and
  against the current claim epoch for the entity being mutated.

**Exit criteria**:

- Postgres tests cover watchdog expiry, no false expiry for fresh nodes,
  runtime-grant startup with schema already provisioned, and revoked-origin
  rejection on a live relay path.
- Pending-delivery tests prove populated dedup/order columns are required and
  claim-scoped sweeps cannot flush another node's rows.
- Multi-process harness covers the UserActor deposed-owner-with-live-socket
  case within one heartbeat interval.
- Default `clustering.enabled=false` behavior is unchanged.

**As-landed Slice 1a**: Bounded stale-node watchdog landed for the
orphan-reaper sweep. It discovers heartbeat-stale, non-expired node rows with a
deterministic per-sweep limit, commits each candidate through
`NodeLeaseStore::expire`, and only then lets the existing committed-expired
orphan scan reclaim SM-session claims. This closes the Phase 3 carried risk
without adding raw-heartbeat reads to claim CAS/steal logic and without XMPP
wire behavior changes.

**As-landed Slice 1b**: `UserRegistryActor` now acquires a typed
`EntityType::UserActor` claim before spawning a local bare-JID actor, stores the
claim owner plus epoch with the actor entry, and releases with that acquisition
identity on explicit removal, empty-resource pruning, reaper pruning, and
dead-actor fail-fast. Claim acquisition also mirrors the room registry's
dead-owner path: a live foreign owner fails closed, while an owner whose node
lease is no longer fresh is recovered through `steal_stale(OwnerStale)`.
Startup wires the same clustering claim store/shared node identity into the
user registry that SM sessions and rooms already use; non-clustering
deployments stay on the single-node `InProcessClaimStore`. The server-side
`UserLocalClaims` handle now reports local UserActor claims to the node-lease
loop, health-asks them for steal-intent veto, forgets them on demotion without
releasing a claim that may have moved, and force-detaches their live
`ConnectionRegistry` resources through the existing connection-owned
`ForceDetachRequest` path so clients receive the native conflict close and the
normal teardown path removes any remaining routing slot. It fails generic drain
sealing closed until a UserActor-specific durable final-write barrier exists.
Registry-level stale-entry reuse also validates the stored owner/epoch against
the current shared node identity and claim fence; if validation fails, it
force-detaches any old live resources or fails closed before reacquiring or
stealing the claim. Cross-node proxying remains pending before the relay hot
path is enabled.

### Slice 2 - ordered relay channel foundation

**Scope**: introduce the relay message types and channel mechanics, still not
called from the hot delivery path.

- Add typed `RemoteStanzaEnvelope` / relay request and reply outcomes for
  message, IQ, presence, and MUC proxy traffic.
- Add per-peer ordering state: sender sequence, receiver gap detection, ack,
  NACK (`NotOwner`, unreachable, parse failure, backpressure), and bounded
  retry.
- Add sticky durable-fallback diversion per `(origin stream, recipient)` once a
  handoff cannot complete in order.
- Add relay metrics: queue depth, in-flight asks, ack latency, NACK reason,
  sticky diversion count, durable flush lag.

**Exit criteria**:

- Unit tests prove ordering under concurrent large/small payloads on the new
  channel, ack-lost retry idempotence, parse-NACK behavior, and no XML string
  construction outside the existing typed codec boundary.
- Harness exercises two nodes with the relay channel active but no production
  routing callers yet.

### Slice 3 - cross-node 1:1 message and full-JID IQ routing

**Scope**: route DM and addressed IQ stanzas to resources owned by a remote
`UserActor`.

- Replace local-only delivery misses with claim lookup plus ordered relay
  handoff to the owning node.
- Preserve `FullJidDeliveryOutcome` semantics: delivered, dropped full JID,
  dropped closed, durable fallback, synthesized IQ error/ack.
- Failed cross-node IQ creates a typed stanza error, not offline storage.
- Keep delivery non-blocking at connection fan-out boundaries; backpressure
  must produce typed outcomes, not awaited fan-out stalls.
- Preserve Jingle session-terminate ack behavior and undeliverable full-JID IQ
  fallback error/ack behavior.

**Exit criteria**:

- XEP-0198 tests prove `<r/>` immediate ack behavior while a remote handoff is
  pending and `h` advances only after handoff ack or durable commit.
- IQ tests cover remote success, unreachable target, stale owner refresh, and
  undeliverable full-JID synthesized reply.
- Delivery receipt tests prove XEP-0184 requests/receipts pass through without
  server-synthesized receipts.
- IQ tests prove failed cross-node IQ does not create `pending_delivery`.
- Multi-process harness sends a DM to a recipient live on another node.

### Slice 4 - MUC proxying and room-state GA gates

**Scope**: route MUC joins, messages, presence, admin/owner operations, and fan
out through the room owner.

- Add the dedicated remote-safe MUC message set; do not serde the local
  `RoomActor` API wholesale.
- Aggregate per-node fan-out and chunk recipient lists.
- Preserve fenced pre-fan-out checks and `NotOwner` bounce/re-read behavior.
- Restore durable room state before joins after ownership steal.
- Implement any missing room-password feature only if required to satisfy the
  ADR's password-holds GA gate; otherwise document and remove that gate from
  the ratified scope before implementation.

**Exit criteria**:

- XEP-0045 tests cover remote join, groupchat fan-out, self-presence `110`,
  service-shutdown/takeover `332`, room full `service-unavailable`, outcast
  denial after ownership steal, members-only state after ownership steal, and
  gap-window `resource-constraint`. Join ordering remains presence roster,
  self-presence, history, subject, then live traffic.
- Multi-process harness covers a room owner on node A and occupants on nodes B
  and C.

### Slice 5 - presence fan-out and anti-entropy

**Scope**: route ordinary, directed, subscription, probe, and MUC-related
presence changes across nodes.

- Version per-resource presence updates and cache remote views only as
  invalidatable hints.
- Fan out presence to remote interested `UserActor`s over the ordered relay.
- Add anti-entropy repair after missed relay traffic.
- Ensure orphan/watchdog expiry emits synthesized unavailable presence and
  prevents stale owner resurrection.

**Exit criteria**:

- Tests cover remote presence fan-out, probe, subscription broadcast, stale
  owner unavailable synthesis, anti-entropy repair, and no ghost-resource
  delivery after `DroppedClosed` self-healing eviction.

### Slice 6 - durable fallback, janitor flush, and ISR cross-node capstone

**Scope**: finish the deferred end-to-end reliability cases.

- Implement the multi-node janitor flush: sender-to-owner handoff fails,
  recipient socket lives on a third node, delivery occurs within one sweep
  interval plus one relay hop.
- Preserve dedup keys across resume-steal and recipient claim movement.
- Add ISR wrap/half-window tests and cross-node consume cases required by
  ADR-0017.
- Tune sqlx pool acquire/statement timeouts from measured harness behavior and
  document defaults.

**Exit criteria**:

- Harness covers un-divert reorder, groupchat durable fallback with recipient
  dimension, ack-lost-post-commit retry, retry-to-new-owner-after-steal, and
  multi-node janitor flush.
- XEP-0397 tests cover both failure cases and no ISR advertisement when
  clustering is disabled or Postgres ISR storage is absent.

### Slice 7 - Helm, GitOps, and SRE unlock

**Scope**: make multi-replica operable, then remove the hard chart lock.

- Add `clustering.enabled` validation requiring Postgres and object storage
  or RWX persistence.
- Drop the RWO PVC upload requirement in favor of the existing S3 path; keep
  the RWO multi-replica guard for unsafe persistence.
- Add headless Service with `publishNotReadyAddresses: true` and a distinct
  swarm `containerPort`.
- Add NetworkPolicy allowing swarm ingress only from waddle-server pods, with a
  chart/pre-flight test that proves allowed intra-selector traffic.
- Add PDB (`maxUnavailable: 1`), soft pod anti-affinity, and RollingUpdate
  values.
- Add keypair-pool Secret templating and an enrollment Helm hook/job with an
  admin-role Secret; enforce enrolled key count >= `replicaCount + maxSurge +
  headroom`.
- Add dashboard/alert definitions or documented Grafana queries for claim
  churn, deploy-window churn, swarm partition, relay backlog, durable-queue
  diversion, drain abandonment, NotOwner NACK rate, and routing-cache miss
  ratio.

**Exit criteria**:

- `helm template` fails unsafe values and renders a valid multi-replica
  deployment for the ratified safe values.
- GitOps manifests enroll every node before unlock.
- Production readiness remains independent of swarm membership on cold start,
  but flips not-ready on self-fence.

### Slice 8 - GA harness, rollout, and PR closeout

**Scope**: final evidence and operational transition.

- Run the full local gate: `cargo fmt`; clippy with default features and
  `--all-features --all-targets -D warnings`; unit tests; Postgres-gated
  tests; real multi-process harness booting subprocesses, not `--no-run`.
- Update this document with all numbered deviations and final as-landed notes.
- Run adversarial council until no real actionable issues remain.
- Update PR title and description to match the completed work, remove draft,
  monitor CI to green, then merge when green and converged.
- Watch the production rollout and soak period with the Track A criteria.

**Exit criteria**:

- CI green on the Phase 4 PR.
- Production deploy observed at the merge SHA.
- No breaking Track A signals during rollout/soak.

## Deviation log

Number every implementation-time deviation here instead of burying it in PR
comments. Include the triggering finding, the chosen correction, and whether it
changes slice scope, XEP behavior, or operational requirements.

1. Slice 0 ratification recorded from user direction on 2026-07-07. This does
   not change slice scope, XEP behavior, or operational requirements.
2. Slice 1a stale-node watchdog landed as the first Phase 4 runtime slice:
   bounded heartbeat-stale candidate discovery plus per-node
   `NodeLeaseStore::expire` CAS before the orphan-reaper candidate scan. This
   implements planned Slice 1 scope, closes the carried hard-crash expiry gap,
   and changes no XEP behavior or operational requirements.
3. Slice 1a adversarial review found two real safety gaps in the first
   watchdog patch: stale-owner steals could be won by a missing/expired
   stealer lease, and claim-only XEP-0198 SM sessions could be stolen before a
   durable detached `sm_sessions` row existed. The correction makes
   `steal_stale` require a live, non-draining stealer row, gates the
   steal-intent consuming CTE on that same live-stealer predicate, and limits
   orphaned SM-session candidates to claims with durable detached rows. This
   narrows reaper authority, preserves XEP-0198/ISR behavior, and does not
   change operational requirements.
4. Slice 1a convergence review found that a heartbeat-stale-but-not-expired
   local node could still run the orphan reaper before self-fencing, and that
   the multi-process capstone still expected a claim-only SM session to be
   reclaimed. The correction adds a read-only `NodeLeaseStore::is_fresh`
   self-lease proof before the reaper expires peers, a reaper-specific
   stale-owner SM-session steal CAS that binds stealer heartbeat freshness in
   the same SQL `UPDATE`, and changes the harness to seed a real detached
   `sm_sessions` row before expecting cross-node hydration. This narrows
   reaper authority and aligns the harness with the XEP-0198 durable-detach
   boundary.
5. Slice 1b initially split the UserActor carryover into registry-level
   acquire/release only, but adversarial review found that split unsafe: old
   local UserActors could keep serving after self-fence/demotion, stale
   foreign UserActor claims could block binds indefinitely, and releases would
   leak after `SharedNodeIdentity` rotation if they used the current identity.
   The correction stores the acquisition identity with each actor entry,
   releases with that identity, steals only from dead owners via
   `steal_stale(OwnerStale)`, and wires `UserLocalClaims` into
   `CombinedLocalClaims` for owned/demote/health-check dispatch. A follow-up
   convergence pass found actor hard-kill alone insufficient, because the real
   WebSocket task and DashMap routing entry live in `ConnectionRegistry`; the
   correction now force-detaches those resources through the existing
   connection-owned conflict-close channel before owner-gated registry cleanup
   only after the connection task acknowledges cleanup. A final correctness
   pass closed three edge cases: UserActor health checks are now
   non-destructive so a failed health probe cannot release the claim before
   demotion; `owned()` falls back to `ConnectionRegistry` bare-JID enumeration
   if the user registry cannot answer during terminal self-fence; and a queued
   force-detach timeout leaves the registry entry in place so the connection
   task can still consume the request and run non-superseded cleanup. XMPP wire
   behavior remains unchanged except for the intended native `<conflict/>`
   close on a deposed live stream. A later XEP/lifecycle review found the
   registry reuse fast path still needed the same discipline: a stale
   `UserEntry` whose stored owner/epoch no longer matches the current shared
   identity/fence is now retired only after its live resources acknowledge
   `ForceDetachRequest`, otherwise the bind fails closed instead of accepting a
   new resource while old streams remain invisible to the fresh `UserActor`. A
   final correctness pass split claim validation into current, proven-stale,
   and unavailable states: transient `ClaimStore::fence` errors now fail
   closed without detaching or removing the still-live actor, because only a
   proven stale owner can justify connection teardown. Cross-node relay
   proxying is still separate Slice 2+ work.
6. CI exposed stale test fixtures from Slice 1a's live-stealer hardening:
   stale-owner steal tests still modeled "missing owner row is enough" and
   the inline self-fence reclaim test still seeded a claim-only SM session.
   The correction updates tests to seed a fresh stealer `clustering_nodes` row
   before `steal_stale(OwnerStale)` and a durable detached `sm_sessions` row
   before expecting targeted hydration. This changes no production behavior;
   it aligns the tests with the already-landed hardened CAS and XEP-0198
   durable-detach boundary.
7. CI exposed a stale multi-process harness shortcut from before UserActor
   claims were wired: two tests bound `admin@localhost` concurrently on node A
   and node B even though neither assertion was about same-bare-JID
   cross-node routing. With Slice 1b's UserActor single-owner fence, the
   second bind correctly fails until the relay routing slices land. The
   correction seeds a second fixed test account for clustered subprocesses and
   uses that distinct bare JID for the node-B orphan-reaper control session
   and MUC joiner. The clustered subprocess profile marks that second account
   owner-capable so the foreign-owned-room test reaches the RoomRegistry claim
   check instead of being denied by the local instant-room creation guard; the
   receive predicate now fails fast on the wrong presence error instead of
   timing out. This preserves the original SM-claim and foreign-owned-room
   assertions without changing production behavior; the same-bare-JID
   cross-node resume capstone remains covered separately.
8. CI exposed one more stale XEP-0198 CAS interleaving fixture after Slice
   1a's live-stealer hardening: `reaper_wins_mid_resume_interleaving_...`
   seeded a synthetic orphan reaper identity but did not register that
   identity in `clustering_nodes`. The hardened `OwnerStale` CAS now requires
   the stealer itself to have a registered, same-epoch, non-expired,
   non-draining row, so the test setup registers the simulated reaper before
   proving the stale observed epoch loses to the reaper's epoch bump. This
   changes no production behavior.
