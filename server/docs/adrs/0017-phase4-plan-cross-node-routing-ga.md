# ADR-0017 Phase 4 Plan - Cross-Node Routing GA and Helm Unlock

Companion execution note to [ADR-0017](./0017-horizontal-scaling-remote-actors.md),
the [Phase 2 plan](./0017-phase2-plan-libp2p-swarm.md), and the
[Phase 3 plan](./0017-phase3-plan-ownership-claims.md). Status: **ratified;
implementation started with Slice 1**.

Tracking: issue #1195 (ADR-0017 horizontal-scaling epic). Phase 4 is the final
unchecked section: cross-node stanza routing over the ordered relay channel, then
chart/operational unlock for `clustering.enabled` and `replicaCount > 1`.

> **Superseded (2026-08-07):** The ISR (XEP-0397) design and
> `clustering_isr_tokens` store described below have been removed outright — see
> issue #1631 and
> [ADR-011](../../../docs/adr/011-remove-isr-sasl2-inline-future.md). This
> historical plan is retained as a record only.

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

**As-landed Slice 2**: Added the ordered relay substrate only. The new
`ordered_relay` module defines typed channel, per-channel sequence,
origin-inbound SM sequence provenance, sender-asserted origin-node provenance
outside the ordering key, claim-provenance, stanza-envelope, MUC proxy kind,
ACK/NACK, diversion, sender-state, and receiver-state values. The relay actor
now accepts one internal `deliver_ordered` ask that reserves a valid in-order
typed envelope, commits the receiver ACK only after the explicit side-effect
boundary, ACKs exact duplicate retries, and NACKs gaps or malformed typed
payloads. Slice 2's side effect is intentionally empty: it does not
authenticate the sender-asserted origin, validate claims against Postgres, call
`UserActor`, `RoomActor`, `ConnectionRegistry`, pending delivery, or mutate
XEP-0198 state. Sequencing is owned by the caller's shared
`OrderedRelaySenderState`; `RelayHandle` only sends an already-sequenced
envelope and deliberately does not retry ordered delivery after the ambiguous
`ActorStopped` case. Metrics landed only for the real receiver reply path:
low-cardinality internal ordered-relay ACK and NACK counters. The cluster
harness sends ordered envelopes after the existing echo proof and asserts ACK,
duplicate ACK, and gap NACK behavior over the live cross-node relay. Cross-node
DM/MUC/presence/IQ hot-path callers, authenticated-origin comparison, and
Postgres claim validation remain pending.

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
9. Slice 2 intentionally landed less than the original metrics wish-list:
   queue-depth, in-flight, durable-flush-lag, and sticky-diversion operational
   metrics have no production callers in this substrate-only slice, so they
   were not forward-declared. Only the ordered relay receiver's real ACK/NACK
   reply path records metrics, with stable low-cardinality labels. Internal
   ACK/NACK replies are not XEP-0184 delivery receipts, do not synthesize
   client stanzas, and do not advance or inspect XEP-0198 `h`. The envelope
   carries typed origin/target claim provenance now so later routing slices
   can add per-message claim revalidation without a wire-shape break, but
   Slice 2 deliberately performs no claim-store reads and applies no delivery
   side effects.
10. Slice 2 adversarial review found three substrate traps before production
    routing can use the relay: sender sequencing cannot live on per-send
    `RelayHandle` instances, receiver ACK must not commit before the local
    delivery/durable side effect, and envelope origin fields must not be
    mistaken for authenticated transport provenance. The correction keeps
    sequencing in a caller-owned `OrderedRelaySenderState`, adds a
    receiver-side reserve/commit boundary, records the origin stream's inbound
    SM sequence in every envelope, renames the node field to
    `asserted_origin_node`, and bounds per-channel sender/receiver caches with
    cleanup hooks. This changes no client XEP wire behavior; it narrows Slice 2
    to an internal substrate and makes authenticated-origin comparison an
    explicit Slice 3 precondition before any delivery side effect.
11. Slice 2 convergence review found five more substrate traps: ordering was
    accidentally keyed by asserted origin node, malformed envelopes could pair
    a channel for recipient A with payload or claim target B, over-capacity
    backpressure diversion was not sticky after unrelated channel cleanup,
    duplicate ACK replay keyed only by sequence could drop a different payload,
    and ordered delivery retried `ActorStopped` even though that case may have
    been enqueued. The correction removes asserted origin from the channel key,
    validates origin claim, channel recipient, stanza addressing, target claim,
    and MUC proxy type before reservation, stores a typed stanza-element
    fingerprint for recent ACK replay, records backpressure diversion
    stickily, and excludes `ActorStopped` from ordered re-lookup retry. This
    preserves the internal-only Slice 2 scope and avoids changing client XEP
    wire behavior.
12. Slice 2 re-review found that the previous fixes still left executable
    edge cases: the multi-process harness built ordered message stanzas without
    a matching `to`, MUC proxy validation accepted bare/full JID shapes that do
    not match XEP-0045 join/groupchat expectations, repeated overflow channels
    could grow diversion state, duplicate replay still compared mutable
    asserted-origin/claim provenance, and reply-side codec failures were
    converted into receiver NACKs even though the remote handler may have run.
    The correction sets the harness `to`, tightens MUC proxy validation by
    kind, makes overflow a bounded global new-channel backpressure state,
    limits duplicate fingerprints to stable inbound sequence plus typed stanza
    payload, and only maps an unknown ordered-relay message type to an internal
    parse NACK; all other codec failures stay ambiguous relay errors. This
    remains internal relay behavior only.
13. CI exposed that the broad Nix test job compiles the entire
    `waddle-server` lib-test crate in release/all-features mode, which made
    the ordered-relay private invariant tests part of the same LTO-heavy
    monolith that was killed before execution. The correction keeps only the
    release-relevant private invariant probes in that lib-test target, moves
    the public ordered-relay behavior coverage into the dedicated
    `clustering_ordered_relay` integration test target, and leaves the larger
    debug-only in-module matrix as a local regression net. This preserves the
    Slice 2 behavior contract without changing XEP wire behavior or runtime
    requirements.
14. Slice 3 started with an intentionally narrow full-JID production pass
    instead of the whole 1:1/bare-JID matrix. The as-landed first pass adds a
    construction-order `OrderedRelayDeliveryBridge`, wires it into the relay
    actor, revalidates origin SM and target UserActor claims before applying a
    local `TrySendPeer` effect, and commits the receiver sequence only after
    live delivery, detached queueing, or a terminal drop. Origin-side
    full-JID routing attempts the relay only when a live SM origin context and
    fresh foreign UserActor claim are present; otherwise it keeps the existing
    local path. Receiver `Unavailable` NACKs flow back to the origin so the
    existing undeliverable full-JID IQ fallback, including Jingle
    `session-terminate` empty-result ack behavior, remains the only
    client-visible reply shape. Bare-JID fan-out, full multi-process DM proof,
    XEP-0198 pending-handoff counter tests, and MUC/presence routing remain
    open Slice 3/4 work, not silently claimed by this partial pass.
15. Slice 3 adversarial review found that Postgres claim checks alone did not
    prove the libp2p sender was the claimed owner. The correction adds a
    signed `origin_proof` to `RemoteStanzaEnvelope`, binds node leases to the
    swarm `PeerId`, and verifies the signature, public key, derived `PeerId`,
    current owner claim, and current target claim before any receiver-side
    delivery effect. The proof is internal relay metadata only; it is never
    serialized into client-visible XMPP XML.
16. Slice 3 XEP/convergence review found three wire-semantics traps in the
    first full-JID pass: some definite no-effect relay failures for full-JID
    IQs could be collapsed into `Dropped` instead of the existing typed IQ
    fallback path, delivery receipts generated on a remote recipient node did
    not inherit an ordered-relay origin context for cross-node return routing,
    and MUC proxy `RoomIq` did not distinguish bare-room IQs from
    occupant-directed IQs. The correction classifies only definite no-effect
    ask/NACK failures as `Unavailable` for IQ fallback while preserving
    ambiguous-timeout `Dropped`, propagates the SM-derived relay origin through
    `PeerStanza` outbound interpretation, and splits MUC IQ proxy validation
    into `BareRoomIq` and `OccupantIq` with bare/full address-shape tests.
    This preserves client-visible XEP-0184, XEP-0198, XEP-0045, and IQ
    semantics while tightening the internal relay contract.
17. Slice 3 correctness/XEP review then found that no-commit ordered failures
    could rewind sender sequence state, ordered delivery awaited local effects
    inside the single relay actor mailbox, the default ordered-delivery ask
    budget could outlive the WebSocket stanza wedge backstop, and receiver-side
    `Dropped` peer delivery was being ACKed as if the recipient node had taken
    responsibility. The correction sticky-diverts the origin channel on
    NACK/ask failure instead of deleting sender sequence state, delegates
    `RelayDeliverOrdered` replies through `Context::spawn` while keeping
    reserve/commit serialized behind the receiver state, caps full-JID ordered
    delivery asks below the stanza backstop, maps receiver `Dropped` to a
    `Backpressure` NACK, and moves live SM inbound-counter advancement until
    after stanza dispatch returns while carrying the prospective count in the
    relay envelope. This reduces false ACK/loss and actor head-of-line
    blocking, but it does not complete the larger GA work for asynchronous
    pending-handoff SM accounting, immediate `<r/>` while a handoff is still
    pending, non-SM server-assigned origin provenance, or stale-owner resend to
    a fresh foreign owner; those remain open before Helm unlock.
18. Slice 3 follow-up implements the SM-backed GA blockers from item 17:
    the origin WebSocket now reserves XEP-0198 inbound handled-count slots per
    stanza, defers completion when ordered relay takes responsibility in a
    background task, keeps `<r/>` responses immediate with `h` advancing only
    after contiguous handoff completion, and waits for pending handoff
    completions before SM cleanup snapshots a dead transport. A later review
    removed both the spawned-handoff timeout and the cleanup drain timeout:
    those timeouts could turn a still-running relay handoff into a stale
    XEP-0198 detach snapshot. The relay ask path itself remains bounded and
    classifies definite no-effect failures with typed outcomes.
19. Slice 3 ordered-channel hardening now makes target ownership part of the
    channel identity via `target_epoch`, splits relay `NotOwner` NACKs by
    origin-vs-target claim role, retries the current stanza once on a fresh
    foreign target-owner channel for `NotFound`/`NoEffect` failures, and keeps
    ambiguous `MaybeCommitted` failures terminal. Per-channel sender locking
    serializes origin-side sequence allocation and relay asks for one lane, with
    a bounded lock map and sticky diversion on backpressure. Receiver-side
    reservations are marked in flight before local delivery effects run, so
    duplicate same-sequence asks cannot execute the receiver effect twice while
    the first commit is pending.
20. Slice 3 protocol scope was tightened rather than broadened by the final
    XEP review: groupchat messages are excluded from the generic full-JID
    bridge and remain Slice 4 MUC proxying work, and non-SM server-generated
    side routes stay local instead of inventing a `UserActor` provenance lane.
    XEP-0184 receipts can still return cross-node when they inherit the
    original SM-backed ordered-relay origin through the recipient pass. The
    real multi-process capstone now provisions the claim schema, signs
    proof-valid ordered envelopes, registers the synthetic origin node, and
    targets a real resource connected on the receiving node. Origin `NotOwner`
    remains a terminal provenance failure; broader bare-JID/MUC/presence
    routing remains open Slice 3/4 work.
21. Slice 3 convergence review found the remaining 1:1 GA gaps in the first
    hot-path patch: bare-JID target-owner refresh could fall back to
    unavailable instead of local RFC 6121 fan-out when the fresh owner was this
    node, receiver-side bare-JID delivery had no local-dispatch timeout, the
    generic user-message relay path accepted `groupchat`, bare-JID channels
    accepted full-JID payloads whose bare form matched, and full-JID IQ
    receiver delivery bypassed the recipient `PeerStanza` pass. The correction
    routes bare-JID DM/IQ/presence payloads over the same ordered relay channel
    using the exact bare recipient as the channel identity, rejects groupchat
    outside the MUC proxy payload, wraps receiver-side local bare-JID dispatch
    in a bounded timeout, maps local fallback replies back to the origin's
    existing IQ fallback behavior, and sends full-JID IQs through
    `UserActor::TrySendPeer` so XEP-0191, XEP-0359, XEP-0313, XEP-0280, and
    inbox projection stay on the normal recipient path. A boxed future boundary
    in `route_to_connection` is compile-structure only; it does not change
    XMPP wire behavior.
22. Slice 3 receiver timeout review split no-effect backpressure from
    ambiguous receiver effects. The relay now bounds the entire receiver-side
    reserved effect before committing an ACK, but timeout after validation or
    local dispatch has started NACKs as internal `MaybeCommitted`, not
    `Backpressure`, and preserves that provenance in the sticky receiver
    diversion so replays remain conservative. The origin maps direct
    `MaybeCommitted` and `Diverted(MaybeCommitted)` to `Dropped`, avoiding
    synthesized IQ fallback or retry assumptions for a stanza that may already
    have archived, updated inbox state, emitted carbons, or reached a live
    resource. Pre-effect failures such as full outbound channels still use
    `Backpressure` and remain definite no-effect.
23. Slice 4 MUC proxying now carries receiver-built client replies in the
    ordered-relay ACK so remote room owners remain the single writer for join
    presence and groupchat reflection. Receiver duplicate ACK replay includes
    the same typed reply stanzas, preserving ordered-channel idempotence. MUC
    join proxying reuses the owner node's `handle_muc_join` path, and
    groupchat proxying reuses the room dispatch path with a room-entity relay
    origin so MUC occupant fanout can itself route cross-node. The proxy keeps
    the conservative `MaybeCommitted` no-reply behavior for fire-and-forget
    room message/presence effects. Remote joins make one repair attempt on a
    distinct UserActor-origin channel; if commit state remains ambiguous, the
    origin records the possible remote membership for later unavailable
    cleanup and emits no false join-failure stanza.
24. Slice 4 presence fanout now threads the inbound ordered-relay origin
    through regular presence broadcast, directed presence, subscription
    presence side effects, and probe responses. Those paths first try the
    ordered relay when the recipient bare/full JID is owned by a fresh foreign
    `UserActor`; otherwise they keep the existing local registry and detached
    SM-storage behavior. IQ blocking side effects and terminated-session
    unavailable broadcasts have no inbound relay origin, so they remain local
    until a separate server-origin provenance lane is introduced.
25. Slice 7 chart unlock landed without Helm hooks because the repo's GitOps
    validation still rejects hook/bootstrap tokens. The chart instead enforces
    the production invariant at render time: multi-replica requires
    `clustering.enabled=true`, Postgres, shared uploads via S3/RWX, at least
    one listen address, and an enrolled-key count of `replicaCount + maxSurge
    + keypairHeadroom` when the pool is supplied by an external Secret. The
    render now emits the swarm port, headless Service, PDB, soft anti-affinity,
    and a NetworkPolicy that keeps normal HTTP ingress open while restricting
    the swarm port to matching server pods.
26. Production unlock uses chart `0.4.x`, `replicaCount: 2`,
    `clustering.enabled: true`, `enrolledKeyCount: 4`, RollingUpdate
    `maxSurge: 1`, and disables the RWO PVC because uploads are already on
    R2/S3. The generated keypool was written with the
    `waddle-cluster-keypool` ops binary to a 0600 file, the non-secret peer
    IDs were enrolled into `clustering_peer_allowlist`, and a cluster-local
    `waddle-clustering-keypool` Secret was created because the local
    1Password CLI could not connect to the desktop app. The permanent follow
    up is to backfill the same pool value into the `server-runtime-production`
    1Password item (property `clustering-keypair-pool`) or replace the bridge
    Secret with an encrypted GitOps-managed secret source.
27. Slice 4 convergence review found that MUC target-owner refresh still
    collapsed ACK reply stanzas and ambiguous commit state into a plain
    delivery enum. The retry path now returns the full remote-delivery outcome,
    and if the refreshed room owner is this node it runs the MUC proxy receiver
    effect directly instead of falling into generic bare-JID routing. This
    preserves remote join self-presence replies and avoids synthesizing a local
    MUC error for room work that may already have committed.
28. Slice 4 convergence review also found that unavailable MUC presence and
    in-room occupant presence updates were still local-only. The origin now
    proxies explicit MUC leave and non-join occupant presence over the
    `OccupantPresence` MUC proxy class, the receiver dispatches that class to
    the existing leave/update handlers, and MUC leave/Muji-clear fanout uses
    the room-origin routing helper so remaining occupants on other nodes get
    the XEP-0045 unavailable or reflected presence. Relayed groupchat now
    reconstructs the authenticated sender session before entering the room
    dispatcher so managed announcements rooms keep their server-owner gate.
29. Slice 1/7 operational convergence removed runtime DDL from clustering
    allowlist startup. The production swarm now only reads
    `clustering_peer_allowlist`; schema creation remains in the control-plane
    provisioning path (`waddle-cluster-keypool` SQL output and harness setup),
    so the application role can be hardened to `SELECT` on the allowlist table
    without breaking startup.
30. Slice 4 final convergence added the missing remote-MUC cleanup and
    per-message enrollment checks. Successful remote joins are tracked in a
    process-local full-JID/room membership index; explicit remote leaves clear
    it only after the owning room actor ACKs, and unclean disconnect/SM-expiry
    drains it by relaying XEP-0045 unavailable presence to each remote room
    owner. Remote leave failures now return a retryable presence error instead
    of falsely acknowledging self-unavailable. Ordered-relay receiver
    validation also re-reads `clustering_peer_allowlist` for every signed
    envelope so a revoked PeerId is rejected before delivery even if the swarm
    refresh has not closed the transport yet.
31. The MUC join `MaybeCommitted` case now keeps receiver-side JoinPresence
    timeouts retryable, replays the same ordered channel once, and only then
    attempts a distinct UserActor-origin repair channel. If neither path proves
    delivery and returns the required roster/status-110 self-presence, the
    origin records the remote membership for cleanup provenance and returns a
    retryable presence error to the client instead of silently hanging the join.
    A later explicit leave, unclean disconnect, SM expiry, or client retry can
    then relay the authoritative unavailable cleanup or complete the idempotent
    join against the remote room owner.
32. Remote MUC disconnect cleanup now treats only an ordered-relay `Delivered`
    ACK as cleanup success. `MaybeCommitted`, `JoinMaybeCommitted`,
    `Unavailable`, `Dropped`, and absent bridge outcomes re-record the
    full-JID/room membership after logging, so uncertain unavailable delivery
    remains eligible for a later cleanup retry instead of deleting the only
    provenance for a possible remote occupant.
33. Directed presence probes and subscription requests now cross the ordered
    relay before local fallback when the target UserActor claim is fresh on a
    different node. The receiver handles Probe/Subscribe/Subscribed/Unsubscribe
    /Unsubscribed as server-side presence requests on the target owner, not as
    direct client frames, so probes are answered from the target node's live and
    detached resources and pending subscription queues remain target-local.
34. Remote MUC disconnect cleanup no longer depends on a still-live resource
    entry keeping the sender's UserActor claim. Before sending remote
    unavailable cleanup, the janitor reacquires the bare UserActor claim through
    the registry, uses that claim as ordered-relay provenance, then asks the
    registry to reap the UserActor only if it is still empty. This covers normal
    full cleanup after mirror-unregister and SM-expiry cleanup after the original
    resource claim was already pruned.
35. Subscription relay convergence keeps sender-local side effects on the
    sender's owner node after the target owner commits the RFC 6121 request over
    ordered relay. The origin node now replays the sender roster push from the
    shared database, clears pending subscribe state for approval/denial, and
    relays the required current/unavailable presence side effects for
    Subscribed/Unsubscribed. The multi-process harness asserts the cross-node
    Subscribe, sender `ask='subscribe'` push, Subscribed approval push, current
    presence catch-up, and an authorized cross-node Probe reply.
36. Remote MUC cleanup provenance was tightened beyond deviation 34 after
    adversarial review: teardown paths now preserve an already-owned route
    origin instead of relying on best-effort reacquisition after releasing it.
    Full disconnect and SM-detach-failure cleanup run before UserActor
    mirror-unregister using that UserActor claim; SM-expiry cleanup runs before
    `pending_delivery.release_claim` using the expired SmSession claim. The
    reacquire-and-reap path remains only as a fallback for callers without
    explicit provenance. The multi-process MUC test now closes a foreign-node
    occupant and asserts the room owner receives the relayed XEP-0045
    unavailable presence.
37. Relay discovery misses (`RelayAskError::NotFound`) are now classified as a
    definite no-effect delivery miss without sticky-diverting the ordered
    channel. The bridge still refreshes the target owner claim and falls back
    for the current stanza, but a transient Kademlia/relay lookup gap no longer
    poisons later sends on the same origin-target channel after discovery
    converges. Transport, timeout, cancellation, and backpressure failures keep
    their existing channel-diversion behavior.
38. Relay names are now re-registered immediately after a new peer connection,
    not only on the 15s supervisor refresh cadence. A relay registration made
    before the first peer connection can otherwise remain local to the node's
    Kademlia view, producing directional `NotFound` misses even though the
    target owner claim is fresh in Postgres. The periodic refresh remains as a
    fallback; the peer-triggered refresh makes newly connected nodes usable for
    ordered relay before cross-node stanza routing starts relying on them.
39. The recipient state machine now wires peer-routed presence stanzas to the
    wire after applying the same session blocklist guard as peer-routed IQ.
    Cross-node subscription approval depends on current-presence catch-up
    (`<presence from='contact/full' to='subscriber'/>`) arriving as a
    `PeerStanza`; before this fix the message recipient-pass handled messages,
    IQs were direct, and presence was only debug-logged and dropped. Dedicated
    machine tests cover peer-routed presence delivery and blocked-sender drop.
40. The ordered-relay receiver now delivers plain bare-JID presence side
    effects as direct server frames to the target user's available/detached
    resources instead of running them through the generic `PeerStanza`
    recipient-pass route. Subscription/probe requests still use the
    server-side presence request handler, and messages/IQs keep their existing
    recipient-pass semantics. This preserves RFC 6121 current/unavailable
    presence catch-up after cross-node subscription approval while keeping the
    delivery path actor-backed through `UserRegistryActor`.
41. Ordered-relay envelopes now carry a signed `sender_claim` for the
    UserActor/RoomActor named by the stanza's `from`, separate from the
    origin lane claim. SM-origin channels keep ordering on the stream id, but
    receivers reject any envelope whose stanza `from` does not match the
    sender claim or whose sender claim is not fresh under the same node
    identity as the origin claim. This closes the enrolled-node forged-from
    gap found in council review and is covered by substrate and bridge
    provenance tests.
42. Deferred XEP-0198 handoffs no longer have an outer spawned-task timeout.
    The relay ask, mailbox, reply, and receiver effect operations remain
    bounded, but the origin's handled counter now advances only when a real
    relay outcome completes. This avoids falsely marking an inbound stanza
    handled while the receiver effect may still commit.
43. Clustered startup now fails when `WADDLE_CLUSTERING_KEYPAIR_POOL` is set
    but any derived pool PeerId is absent from `clustering_peer_allowlist`.
    The runtime still remains SELECT-only on the allowlist table; the preflight
    turns a missed GitOps/SQL enrollment step into an explicit startup/readiness
    failure instead of a deny-all, split-brain-looking cluster with green
    manifests.
44. Council review found deviation 37 was incomplete for current bare-JID
    stanzas: `RelayAskError::NotFound` no longer returns a terminal `Dropped`
    outcome to the route hook. It now returns a bridge miss (`None`) so the
    current stanza continues through the normal local/headless/durable fallback
    path. A second council pass found the miss still advanced the sender
    sequence, so definite no-effect lookup misses now roll back the unseen
    envelope sequence and target-refresh only forgets the old channel after an
    actual owner/epoch change or local takeover. Tests cover both a cold lookup
    miss retrying at sequence 1 and an established channel retrying the missed
    sequence 2 after sequence 1 was ACKed.
45. Council review tightened cross-node presence side-effect consumption:
    presence relay helpers now treat only `Delivered` and `QueuedDetached` as
    consumed. `Unavailable`, `Dropped`, and bridge misses fall back to the
    caller's normal RFC 6121 handling, preventing current-presence, probe, and
    subscription side effects from being silently acknowledged after an ordered
    relay NACK or lookup miss.
46. XEP-0045 MUC presence dispatch now rejects typed room/occupant presence
    unless it is `type='unavailable'`. Join and in-room update paths accept
    only available presence (`type` absent), so typed stanzas such as
    `type='probe'` cannot be interpreted as room entry or occupant activity.
    Ordered-relay `InFlight` NACKs are also classified as maybe-committed but
    explicitly disallow MUC join repair, suppressing local fallback without
    racing a duplicate room mutation against the pending receiver effect.
47. Remote bare-JID presence delivered by the ordered-relay receiver now
    re-validates the recipient's XEP-0191 blocklist before any live-resource
    write or detached XEP-0198 replay. A blocked sender is silently dropped
    with no delivery effect, matching normal presence privacy semantics; a
    blocklist storage failure is treated as `InFlight` so the relay remains
    fail-closed instead of leaking presence.
48. Clustering now treats an empty keypair pool as a configuration error, not a
    cue to mint an ephemeral runtime identity. The runtime raises
    `KeypairPoolRequired`, Helm render validation requires either an inline
    keypair pool or an external secret source whenever `clustering.enabled=true`,
    and Postgres-backed keypair-slot tests share the cluster test lock to keep
    the real multi-process harness deterministic.
