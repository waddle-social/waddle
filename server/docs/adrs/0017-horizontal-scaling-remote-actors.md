# ADR-0017: Horizontal Scaling via Remote Actors with Postgres-Authoritative Ownership

> **Superseded in part (2026-08-07):** the XEP-0397 ISR requirements in this
> ADR (element 10 and related roadmap items) have been removed outright — see
> issue #1631 and
> [ADR-011](../../../docs/adr/011-remove-isr-sasl2-inline-future.md). The rest
> of this ADR remains in force.

## Status

Accepted (consensus of the adversarial review council: six domain reviewers —
XMPP/XEP conformance, distributed systems, Rust/kameo/libp2p, SRE/Kubernetes,
storage, security — approve with no blocking or major findings remaining)

## Context

Waddle currently runs as a single stateful replica by design. The Helm chart
pins `replicaCount: 1` and `templates/validations.yaml` fails rendering when
`replicaCount > 1` with a ReadWriteOnce PVC. All real-time routing state is
process-local:

- **Connection routing**: `ConnectionRegistry.connections: DashMap<FullJid,
  ConnectionEntry>` maps JIDs to in-process `mpsc::Sender<OutboundStanza>`
  channels (`waddle-xmpp/src/registry/connection_registry.rs`). There is no
  JID→node lookup; targets with no local connection fall through to
  detached-session queueing or offline storage. The delivery path encodes
  hard-won invariants (see Phase 1) that any replacement must preserve.
- **MUC rooms**: each room is a single in-memory kameo `RoomActor` owning a
  `MucRoom` with occupant state (`muc/room.rs`, `muc/room_registry.rs`).
  Occupant state is not persisted anywhere.
- **Presence**: `presence_states: DashMap<FullJid, PresenceState>` in the
  `ConnectionRegistry`, never shared or persisted.
- **XEP-0198 sessions**: `InMemorySmSessionRegistry` with a durable
  SQLite/Postgres mirror (`sm_sessions`/`sm_unacked`). Durable rows are
  written **only at detach** (`websocket/cleanup.rs` → `store_session`);
  live streams' `h` counters and unacked queues are in-memory only.
  `<resume/>` only consults the local in-memory registry. Startup
  `restore_from_persistence` hydrates **all** rows via an unscoped
  full-table read, and the SM-expiry janitor promotes from that local view —
  both assume exclusive ownership of the durable tables. Durable-write
  atomicity relies on **process-local** lock maps (`sm_persistence.rs`
  `StreamLockMap`, `session_registry/core.rs` shard mutexes) that serialize
  nothing across replicas.

Restart resilience for the single-node case: durable XEP-0198 resumption,
graceful SIGTERM drain that emits RFC 6120 `<system-shutdown/>` stream errors
and RFC 7395 `</close>` frames, Q6 promotion of unacked stanzas into durable
offline delivery, MAM (XEP-0313) catch-up, and a client with
exponential-backoff reconnect and persisted SM resume state.

XEP-0397 ISR is **not** part of that story today: `IsrTokenStore` is a
process-local `RwLock<HashMap>` (`waddle-xmpp/src/isr/store.rs`) that does
not survive even a single-node restart, and `validate_token`/`consume_token`
are not wired into the live resume path. ISR is treated below as
not-yet-implemented, not as an existing resilience mechanism.

The gap is any cross-node fabric. Two users on different replicas cannot
exchange stanzas; two replicas would each materialize their own copy of the
same MUC room; the shared durable SM tables would be double-hydrated and
double-promoted.

ADR-0008 adopted kameo as the actor framework. Kameo's `remote` feature
(disabled today; `kameo = "0.20"` without libp2p in the lockfile) provides
`RemoteActorRef` lookup by registered name over a libp2p swarm with
serde-serialized typed messages, plus a kademlia-based registry. Three
realities constrain the design: kameo remote delivery is libp2p
request-response with **no ordering guarantee between tells** to the same
actor; `remote::bootstrap()` is an mDNS development helper — production
use means composing `kameo::remote::Behaviour` into a hand-built libp2p
swarm we own and operate; and kameo 0.20's registry hardcodes
`kad::store::MemoryStore` with **default limits (1024 provided keys, 1024
records) and no configuration surface**, while each registration consumes
one provider key plus one metadata record — so per-entity DHT registration
cannot reach even 2% of the modeled scale (50k users + 5k rooms). Neither
`xmpp-parsers` 0.22 nor `minidom` 0.18 implements serde, so every remote
payload needs an explicit codec.

## Decision

Scale horizontally using **kameo remote actors as the inter-node transport**
(with kademlia providing **node discovery only, never entity routing**),
**Postgres as the authoritative ownership and routing store**, **epoch-fenced
writes ordered by row locks on the claims table**, and **at-least-once
ordered inter-node delivery**.

Key elements:

1. **Postgres-only for multi-replica.** Clustering is gated on the Postgres
   backend; SQLite remains supported for single-node deployments only.
   Ownership claims are exposed through a `ClaimStore` trait with exactly two
   implementations: the existing in-process claim logic
   (`session_registry/claims.rs` semantics) for single-node/SQLite
   deployments, and a Postgres-only implementation using server-time CAS SQL
   (element 4) that deliberately bypasses the portable statement layer
   (`rewrite_positional_for_postgres`), because `now()`-based predicates and
   row-level CAS have no meaningful SQLite equivalent. The Postgres
   implementation gets Postgres-backed integration tests (testcontainers) in
   CI; the portable layer is never asked to express claims SQL.

   The same two-implementation split applies to **SM persistence**, because
   epoch fencing (element 4) is a property of the write path, not of the
   claims schema, and `FOR SHARE` row locks do not exist in SQLite. The
   portable `DatabaseSmPersistence` (`sm_persistence.rs`, `sm_promotion`)
   remains the single-node implementation, untouched — its exclusivity
   guarantee stays process-level. Cluster mode selects a **Postgres-only
   fenced implementation of `SmPersistenceStorage`** — a full second
   implementation of the trait built on `Database::begin`, **not a
   decorator**: the portable impl acquires a pooled connection per
   statement (`ConnectionGuard`), so no wrapper can place the `FOR SHARE`
   fencing lock in the same transaction as the inner impl's writes, and
   multi-statement methods (`delete_session`, `store_session_atomic`,
   `record_promotion_failure`) each need the fencing SELECT inside one
   `Transaction`. Two trait-shape divergences from the portable impl are
   explicit and accepted: (a) detach writes ignore the caller-supplied
   `detached_at` and stamp/re-read Postgres `now()` (element 4), so
   `PersistedSession.detached_at` as read back is the DB value, not the
   caller's; (b) expiry listing evaluates the window in SQL against
   Postgres `now()`, treating the trait's `now` parameter as advisory in
   the fenced impl. The portable impl and schema remain byte-identical for
   SQLite. The fenced impl is chosen when `clustering.enabled`. Cluster
   mode never routes SM writes
   through the portable layer; no conditional SQL is scattered into shared
   statements. This split is a named Phase 3 deliverable, not an
   implementation surprise.

2. **Finish the actor migration first (single-node refactor).** Route live
   delivery through `UserActor` (actor-per-bare-JID; today a state store for
   resources/presence/carbons with tests, but with **no delivery surface**
   and not wired into `waddle-server`) and introduce a connection actor per
   WebSocket pinned to the node owning the TCP socket. Retire the
   `ConnectionRegistry` DashMap delivery path only after the invariants it
   encodes are reproduced and tested (Phase 1 lists them as acceptance
   criteria). Connection actors are spawned with an **explicit named mailbox
   capacity** `CONNECTION_ACTOR_MAILBOX_CAPACITY = 256` (kameo's default
   spawn mailbox is 64, which would drop on any burst over 64 — e.g. a
   200-occupant join fan-out — where today's `mpsc(256)` does not; the
   constant mirrors the reviewable `ROOM_REGISTRY_MAILBOX_CAPACITY`
   pattern), and every fan-out path uses `try_send` with a typed drop
   outcome — never an awaited `tell().send()` per occupant, which would
   reintroduce the #699 zombie-peer stall.

3. **Enable kameo `remote` as an owned subsystem, with peer authorization.**
   This is not "a feature flag": waddle-server gains a swarm module that
   composes `kameo::remote::Behaviour` via `derive(NetworkBehaviour)`, builds
   transports, manages a per-pod keypair, dials peers resolved from a
   Kubernetes headless Service (no mDNS), runs kademlia bootstrap, and drives
   the swarm event loop. Node identity and trust:
   - **Per-pod swarm identity is a leased slot from a pre-enrolled keypair
     pool.** A Deployment cannot mount distinct per-pod Secrets — every
     replica mounts the identical pod template — so "one static keypair
     Secret per pod" is unimplementable on this chart; a single shared
     keypair would collapse all replicas onto one libp2p PeerId (breaking
     kademlia discovery, connection bookkeeping, and per-pod revocation),
     and startup-generated keys can never be pre-enrolled by a pipeline
     that runs before the pod exists. Instead the chart mounts **one
     Secret containing a pool of ≥ `maxReplicas + maxSurge` enrolled
     keypairs** (with **headroom ≥ 1 above `maxReplicas + maxSurge`**,
     default `+ceil(churn)`); at startup each pod leases exactly one slot
     via a
     Postgres CAS (`keypair_slots`, a heartbeat-renewed lease of the same
     shape as the `nodes` lease, landing with the Phase 2 swarm
     subsystem) and releases it on drain. The headroom exists because a
     **hard-killed** pod (drain overrun → SIGKILL, or node loss) never runs
     its slot release, so its lease stays non-stealable until the slot-lease
     TTL lapses; sizing the pool at exactly `maxReplicas + maxSurge` would
     let a replacement pod — which Kubernetes keeps inside that same
     envelope — find every fresh slot taken and crash-loop until the dead
     lease expires, right when the cluster is already down a pod. With
     headroom the replacement leases immediately; without it,
     replacement-pod startup is bounded by (and is documented as) the
     slot-lease TTL. The CAS guarantees at most one
     live leaseholder per slot, so no two concurrent swarm members ever
     share a PeerId — including the old+new pod overlap of a
     RollingUpdate with surge; as defense in depth, the swarm behaviour
     layer rejects a second live session for an already-connected PeerId.
     **Scale-up beyond the enrolled pool requires a pipeline enrollment
     run first** — a documented operational constraint — and the Phase 4
     chart ships the pool-Secret templating plus the enrollment Helm hook
     job with its own admin-role Secret. To keep this constraint from being
     convention-only, Phase 4 **pins `maxSurge`/`maxUnavailable` as chart
     values** and adds a `validations.yaml` render-time assert that the pool
     Secret's enrolled-key count `≥ replicaCount + maxSurge + headroom`
     (the same encode-invariants-in-the-chart pattern as Phase 0), so an
     operator raising `maxSurge` without a prior enrollment run fails
     `helm template` rather than crash-looping surge pods mid-deploy. (Alternative considered: a
     StatefulSet with per-ordinal Secrets dissolves the duplicate-PeerId
     window structurally, but forces reworking the Deployment-based
     rollout-placement design and the storage story; the pool lease
     reuses CAS machinery this ADR already owns. Tradeoff recorded.)
     The cluster maintains an **allowlist of enrolled peer IDs** (rows in
     Postgres, refreshed periodically). Connections from peer IDs not on
     the allowlist are rejected at the swarm behaviour layer — completing
     the Noise handshake is necessary but never sufficient.
   - **Enrollment authority is separate from the runtime DSN.** Allowlist
     rows are provisioned only by an authority distinct from the shared
     runtime database credential: an operator/admin-only Postgres role used
     by the deployment pipeline (CI/CD-managed migration or Helm hook job
     with its own Secret), with the runtime role granted `SELECT` but not
     `INSERT`/`UPDATE`/`DELETE` on the allowlist table. A running node can
     never enroll itself or any other peer; otherwise any process holding
     the runtime DSN (i.e. any compromised pod) could mint swarm members
     and the allowlist would add nothing over Noise. Key rotation = enroll
     new key via the pipeline, roll the pod, revoke old key.
   - **Revocation acts on live connections, not only new dials.** Each
     periodic allowlist refresh diffs the enrolled set and actively
     closes/bans existing swarm connections whose peer ID is no longer
     enrolled (libp2p deny-list behaviour plus explicit disconnect), and
     every inbound remote message re-checks that the origin peer is
     *currently* allowlisted — not merely that it presents a valid claim
     epoch. Gating only new dials would make revocation a no-op against
     an active attacker whose connections predate the row deletion —
     exactly the case revocation exists for. Containment bound: a revoked
     peer's swarm access ends within one refresh interval, asserted by a
     Phase 2 fault-injection case (revoked-peer-with-live-connection).
   - The **remote-actor boundary is an untrusted ingress**, not a
     pre-authorized internal channel. Every inbound remote message carries
     the authenticated origin node identity and the sender's claim epoch; the
     receiving node re-validates that the origin node currently holds the
     Postgres claim relevant to the message, and re-derives/enforces the
     stanza `from` (a remote node may only originate stanzas from JIDs whose
     sessions it owns). Deserialization is bounded: max envelope size,
     collection-length caps, **and a maximum XML nesting depth plus
     attribute/namespace count caps enforced before tree construction**
     (minidom parsing is recursive; a small deeply-nested payload must be a
     typed re-parse NACK, not a stack overflow that crashes the node and
     forfeits its claims). This caps DoS blast radius from a compromised
     peer; the honest trust statement for a compromised *enrolled* peer is
     recorded in Consequences.
   - Kademlia carries **node discovery only** (element 6); entity routing is
     resolved from Postgres. A forged or stale DHT record can therefore at
     worst delay dialing a peer, never attract or misroute entity traffic.

   Because neither `xmpp-parsers` nor `minidom` implements serde, the remote
   codec is an explicit deliverable: XML-text serde wrappers for
   `Stanza`/`Element` (round-tripping through `Stanza::to_element`, including
   the `ensure_thread_element` fixup) with a **typed re-parse error path** and
   drop metrics — a payload that fails to re-parse on the receiving node is a
   NACK to the sender, never a silent drop. `RoomActor` does **not** get serde
   over its 20+ local message/reply types; MUC proxying uses a dedicated,
   small, remote-safe message set (element 7).

4. **Postgres-authoritative ownership with per-node leases and epoch
   fencing.** Two tables:
   - `nodes (node_id, node_epoch, heartbeat, expired, pod_template_hash)` —
     one liveness row per replica, one heartbeat CAS per node per interval.
     `expired` is the **committed, monotone expiry fact** (see *Expire*
     below): no other statement ever infers staleness from a raw
     `heartbeat` comparison.
     Heartbeats are **per node, not per entity**: at 50k users + 5k rooms a
     per-entity heartbeat would be ~thousands of liveness UPDATEs/sec;
     per-node is O(replicas). `node_id` is **per pod instance** — freshly
     generated on every process start, paired with a fresh `node_epoch`;
     it is never reused across restarts. Dead `nodes` rows are
     garbage-collected only after no claims reference them (claims are
     reaped first), so the orphan reaper's staleness decisions never depend
     on a row that has vanished.
   - `claims (entity, entity_type, node_id, node_epoch, claim_epoch, ...)` —
     which node owns a given `UserActor`, `RoomActor`, **SM session (live or
     detached — the claim row is created at `<enable/>` time, element 8)**,
     or ISR token scope. A claim is stale iff its node's liveness row is
     expired (the committed flag), **missing**, or its `node_epoch`
     predates the node's current registration. The stale predicate is
     written as a `LEFT JOIN` against `nodes` with `nodes.node_id IS NULL
     OR nodes.expired OR node_epoch mismatch` — never a raw
     `heartbeat < now() - ttl` comparison, which would be an unordered
     snapshot read (see *Expire*) — so a claim whose owner row never
     registered or was reaped is definitionally stale rather than
     permanently unstealable.

   The exact SQL contract (so "transactional" cannot be implemented as a
   broken READ COMMITTED SELECT-then-UPDATE):
   - *Acquire*: `INSERT ... ON CONFLICT (entity) DO NOTHING` +
     `rows_affected == 1` check.
   - *Expire* — the single serialized ordering point that makes lease
     expiry genuinely monotone: before any steal or reap that relies on
     owner staleness, the stealer/reaper first executes
     `UPDATE nodes SET expired = true WHERE node_id=$owner AND
     node_epoch=$theirs AND NOT expired AND heartbeat < now() - $lease_ttl`
     and proceeds only if it committed (or the flag is already committed
     `true`). Because this CAS and the heartbeat renewal write the **same
     `nodes` row**, row-lock ordering serializes them and EvalPlanQual
     re-evaluates the blocked statement against the new committed row
     version — the same EPQ fact that makes lockless *join* fencing
     unsafe makes same-row CAS safe. Renewal-commits-first ⇒ the expire
     CAS re-evaluates, sees a fresh heartbeat, returns 0 rows ⇒ no steal.
     Expire-commits-first ⇒ the renewal's `AND NOT expired` fails ⇒
     0 rows ⇒ the owner demotes. A prior revision claimed the renewal
     predicate alone made expiry monotone; it does not: a renewal whose
     transaction-fixed `now()` was evaluated pre-expiry (e.g. its commit
     delayed by an fsync/replication stall — exactly the overloaded-DB
     condition that also makes heartbeats late) can commit *after* a
     steal that judged the owner stale from an earlier snapshot. Both
     statements then succeed — they lock disjoint rows, so Postgres never
     ordered them — and the owner sees `rows_affected == 1` ("lease
     intact") while serving stolen claims with zero signal, resurrecting
     exactly the half-reaped/ghost-presence states the design must
     exclude. The `expired` flag closes that interleaving; it is a named
     Phase 3 race test (renewal evaluated pre-expiry, committed
     post-steal, must return 0 rows). `expired` is cleared only by
     explicit re-registration under a fresh `node_id`/`node_epoch` —
     never in place.
   - *Steal (stale owner)*: single-statement CAS —
     `UPDATE claims SET node_id=$me, node_epoch=$my_node_epoch,
     claim_epoch = claim_epoch + 1 WHERE entity=$e AND claim_epoch=$observed
     AND <owner-stale LEFT-JOIN predicate over nodes>` with
     `rows_affected == 1` required. Losers observe 0 rows and give up.
     The join read of `nodes` inside this CAS is safe **only because it
     reads the committed `expired` flag** — monotone by the *Expire*
     contract above — and never re-derives staleness from `heartbeat`
     directly.
   - *Steal (consent/epoch-only — SM resume paths exclusively)*: element
     8's two resume paths steal from owners whose lease is **fresh**,
     which the owner-stale predicate can never authorize, so they use a
     third CAS variant with no staleness predicate:
     `UPDATE claims SET node_id=$me, node_epoch=$mine,
     claim_epoch = claim_epoch + 1 WHERE entity=$e AND
     claim_epoch=$observed` — authorized exclusively by the
     identity-bound resume path (detached-session steal) or by the old
     owner's handshake ack (live-session steal). The epoch CAS resolves
     two simultaneous resumes: the loser observes 0 rows and answers
     `<failed/>`; if the owner acks the first handshake and a second
     requester races it, the second falls back to the detached path and
     loses the epoch CAS. No other path may use this variant — and this
     "exclusively" is **compiler-enforced, not conventional** (Greptile P2 on
     PR #1177): the `ClaimStore` trait exposes the consent variant only as
     `steal_for_resume(entity, observed_epoch, witness: ResumeIdentityProof)`,
     where `ResumeIdentityProof` is a witness type constructable **only**
     inside the resume module (private field; minted after the SASL-identity
     ↔ snapshot bare-JID check of element 8). General stale-owner takeover is a
     separate `steal_stale(entity, observed_epoch, staleness: StalePredicate)`
     method that cannot express the no-staleness variant. A caller outside the
     resume path therefore cannot even name the consent CAS, so "any enrolled
     node displacing a fresh-lease owner without an identity check" is
     unrepresentable rather than merely forbidden by review.
   - *Heartbeat*: renewal is itself a CAS on lease freshness —
     `UPDATE nodes SET heartbeat = now() WHERE node_id=$me AND
     node_epoch=$mine AND NOT expired AND heartbeat >= now() - $lease_ttl`.
     `rows_affected == 0` is **fencing loss**, and now unambiguously means
     "your lease lapsed, was expired by a stealer/reaper, or your epoch
     was superseded": the node must immediately demote all local actors,
     drop all claims, stop writing, and may return to service only by
     re-registering with a fresh `node_id`/`node_epoch` and re-acquiring
     claims from scratch. Monotone expiry is a property of the
     **committed `expired` flag** (the *Expire* CAS), not of this
     predicate alone — so a GC/VM pause longer than the pause budget
     cannot resurrect liveness mid-reap (half-reaped state, ghost
     synthesized-unavailable presence, staleness flapping under the reaper
     are all excluded by the flag's one-way transition, not by node-local
     timers). All time
     predicates use **Postgres `now()`**, never node-local
     `chrono::Utc::now()`, so only one clock matters. Stored timestamps that
     feed those predicates are also DB-stamped: `detached_at` is derived
     from `now()` inside the fenced detach write, never bound from a
     node-supplied value (node clocks stamp timestamps only in single-node
     SQLite, where one clock exists by construction). Lease TTL ≥
     heartbeat-interval × N-missed + max GC/pause budget + **p99
     heartbeat-write-latency budget under load** — process pauses are not
     the only thing that delays a renewal; DB pool-wait and statement
     latency degrade first under exactly the load the cluster is scaled
     for. `lease_ttl` is **cluster-wide configuration**, node-supplied into
     both the *Expire* predicate and the renewal CAS: during a rolling
     config change that alters the TTL, a shorter-TTL node can expire a
     longer-TTL owner while that owner still believes its lease intact.
     This is **safe by construction, never split-brain** — the affected
     owner's next renewal fails its `AND NOT expired` predicate and demotes
     — so the worst outcome is a premature, conservative demotion; a TTL
     change is nonetheless treated as a coordinated cluster setting so the
     mixed-TTL window is bounded to one rollout.
   - *The liveness control plane is isolated from the data plane*: the
     heartbeat CAS and the claim acquire/steal/expire/release statements
     execute on a **dedicated, small connection pool** (or a reserved
     connection), never the shared statement pool. Otherwise a traffic
     spike exhausts the shared pool, the renewal queues behind fenced
     bulk transactions and claims-read storms, the lease lapses, the node
     demotes *everything*, and the resulting reconnect storm loads the
     survivors' pools in turn — a metastable, self-amplifying failure.
     Pool exhaustion must degrade stanza latency, never lease liveness; a
     heartbeat-write-latency histogram + alert (Phase 3) watches the
     cause, not just the heartbeat-age symptom.
   - *Demotion reconciliation — guaranteed discovery for every entity
     type*: epoch fencing fires only on durable writes, and a `UserActor`
     serving live local sockets performs none on its delivery hot path
     (unacked queues are in-memory until detach), so without a backstop a
     deposed UserActor owner could keep answering stream-level XEP-0198
     `<r/>`/`<a/>` on a healthy-looking socket indefinitely while inbound
     traffic routes to the new owner — the user sends successfully and
     receives nothing — and the best-effort Demote ask travels the same
     swarm path whose failure typically motivated the steal. Two
     mechanisms close this: (1) each heartbeat interval, alongside the
     renewal CAS on the control-plane pool, the node runs **one indexed
     reconciliation query** over `claims WHERE node_id = $me AND
     node_epoch = $mine`, diffs the result against its local owned-entity
     set, and demotes/tombstones anything it no longer owns —
     conflict-closing live local sockets for lost `UserActor`/SM claims —
     bounding dual-ownership for **every** entity type to one heartbeat
     interval at the cost of one O(local claims) indexed query per node
     per interval, consistent with the O(replicas) write-amplification
     goal; (2) an owner whose internal health ask fails during
     steal-intent processing does not wait to be stolen from: it kills
     the wedged actor and conflict-closes its sockets **before** the
     steal lands at `intent_ttl`, since it already knows the steal will
     proceed. The Demote ask (element 7) remains the fast path;
     reconciliation is the guarantee.
   - *Fencing*: every durable write performed on behalf of a claimed entity
     (`sm_sessions`, `sm_unacked`, `pending_delivery` promotion,
     `confirm_drained` deletes, MAM writes from a `RoomActor`, occupant
     roster rows, ISR tokens) executes in a transaction that **takes a row
     lock on the claims row as part of the epoch check**:

     ```sql
     BEGIN;
     SELECT 1 FROM claims
       WHERE entity=$e AND node_id=$me AND claim_epoch=$mine
       FOR SHARE;
     -- writes only if the SELECT returned a row
     COMMIT;
     ```

     or the single-statement CTE form
     `WITH c AS (SELECT ... FOR SHARE) INSERT/UPDATE ... FROM c`.
     `FOR SHARE` conflicts with the steal CAS's row lock; under READ
     COMMITTED a `FOR SHARE` that blocks on a concurrent steal re-fetches
     the latest committed row version and re-evaluates the predicate,
     returning 0 rows after the steal — which is what restores the
     demote-on-0-rows semantics. Two forms are **explicitly banned** because
     they do not fence: (a) a bare (lockless) join read of `claims` — it is
     evaluated on the statement/transaction snapshot and is not ordered
     against a concurrent steal (EvalPlanQual re-checks only the target
     relation, not joined rows), so a stale owner could resurrect deleted
     `sm_sessions` rows or double-promote across a steal that commits
     mid-transaction; (b) fencing columns denormalized onto the written
     tables — the steal CAS never touches those columns, so the check would
     pass forever. `rows_affected == 0` (or an empty fencing SELECT) means
     the claim was stolen: the local actor demotes immediately. The
     process-local `StreamLockMap` no longer provides any cross-node
     guarantee; row-locked epoch fencing replaces it as the multi-writer
     safety mechanism (the lock map remains only as an in-process contention
     optimization). Because fencing uses row locks, not session advisory
     locks, PgBouncer transaction mode remains compatible (element 12).
   - *Self-fencing and partition detection*: a node that observes fencing
     loss (heartbeat CAS returns 0 rows, or Postgres unreachable for N
     intervals) must stop serving its claimed entities — detach sessions,
     close sockets, stop MUC broadcasts — **before** its lease becomes
     stealable (guaranteed by the TTL formula). Entering the self-fenced
     state also **flips the node's client-facing HTTP readiness probe to
     not-ready** (a signal distinct from the swarm-liveness signal of Phase
     4, which deliberately stays off swarm membership for cold-start): a
     node that has dropped all claims but still passed readiness would stay
     in the client Service/Ingress endpoint set, and clients whose sockets
     it just closed would be routed straight back to the still-refusing node
     — an avoidable reconnect-bounce, and a longer black-hole under a
     Postgres-unreachable fence. Readiness is cleared (node re-added to the
     client Service) only on **successful re-registration under a fresh
     `node_id`/`node_epoch` plus claim re-acquisition**, so the Service
     never directs new client connections at a node that is currently
     refusing to serve. Heartbeat renewal is
     additionally coupled to self-health, with the swarm condition
     detecting **isolation, not any single unreachable peer**: a node
     refuses to renew only when the `nodes` table shows **two or more**
     other live nodes (fresh per Postgres, the authority) and this node
     can reach **none of them** over the swarm for M consecutive
     intervals, or when the actor runtime fails an internal ping.
     Partial pairwise unreachability — some live peers reachable, some
     not — is a **degraded-routing condition, never node suicide**:
     traffic to unreachable peers diverts to the durable
     `pending_delivery` fallback (delivery bounded by one janitor sweep,
     element 5) and an alert fires. The previous "at least one
     unreachable live peer" rule over-fenced: a single bad A↔B link in a
     healthy three-node mesh would fence both endpoints — 2/3 capacity
     destroyed while every entity remained reachable via Postgres and
     via C — and a misconfigured NetworkPolicy on the swarm port (which
     Phase 4 itself introduces) would fence the entire cluster. With
     exactly one other live node (N=2), swarm unreachability alone
     **never fences**: there is no witness to assign blame, and both
     sides fencing converts a one-link fault into a total outage; the
     pair degrades to durable-queue routing plus an alert instead.
     **Re-registration hysteresis**: a node that fenced due to swarm
     isolation re-registers (making its liveness visible) but
     re-acquires claims only after observing swarm reachability to at
     least one live peer whenever other live node rows exist, with
     exponential backoff on re-registration — without this, two mutually
     partitioned survivors oscillate forever (fence → leases expire →
     each sees "all peers expired" → lone-survivor rule → both serve →
     both see each other fresh-and-unreachable → fence again),
     conflict-closing every client once per cycle; with it, a persistent
     swarm-only fault converges to a stable degraded state. A total
     pairwise swarm partition with Postgres still reachable therefore
     fences every node at N≥3 and, with hysteresis, *stays* fenced until
     connectivity returns — a conservative fence for a **genuine** network
     partition, on the rationale that infrastructure faults large enough to
     sever every swarm link usually also sever or imminently sever Postgres,
     so continuing to serve would black-hole clients anyway. Two things keep
     this from being a foot-gun. First, the one total-fence cause that
     demonstrably does **not** share fate with Postgres is the ADR's own
     Phase 4 swarm-port NetworkPolicy (a different destination and port than
     Postgres): a mis-scoped policy could sever every swarm link while
     Postgres stays reachable and fence the whole cluster. That artifact is
     therefore **CI-validated / pre-flight-checked** — the chart test asserts
     the NetworkPolicy admits intra-selector traffic on the swarm
     `containerPort` (a positive intra-selector-reachability assertion, not
     just a lint), so a self-inflicted total fence cannot ship silently
     (Phase 4 deliverable). Second, the durable-queue degraded mode used for
     *partial* partitions is a working alternative to a total outage when
     Postgres — the ownership authority **and** the durable-queue substrate —
     is reachable by hypothesis; the conservative fence is chosen over it
     deliberately (a node that can reach neither its peers nor, shortly,
     Postgres is better removed than left half-serving), and the tradeoff is
     recorded. If operational experience shows benign total-swarm /
     Postgres-reachable partitions to be common, routing them through the
     same durable-queue path as partial partitions is the pre-identified
     escalation, gated on that CI-validated NetworkPolicy. A node whose peers
     are all heartbeat-expired
     in Postgres — the lone survivor, or the first pod of a rolling
     restart — **keeps renewing and serving**: Postgres, not the swarm,
     is the ownership authority, and a survivor that can reach Postgres
     is safe. The intended replica count is never configured into the
     node — it is read as live `nodes` rows.
   - *Unwedge (steal-intent with owner veto — replaces "forced steal")*:
     no node may evict an owner whose lease is fresh on its own unverifiable
     say-so ("I saw N failures" cannot be attested in a CAS, so it would be
     a room/user-takeover primitive for any enrolled node). Instead, after N
     consecutive failed/NACKed remote deliveries to a fresh-lease owner, the
     frustrated node writes a `steal_intents (entity, reporter_node,
     created_at DEFAULT now())` row. The table carries **`UNIQUE (entity,
     reporter_node)` with `ON CONFLICT (entity, reporter_node) DO UPDATE SET
     created_at = EXCLUDED.created_at`** (refresh, not accumulate) so N
     consecutive failures from one reporter against one entity collapse to a
     single row rather than growing unbounded during a sustained A↔B relay
     fault, and an **index on `(entity, created_at)`** so the steal CAS's
     `EXISTS` predicate is a bounded index probe, never an O(table) seq-scan
     on the routing-critical path (Greptile P2 on PR #1177). Cleared rows are
     deleted on the owner-veto path above; abandoned rows (reporter died) are
     swept by the orphan-reaper on its existing cadence, keyed by
     `created_at < now() - k·intent_ttl`. Every owner's heartbeat loop reads
     intents against its own claims and, for each, performs an internal
     health ask **of that entity's owning actor**; on success it clears the
     intent with an epoch-fenced DELETE — a healthy owner has an unforgeable
     veto, proven by writing under its own live epoch. If the intent
     survives `intent_ttl` (a small multiple of the heartbeat interval)
     uncleared — the actor is wedged, or the owner cannot write — the steal
     CAS may proceed with the in-statement predicate `EXISTS (SELECT 1 FROM
     steal_intents WHERE entity=$e AND created_at < now() - $intent_ttl)`
     substituting for the owner-stale predicate. This applies to **both
     `RoomActor` and `UserActor` claims** (a deposed UserActor owner
     conflict-closes any live local socket for that user upon demotion
     discovery — guaranteed within one heartbeat interval by the
     reconciliation query above, and pre-empted entirely when the owner's
     own failed health ask triggers the proactive wedge-kill — reusing
     element 8's demote machinery), bounding a
     sick-but-heartbeating node's hostage window to `intent_ttl` for every
     routed entity type. SM-session claims are still stolen **only** via the
     identity-bound resume path (element 8). The fresh-heartbeat carve-out
     of the previous revision is removed entirely.

5. **At-least-once, ordered inter-node delivery.** (Replaces the previous
   "at-most-once is acceptable" decision, which was protocol-false: XEP-0198
   acks are hop-scoped to a single client-server stream, not end-to-end
   across the routing fabric, and Q6 promotes only the *recipient's* recorded
   unacked queue — a stanza lost between nodes never enters any queue, and
   the sending node has already acked the sender's client, so silent loss
   would violate RFC 6120's deliver-or-error obligation.)
   - Cross-node stanza handoff is a **`RemoteAskRequest` with explicit
     `mailbox_timeout` and `reply_timeout`** — never `RemoteTellRequest`,
     whose delivery result acks only mailbox enqueue. The handoff ack must
     be issued by the receiving handler **after** it has recorded the stanza
     into the recipient stream's SM unacked queue or durable
     `pending_delivery`/offline storage; enqueue-acks cannot carry that
     meaning. Until acked, the sender retains the stanza; on timeout/failure
     it retries, then falls back to writing durable `pending_delivery`
     itself.
   - **The timeout hierarchy is explicit and transport-capped.** kameo's
     `messaging::Config` carries a fourth, easily missed field:
     `request_timeout` (default **10s**, mapped to libp2p
     `request_response::Config::with_request_timeout`), which bounds the
     entire ask exchange sender-side — dial, send, receiver handler
     execution, reply — while `mailbox_timeout`/`reply_timeout` are
     serialized into the request and enforced only on the receiving
     node. Any reply_timeout above request_timeout is dead configuration:
     the sender observes `OutboundFailure(Timeout)` at the transport cap
     regardless. Because this design deliberately places durable
     Postgres writes inside ask round-trips (the handoff ack fires after
     the receiver's durable/SM record; the live-resume handshake asks
     the old owner to detach-flush a full `sm_unacked` snapshot), Phase
     2 sets `with_request_timeout` explicitly, sized above the
     worst-case fenced-write/handshake budget — the live-resume
     detach-flush of a large unacked queue on a loaded DB is the sizing
     case: at the 10s default, every backoff retry of a >10s flush times
     out identically and forfeits a live session the held-response
     window (element 8) exists to save. Stated invariants:
     reply_timeout ≤ request_timeout, and mailbox_timeout + receiver
     handler budget ≤ request_timeout. `max_concurrent_streams` (default
     100) caps **all** concurrent asks per peer connection — relay
     traffic, state-mutation asks, flush pokes, presence anti-entropy,
     Demote, and steal handshakes share it — and is set/documented
     against the modeled concurrent-ask load per peer pair.
   - **Inbound `h` accounting is coupled to handoff; `<r/>` answers are
     not.** A cross-node-destined inbound stanza counts as "handled" for
     the origin client's stream — included in inbound `h` — only after
     the handoff ack arrives or the durable fallback write commits.
     XEP-0198 defines "handled" as the server having taken
     responsibility; we take responsibility exactly at owner-acked or
     durable handoff, never before, and `h` advances in stream order, so
     an unresolved handoff for stanza k holds `h` at k−1. But every
     client `<r/>` is **answered immediately with the current `h`** —
     never withheld: XEP-0198 §4 is explicit that the `<a/>` response
     "MUST NOT be withheld for any condition other than a timeout", and
     a pending remote handoff is not a timeout. Answering `h = k−1`
     while stanza k is unresolved is fully conformant — the client keeps
     k in its unacked queue and, if the stream later drops, retransmits
     it on resume; the dedup layer absorbs that retransmission by design
     (see the dedup key below). Withholding would additionally starve
     clients of keepalive acks during handoff retries, turning every
     slow cross-node handoff into a client-side ack timeout and a
     needless reconnect. The sender-node-crash loss window stays closed
     exactly as before: any stanza covered by an acked `h` is, by
     construction, already in the recipient's SM queue or in durable
     storage. (Latency consequence recorded in Consequences.)
   - Whenever a node inserts `pending_delivery` rows for a user owned
     elsewhere, it sends a **flush poke** to the owning `UserActor` — as an
     **acked ask with bounded retries** feeding a `stalled_pending_delivery`
     gauge/alert on exhaustion, because the poke travels the same possibly
     broken path that forced the durable fallback. The poke is an
     optimization, not the guarantee: the **guaranteed flush path is the
     owning node's claim-scoped janitor**, which periodically sweeps
     `pending_delivery` for bare JIDs whose `UserActor` claim it holds and
     flushes rows through the UserActor's **full delivery surface — local
     connection actors and the ordered relay to remote connection actors
     alike**, never "locally connected resources" only. Ownership is
     deliberately decoupled from socket location (a UserActor claim on
     node A with the socket on node B is routine multi-device topology),
     so a local-only flush would strand rows indefinitely for an *online*
     user whose socket lives elsewhere even while the A↔B relay is
     healthy — and the flush poke cannot cover that case, because the
     poke targets the same owner whose local flush is the dead end.
     Flushed rows route through the same per-`(origin stream → recipient)`
     sequencing and sticky-failover machinery as live traffic, preserving
     order across both ingress paths. A `pending_delivery` row flushed by
     the owning node's janitor — a stanza that could not be delivered live
     and was committed to durable storage for a sweep interval — is stamped
     with an **XEP-0203 `<delay/>`** carrying the original ingress
     timestamp before delivery (`pending_delivery` therefore persists that
     timestamp per row), matching the existing Q6-promotion behavior. The
     conformance basis for marking server-side deferred delivery is XEP-0203
     itself (XEP-0198 §6 is Error Handling and carries no such SHOULD; the
     `<delay/>` SHOULD in XEP-0198 §5 is directed at clients resending
     unacked stanzas, not at a server flushing deferred storage); the
     presence-triggered reconnect fast path stamps identically. Both sides of a partial swarm
     partition still reach Postgres, so delivery delay for an online user
     is bounded by one sweep interval plus one relay hop regardless of
     which node terminates the socket (the existing presence-triggered
     flush in `pending_delivery/flush.rs` remains the fast path on
     reconnect). Persistent poke failure also files
     a `steal_intents` row for the UserActor claim (element 4), bounding the
     wedged-owner case.
   - Retries are idempotent via a dedup key **exclusively** the
     server-internal `(recipient, origin stream id, inbound sequence)`
     carried in the envelope, preserving XEP-0198's no-duplication
     guarantee under at-least-once — with three properties that are
     load-bearing, not implementation detail:
     0. **The key is scoped by recipient, not table-global.** One inbound
        stanza with a single `(origin stream id, inbound sequence)`
        legitimately fans out to *many* recipients — the canonical case is
        a XEP-0045 groupchat message reflected to N occupants whose
        remote/offline members each take the durable `pending_delivery`
        fallback. A table-global UNIQUE on `(origin_stream_id,
        inbound_seq)` would insert the first recipient's row and silently
        `ON CONFLICT DO NOTHING`-discard every other recipient's — an RFC
        6120 deliver-or-error violation on exactly the durable path this
        element exists to guarantee. The dedup dimension that must collapse
        two *retries* is `(recipient, origin stream, inbound seq)`, and
        both edge cases that motivate durable dedup operate **within a
        fixed recipient**: the resume-retransmit collision (property 1) is
        the same source stanza re-relayed to the *same* recipient, and the
        recipient-claim-move retry (property 2b) is by definition scoped to
        one recipient whose claim moved. So recipient-scoping preserves
        every de-duplication guarantee while permitting one source stanza
        to reach every intended recipient. `recipient` is the durable row's
        per-recipient key (`pending_delivery.recipient`, a bare JID) or the
        owning SM stream (`sm_unacked`'s recipient session), so the key is
        realized as `(recipient/stream_id, origin_stream_id, inbound_seq)`
        on each table.
     1. **The origin stream id is the origin session's SM-ID** (a
        server-generated stream UUID for non-SM streams) — an identifier
        stable across stream resumption and cross-node resume-steal,
        **never a per-TCP-connection id**. Stability is what makes the
        resume-retransmit race collide in dedup: after a live
        resume-steal, the old owner's already-in-flight relay envelope
        for stanza k (a socket close cannot retract it) and the resumed
        client's §5-mandated retransmission of k, relayed by the new
        owner, must carry the **same** key at the recipient — with a
        per-connection id they would be two distinct keys and the
        recipient would deliver twice. **The inbound sequence is the origin
        stream's XEP-0198 SM inbound-handled (`h`) counter value** for that
        stanza — the very counter that resumes from the loaded snapshot, so
        a client retransmission after resume re-lands on the identical
        number — **never a node-local or relay-channel monotonic counter**,
        which would mint a fresh value on the new owner and split one
        logical stanza into two distinct keys, defeating §5. (For
        non-SM origin streams the server-assigned per-stream ingress
        sequence plays the same resumption-stable role.)
     2. **Dedup state survives the receiver.** In-memory receiver-side
        dedup cannot close the at-least-once edge cases — receiver
        restart after a committed-but-unacked write, and recipient-claim
        movement between retries, are precisely when memory is gone. So:
        (a) the durable tables enforce the key — `pending_delivery` and
        `sm_unacked` carry a **UNIQUE constraint on
        `(recipient/stream_id, origin_stream_id, inbound_seq)`** (the
        recipient bare JID on `pending_delivery`, the owning recipient
        session on `sm_unacked`) and fallback/promotion
        inserts are `INSERT ... ON CONFLICT DO NOTHING` inside the
        fenced transaction, making a retry that races a committed write
        (ask reply lost, or reply_timeout fired during a slow commit)
        idempotent regardless of which node handles it, including a new
        owner after a mid-flight claim steal — while a fan-out of one
        source stanza to N distinct recipients inserts N rows, because the
        recipient dimension is part of the key; (b) the receiver-side
        **per-(recipient, origin-stream)** dedup high-water mark /
        recent-key ledger is
        **part of the recipient session's SM state** — included in the
        detach snapshot and the live-handoff flush, and consulted by the
        durable fallback path — so a sender retry that arrives after the
        recipient's claim moved to a node whose snapshot already
        contains the stanza is still suppressed. Dedup-key retention is
        bounded to the origin stream's lifetime: rows and ledger entries
        are dropped when the origin SM session completes or expires.
     XEP-0359 `origin-id` is client-controlled input: it is
     propagated unchanged so receiving clients can deduplicate per XEP-0359,
     and is **never** used server-side as a suppression key (a client reusing
     an origin-id across distinct messages must not get stanzas dropped).
   - **Ordering** (RFC 6120 §10.1): all cross-node stanza traffic between a
     node pair flows through a per-peer **relay actor pair** owning a single
     sequenced channel, with per-`(origin stream → target entity)` sequence
     numbers and receiver-side gap detection. Fan-out to a remote node is
     aggregated **once per peer node with the recipient list** (the receiving
     relay fans out locally), not one tell per occupant. Envelope size is
     bounded explicitly: Phase 2 sets `messaging::Config`
     request/response size maxima (kameo's defaults are 1MB/10MB), recipient
     lists are **chunked to a bounded envelope size**, and
     `OversizedEnvelope` is a typed sender-side error that fails fast to the
     durable per-recipient path — an oversized envelope is never retried
     as-is (it fails forever at the transport). Sticky failover: once any
     stanza for a given `(origin stream → recipient)` pair diverts to the
     durable queue, all subsequent stanzas for that pair divert too, and
     the pair resumes direct sends only via an **explicit un-divert
     protocol** — "until the queue has flushed" cannot be left as an
     invariant without a mechanism, because the flush is executed by the
     owning node's janitor on its own schedule and relay-channel recovery
     is *not* the same event as queue drain; naive resumption on relay
     recovery reorders on exactly the path this machinery protects
     (divert at seq n; n..n+4 sit queued; the relay heals; the sender
     resumes direct at n+5; the receiver delivers n+5 before the janitor
     flushes n..n+4 — an RFC 6120 §10.1 violation on the recovery path).
     Concretely: (a) `pending_delivery` rows carry the per-pair sequence
     number; (b) a diverted sender resumes direct sends only after
     observing zero remaining queued rows for that pair — via a point
     read of the queue or an ack from the owning node's janitor; (c) the
     receiving node's per-pair gap detection spans **both ingress paths**
     (relay and janitor flush), so a premature direct send is held, never
     delivered out of order. On a recipient-claim move mid-stream the new
     owner reconstructs per-pair *expected-next* from the durable
     `pending_delivery` per-pair sequence numbers — the ordering high-water
     mark rides the same SM snapshot as the dedup ledger (property 2a
     below) — so ordering gap detection is continuous across claim moves
     exactly as dedup suppression is.
   - **IQ**: a cross-node IQ whose handoff ultimately fails synthesizes a
     typed `<service-unavailable/>` error back to the requester — offline
     storage is message-only and must never be the IQ fallback.
   - **UserActor state mutations** (presence updates, carbons toggles,
     resource priority changes) use acked asks, never fire-and-forget: a lost
     `priority = -1` update would make `UserActor` deliver bare-JID messages
     to a resource RFC 6121 §8.5.2.1.1 forbids.

6. **Routing: Postgres-authoritative with an in-process cache; kademlia is
   node discovery only.** Two verified kameo-0.20 realities rule out the
   per-entity DHT registry: the hardcoded `MemoryStore` limits (1024
   provided keys/records vs ~55k modeled entities — registration fails with
   `MaxProvidedKeys` at ~2% of scale), and per-publisher records (record
   validation rejects a key whose peer-id does not match the source, so a
   new claim owner **cannot** overwrite a deposed owner's registration;
   singular `lookup` returns an arbitrary provider). Forking kameo to expose
   `MemoryStoreConfig` was considered and rejected as an owned-fork
   liability. Instead:
   - Each node registers **O(1) names** in kademlia: its per-node relay
     actor (element 5), keyed by its unique per-instance `node_id`. Kademlia
     provides node discovery and connectivity only — never entity→node
     resolution.
   - Entity→node resolution reads the **Postgres claims table through a
     bounded in-process cache** (TTL plus NotOwner invalidation); cache
     entries carry `(node_id, claim_epoch)`.
   - Every cross-node envelope carries the sender's believed
     `(entity, claim_epoch)`. The receiver checks it against its local
     claim; on mismatch it replies with a typed
     `NotOwner { entity, current_epoch }` NACK (this requires ask semantics
     — element 5). On `NotOwner` (or first contact), the sender re-reads the
     claims table, refreshes its cache, and re-sends.
   - A demoted node keeps a **tombstone** for entities it lost and answers
     relayed traffic with `NotOwner` rather than processing it; demotion
     (graceful or discovered) also calls `remote::unregister()` for any
     name the node will no longer serve.
   - **The relay actor is supervised; re-registration is mandatory; stale
     `RemoteActorRef`s have an explicit refresh trigger.** kameo
     auto-unregisters any actor that stops or panics — removal from the
     remote registry plus `swarm.unregister` of its provider/metadata
     records (kameo 0.20 `actor/spawn.rs`) — so an unsupervised relay
     panic silently removes a healthy node from the routing fabric while
     its Postgres heartbeat stays fresh: peers' cached `RemoteActorRef`s
     fail with `ActorNotRunning` (respawn mints a new `ActorId`), the
     healthy owner correctly vetoes the resulting steal intents, and all
     traffic toward the node diverts to the durable queue at janitor
     latency — a steady-state, cluster-wide degradation with no
     self-healing path. Therefore the relay actor runs under supervision
     (kameo link or owning task) with respawn plus **mandatory
     re-registration under the same `node_id` name** (same-peer metadata
     overwrite is permitted by kameo); and sender-side
     `ActorNotRunning`/`UnknownActor`/`BadActorType` errors are an
     explicit trigger for a kademlia re-lookup of the peer's relay name
     with bounded backoff — a node_id→`RemoteActorRef` refresh path
     **distinct from** the `NotOwner` claims-refresh path, which only
     refreshes entity→node_id and never sees these transport-layer
     errors.
   - **Negative caching**: entities with no claims row (offline users,
     absent rooms) get short-TTL negative cache entries, invalidated
     locally when this node acquires a claim for the entity and treated
     as a miss on any inbound envelope naming the entity. Without this,
     every stanza to every offline recipient — roster fan-out where most
     contacts are offline, MUC invites, retry storms — is an uncacheable
     claims-table point read on the hot path; the negative-lookup rate is
     an explicit term in element 12's load model.
   - Staleness bound: a hard-dead node's provider and metadata records
     remain visible until the libp2p provider/record TTLs expire; the
     **measured visibility window of a dead publisher's records** — not the
     republish interval — is the actual staleness bound and a Phase 2 spike
     exit criterion. Because entity resolution never touches the DHT, a
     stale node record at worst delays dialing a dead peer; it cannot
     misroute an entity. Under NotOwner storms every route degrades to a
     claims-table point read; pool sizing (element 12) budgets for the
     cache-miss and NACK rates against this table.

7. **MUC stays single-writer, with a specified re-election protocol.**
   `RoomActor` ownership is a claims-table entry; non-owner nodes proxy joins
   and messages to the owning actor via the dedicated remote-safe MUC message
   set. Because occupant state died with the owner under the old design and
   nobody could send the `<presence type='unavailable'/>` that re-join
   depends on, occupant tracking becomes durable:
   - The owning `RoomActor` persists in Postgres alongside the room claim,
     epoch-fenced like all claimed-entity writes: the **occupant roster
     (real JID, nick, occupant-id)** on join/leave, and the **room's
     long-lived state** — the full room configuration (including password
     and members-only), the **affiliation lists
     (owner/admin/member/outcast)**, and the current subject — on every
     change. Occupant-roster durability alone is nowhere near sufficient
     for a correct takeover: room state is in-memory only today
     (`muc/room.rs` `is_dormant` documents exactly this, and no
     room/affiliation table exists in any schema), so a takeover that
     rematerialized a default `MucRoom` would silently drop ban lists,
     member lists, passwords, config, and subject on **every** ownership
     move — including once per room per rolling deploy — letting outcasts
     rejoin right after their 332 kick (violating XEP-0045 §7.2.9 and
     §5.2's long-lived affiliations: ban evasion on every deploy),
     opening password-protected and members-only rooms, and resetting
     owner lists. Config/affiliation/subject changes are low-rate, and
     join/leave already pays a durable write, so the added write
     amplification is marginal. The new owner **restores configuration,
     affiliations, and subject from Postgres before accepting any join**
     and serves §7.2.15 subject-after-join from the restored subject; the
     Phase 4 GA gates assert that an outcast is still denied entry and
     that password/members-only protection holds after an ownership
     steal.
   - **Deposed-owner demotion is a two-part protocol**, because epoch
     fencing alone only fires on durable writes and a quiet room might never
     perform one: (1) after any steal CAS succeeds, the new owner sends a
     best-effort acked `Demote { entity, new_epoch }` to the old owner via
     its node relay; the recipient tombstones the entity and NotOwner-NACKs
     subsequent traffic. (2) As the guaranteed backstop, the owning
     `RoomActor` passes **every broadcast through one fenced statement
     before local fan-out** — the MAM archive insert where archiving is on
     (already fenced, so free), otherwise the exact fencing primitive
     element 4 already prescribes: an autocommit `SELECT 1 FROM claims
     WHERE entity=$e AND node_id=$me AND claim_epoch=$mine FOR SHARE`
     (0 rows ⇒ demote before fan-out). A per-broadcast epoch-check
     `UPDATE` on the claims row was considered and rejected: at 20–50
     msg/s an archive-off room would churn millions of dead tuples per
     day on one row of the routing-critical claims table (HOT-chain
     growth and autovacuum pressure against the table every cache-miss
     route point-reads) and would take an exclusive lock where a share
     lock gives the identical ordering guarantee for free. Because the
     `FOR SHARE` lock conflicts with the steal CAS's row lock
     (element 4), this statement is ordered against the steal:
     a deposed owner's very next broadcast attempt returns 0 rows and
     demotes it **before** any local fan-out. The dual-owner exposure is
     therefore bounded: a deposed owner delivers no post-steal broadcasts,
     and each stale proxy node sends at most one envelope that is
     NotOwner-NACKed and re-routed. The cost — one Postgres round-trip per
     broadcast in archive-off rooms — is recorded in Consequences.
   - On ownership-epoch change (steal after owner death, steal-intent
     expiry, or graceful release), the new owner bumps the **room epoch**
     and notifies proxy nodes; each occupant's **local node** synthesizes
     XEP-0045 unavailable presence from the occupant's room JID with status
     **332** (system shutdown/takeover) — and **110** on the self-presence —
     to every local occupant, fulfilling the XEP-0045 service-shutdown
     obligation that transfers to the surviving cluster and triggering
     standard client re-join.
   - Messages arriving during the ownership gap are **bounced with a typed
     recoverable `<resource-constraint/>` error**, never silently dropped.
   - Re-join and steal attempts use jittered backoff for herd control on
     large rooms.

8. **Cross-node SM resume over a real ownership substrate.** Every SM
   session gets a **claims row at `<enable/>` time** (entity = SM-ID,
   `entity_type = sm-session`, owned by the socket-terminating node) — one
   durable write per session establishment, off the per-stanza hot path,
   consistent with this ADR's rejection of write-ahead stream persistence.
   The claim persists across detach (the fenced detach snapshot writes
   against it) and is deleted by the epoch-fenced `complete_claim` on clean
   close or expiry. This gives `<resume previd=P/>` on any node an
   authoritative owner lookup and gives every branch below a real lease to
   consult; there is no unclaimed live-session state.
   - **Detached session owned elsewhere**: the receiving node reads the
     durable snapshot, compares its bound bare JID against the stealing
     stream's locally SASL-authenticated bare JID **before** any write, and
     only on match steals the claim via the **consent/epoch-only CAS**
     (element 4 — the previous owner's lease may be perfectly fresh, so the
     owner-stale predicate can never authorize this path) and resumes. On
     mismatch it returns `<failed/>` `not-authorized` and leaves the claim
     epoch untouched, so a wrong-identity `<resume/>` cannot fence the
     legitimate owner's snapshot out. The identity check preceding the CAS
     makes "does not steal the claim on wrong identity" hold here exactly as
     it does on the live-session path below. The final durable delete in
     `complete_claim` is epoch-fenced, so of two simultaneous identity-valid
     `<resume/>` attempts on different nodes exactly one wins; the loser
     returns `<failed/>`.
   - **Live session owned elsewhere** (the common mobile roaming case — old
     socket still open, no durable snapshot yet): steal is a **handshake,
     not a bare table write**. The stealing node resolves the owner from the
     claims row and asks it (remote ask with timeout) to detach-flush a
     snapshot and close the old stream with a `<conflict/>` stream error per
     XEP-0198 §5; only on ack does the fenced epoch-bump CAS (the
     consent/epoch-only variant, element 4) on the claims row commit and
     the snapshot get loaded. This also prevents the old node
     from continuing to count/send on a half-open socket and diverging `h`.
     **The destructive `<conflict/>` close is gated on the identity match,
     not merely the subsequent CAS.** The steal-handshake request carries
     the stealing node's locally SASL-authenticated bare JID; the owner
     compares it against its live session's bare JID **before** flushing or
     closing, and on mismatch returns `not-authorized` **without** closing
     the victim's stream. Otherwise an authenticated peer presenting another
     user's `previd` would force-disconnect that user's live session before
     the identity check (which the current ordering runs only after the
     snapshot loads) could reject the steal. This is defense in depth —
     `previd`/SM-ID is an unguessable UUIDv4 that only enrolled (already
     fully trusted) peers can read from the claims table, so there is no
     realistic external attacker capability — but it makes the victim's
     stream survive a wrong-identity steal attempt rather than relying on
     the CAS to fail after the close already happened.
   - **Owner unreachable but lease fresh**: XEP-0198 has no retry-after
     semantic (`<failed/>` children MUST be RFC 6120 conditions, and real
     clients treat `<failed/>` as terminal for that previd — they clear SM
     state and bind fresh). So the node **holds the `<resume/>` response** —
     conformant, since the XEP mandates no response deadline — and retries
     the owner handshake with backoff for a bounded window capped at
     min(remaining lease TTL, resume-handshake timeout). A routine transient
     inter-node blip thus resolves into a successful handoff instead of
     forfeiting the session. If the owner's lease expires within the window,
     the steal proceeds as the dead-owner case. Only when the window expires
     with the owner still leased-but-unreachable does the node answer
     `<failed/>` with `<resource-constraint/>` (the session may still exist
     server-side); once the session is known gone it is `<item-not-found/>`.
     Either `<failed/>` is **terminal for the client** — fresh session plus
     MAM catch-up — extending the accepted-forfeit tradeoff (Consequences)
     to handshake-timeout cases.
   - Once the lease expires, the steal proceeds; a live session on a
     hard-dead node has no durable snapshot, so the resume returns
     `<failed/>` with `<item-not-found/>` and unacked in-flight stanzas are
     lost. **This is an accepted tradeoff** (recorded in Consequences): we
     choose owner-mediated handoff over write-ahead persistence of every
     live stream (per-stanza durable writes on the hot path), so hard node
     death forfeits live sessions' resume; clients re-establish and recover
     via MAM catch-up.
   - **Identity binding is non-negotiable**: a claim steal may be initiated
     only by a node that has locally SASL-authenticated the requesting
     connection (`allows_stream_management_resume` gating stays), and only
     after the loaded snapshot's bare JID is confirmed equal to that
     authenticated identity. `previd` is never sufficient authorization; a
     forged previd with the wrong identity returns `not-authorized` and does
     **not** steal the claim. No load-balancer session affinity is required.

9. **Claim-scoped durable-SM consumers.** All three consumers of the shared
   tables become claim-scoped in cluster mode:
   - `restore_from_persistence` hydrates only sessions whose claim this node
     holds or can acquire at startup (acquire-then-hydrate); it never
     performs unscoped full-table hydration, and restore-time expired-row
     deletion moves behind the claim (and uses Postgres `now()` against the
     DB-stamped `detached_at`, element 4).
   - The SM-expiry janitor sweeps and Q6-promotes only self-claimed sessions;
     promotion executes under the row-locked fenced epoch, so two nodes can
     never double-promote the same unacked queue into `pending_delivery`.
     The same janitor performs the `pending_delivery` sweep-flush for owned
     bare JIDs (element 5).
   - Graceful shutdown drain **releases claims per entity, after that
     entity's final fenced writes** (Phase 3 drain sequence) rather than
     assuming exclusive table ownership.
   - An **orphan reaper** handles rows claimed by nodes whose liveness
     expired: any node may steal such claims (fenced CAS) and then expire or
     promote them, after first committing the expire CAS on the owner's
     `nodes` row (element 4). The committed `expired` flag guarantees a
     reaped node cannot flip back to fresh mid-sweep: a concurrent
     renewal either commits first (blocking the expire CAS via the shared
     row lock) or fails its `NOT expired` predicate.
   - A stanza that arrives at a node whose claim was stolen mid-flight is
     re-routed: the node re-reads the claims table and relays to the new
     owner (element 6's `NotOwner` path applies symmetrically).

10. **XEP-0397 ISR becomes cluster-correct or is not advertised.**
    `IsrTokenStore` moves to Postgres, keyed to the SM claim: token consume
    fetches the token row **by the non-secret key** (the SM-ID/claim),
    compares the stored token against the presented token **in Rust with a
    constant-time primitive** (`subtle`/`constant_time_eq`), and only then
    performs the delete — all inside one epoch-fenced, `FOR SHARE`-locked
    transaction preserving single-use atomicity, bound to the same
    authenticated-identity check as resume (element 8). Matching the token
    in a SQL `WHERE` clause (`DELETE ... WHERE token=$2 RETURNING`) is
    explicitly banned: Postgres byte-wise equality short-circuits on the
    first mismatching byte and is a timing oracle, so a "constant-time"
    claim over it would be false. A wrong-node ISR
    attempt performs the same claim lookup as `<resume/>`. Failure handling
    distinguishes the XEP's two cases exactly:
    - **Authentication succeeded but instant resumption impossible** (e.g.
      claim handshake not completed in time, session expired): reply with a
      XEP-0388 `<success/>` Nonza containing `<inst-resume-failed/>`
      (namespace `https://xmpp.org/extensions/isr/0`) which wraps the
      XEP-0198 `<failed/>` element. The client is authenticated and
      continues with normal session establishment/resource binding; this is
      the recoverable degradation path.
    - **SM-ID valid but ISR token authentication failed**: reply with a
      XEP-0388 `<failure/>` Nonza, **and delete the resumable session state
      the SM-ID identified** — the detached `sm_sessions`/`sm_unacked` rows
      and the SM claim, via an epoch-fenced delete — per the XEP's MUST
      ("the server MUST delete any state of the stream which was attempted
      to resume in case the SM-ID was correct but the authentication
      failed", which exists to prevent brute-force token guessing against a
      kept-alive session). There is **no** degradation to ordinary XEP-0198
      resume after failed ISR auth; the session is gone by design.

    Until this ships (Phase 3), ISR is **removed from stream features/disco**
    in Phase 0 — it is currently unwired anyway, and advertising a
    non-conformant feature violates project rules.

11. **Presence fan-out with reconciliation.** Presence changes are broadcast
    to interested remote `UserActor`s over the ordered relay channel (element
    5), aggregated per peer node; each node keeps a local view for its own
    connections. Because a lost `<unavailable/>` would otherwise leave ghost
    presence forever:
    - Per-resource presence carries a version/sequence number; nodes run
      periodic **anti-entropy sweeps** against the owning `UserActor` for
      remote users their local users subscribe to.
    - The claims orphan-reaper emits **synthesized unavailable presence** for
      all resources of entities whose owner's liveness was reaped — the same
      fenced-expiry event that triggers SM drain. The committed `expired`
      flag (element 4's expire CAS) guarantees the reaped owner cannot
      return mid-sweep and contradict the synthesized presence.
    - Presence **probes are answered only by the authoritative owner**, never
      from a cached remote view.
    - The remote presence message set is typed end-to-end:
      `UpdatePresenceState`'s `show: Option<String>` is converted to
      `xmpp_parsers::presence::Show` **before** any remote message set is
      defined, per the typed-payloads rule.

12. **Database capacity is planned, not discovered.** Pool size becomes a
    `DatabaseConfig` field surfaced through Helm values **in Phase 3, not
    Phase 4**: pool configurability is a prerequisite of the Phase 3
    drain sequence, which funnels O(owned-entities) fenced transactions
    through the pool (both adapters currently hardcode
    `.max_connections(10)`). The **liveness control plane runs on its own
    dedicated pool** (element 4): the heartbeat CAS and claim CAS
    statements never queue behind fenced bulk writes, backstop fencing
    SELECTs, claims-read storms, or janitor batches — pool exhaustion
    must degrade stanza latency, never lease liveness. The ADR's load
    model per replica: claims point-reads at (stanza rate ×
    in-process-cache-miss ratio + NotOwner NACK rate + negative-lookup
    rate for unclaimed entities, element 6) — the cache is process-local
    with NotOwner invalidation, so the miss ratio, not a DHT hit rate, is
    the modeled variable; one heartbeat CAS plus one
    demotion-reconciliation query per interval (per node, element 4);
    claim CAS on enable/resume/join/steal; per-broadcast `FOR SHARE`
    fencing SELECTs for archive-off rooms (element 7); and claim-scoped
    janitor batches — plus the existing per-subsystem pools. **The
    model's inputs are exported as metrics** (Phase 3: routing-cache
    hit/miss counters, NotOwner NACK counters sent/received by entity
    type, claims-table point-read rate) so the sizing is validated in
    production rather than inferred from connection-acquire timeouts on
    the routing hot path, with Phase 4 alerts on NotOwner rate and on
    cache-miss ratio exceeding the sizing assumption. Deployment docs
    must budget total connections (replicas × pools × size, including the
    dedicated control-plane pool) against Postgres `max_connections`;
    PgBouncer in transaction mode is compatible with this
    design **because** the contract is single-statement CAS plus
    transaction-scoped `FOR SHARE` row locks, never session advisory locks —
    one more reason advisory locks are prohibited here.

## Implementation Plan

Phased so each step ships value independently. `replicaCount: 1` remains the
default until the final phase, **enforced by the chart at render time and
by the server-side singleton guard at runtime (Phase 0)** — never by
convention alone.

- **Phase 0 — restart hardening + guardrails (independent of clustering):**
  - Chart validation (unconditional, now): fail rendering when
    `replicaCount > 1` — **unconditionally**, with no escape hatch — and
    additionally fail whenever `.Values.clustering` is set **at all**
    (`{{- if .Values.clustering }}{{ fail "clustering is not supported by
    this chart/server version" }}{{- end }}`). Helm does not reject
    undeclared values, so "the value only ships later" is not enforcement:
    `--set clustering.enabled=true` would silently satisfy a
    `unless clustering.enabled` check on a pre-Phase-4 chart. Ship a
    `values.schema.json` with `additionalProperties: false` for defense in
    depth. In Phase 4 this hard-fail is replaced by the real
    `clustering.enabled` + postgres-driver validation. Keep the RWO check as
    a separate concern. Today an S3-configured deployment can scale to N and
    split-brain with a one-line values change; this closes that, and closes
    it against unknown-key injection too.
  - **Server-side singleton guard.** Chart validation is render-time only:
    `kubectl scale deployment waddle-server --replicas=3` during an
    incident, or an externally attached HPA/KEDA policy silently taking
    over `.spec.replicas`, bypasses every Helm check — and an
    S3-configured deployment with persistence disabled scales today
    without even a Multi-Attach wedge, after which three pods sharing the
    Postgres DSN each run the unscoped `restore_from_persistence`
    hydration and the SM-expiry janitor: double/triple promotion of the
    same `sm_unacked` queues and MUC split-brain — data duplication, not
    just degraded service. So in non-clustering mode the server itself
    acquires an **exclusive cluster-singleton lease row** at startup
    (heartbeat-renewed, Postgres `now()`-based — a degenerate
    single-entity precursor of the Phase 3 `nodes` table; SQLite gets
    exclusivity free from its single-writer file) and refuses to serve
    while another live holder exists, crash-looping with an explicit
    error. This also protects raw-manifest and non-Helm installs.
  - Delete the now-dead `persistence.allowUnsafeRwoScale` escape hatch
    (`values.yaml`, `validations.yaml`): the unconditional
    `replicaCount > 1` fail makes it unreachable — dead compatibility
    code is removed, not preserved, per project policy.
  - Set `strategy: Recreate` in the deployment while persistence uses a RWO
    PVC (the default RollingUpdate surges a second pod that deadlocks on
    Multi-Attach with attach-based storage); flip to RollingUpdate with surge
    only in Phase 4 when the PVC requirement is dropped.
  - `preStop` hook via the **Kubernetes native Sleep lifecycle action**
    (`lifecycle.preStop.sleep: {seconds: 5}` — `PodLifecycleSleepAction`
    is beta and enabled by default in Kubernetes 1.30 and **GA in 1.32**,
    not "GA in 1.30": on 1.29, or on 1.30/1.31 with the feature gate
    disabled, the pod spec fails API validation and the entire Deployment
    stops applying — a hard rollout failure, not a degraded hook. The
    version floor is therefore encoded, not merely documented: Chart.yaml
    gains **`kubeVersion: ">=1.32.0"`** — the GA version where the gate can no
    longer be disabled — rather than `">=1.30.0"`, because Helm's
    `kubeVersion` promises the chart works on *every* version at or above the
    floor, and a documented "but it hard-fails on 1.30/1.31 with the gate off"
    caveat (common in security-hardened clusters that opt out of beta gates)
    breaks that contract with no `helm install` warning (Greptile P2 on PR
    #1177). Operators pinned to 1.30/1.31 who have verified the gate is
    enabled may lower the floor via a values override, accepting the caveat
    explicitly. An exec `sleep 5`
    cannot work: the production image is a Nix `streamLayeredImage`
    containing only the waddle-server binary, cacert, and iana-etc — no
    shell, no coreutils — so an exec hook would fail with
    executable-not-found and the kubelet would proceed to SIGTERM
    immediately, silently skipping the endpoint-removal propagation bridge.
    The chart template currently has **no lifecycle block or value**; Phase
    0 adds it. Re-derive the budget: `terminationGracePeriodSeconds ≥
    preStop(5) + WADDLE_DRAIN_TIMEOUT_SECS (30) + claimReleaseBudget (a
    chart value, default 5, consumed from Phase 3 on) + kill margin(5)`,
    extend the existing `validations.yaml` grace≥drain check to encode the
    full formula, **and bump the chart default
    `terminationGracePeriodSeconds` to satisfy it** (≥ 40 at Phase 0,
    ≥ 45 once claim release lands in Phase 3): encoding the formula
    against the current 35s default would fail the chart's own render
    (35 < 5+30+5). `claimReleaseBudget` is a chart value, not a constant,
    because the drain's claim-release tail is O(owned entities), not
    O(1) — see the Phase 3 drain sequence.
  - Fail hard at startup when SM persistence has no DSN in production.
  - Ecdysis listener fd hand-off: **scoped to non-K8s/in-place binary
    restarts only.** A Kubernetes rollout replaces the pod and network
    namespace, so fd handoff cannot close the K8s listen gap; that gap is
    closed by SM resume + client reconnect.
  - Remove the ISR advertisement from stream features/disco (element 10).
- **Phase 1 — actor migration (single node):** wire `UserRegistryActor` /
  `UserActor` into the live delivery path (UserActor today has **no delivery
  surface** — this phase builds it); introduce per-connection actors wrapping
  the WebSocket sink; delete the DashMap delivery path only when each of the
  following **preserved invariants** has an equivalent test in the migration
  PR:
  1. MUC reflection/fan-out uses non-blocking `try_send` (locked Q-fix #699:
     one zombie WebSocket peer must never stall groupchat dispatch);
  2. race-safe replacement-connection retry
     (`remove_if_sender_closed_owner`, `same_channel` checks);
  3. the DirectFrame vs PeerStanza recipient-pass split;
  4. pending-flush SM row binding for the Q7b ack lifecycle;
  5. `DroppedFull`/`DroppedClosed` Prometheus accounting;
  6. **no outbound drop-rate regression vs today's `mpsc(256)` under a
     join-burst load test** (e.g. joining a 200-occupant room over a slow
     client socket) — enforced by the explicit
     `CONNECTION_ACTOR_MAILBOX_CAPACITY = 256` constant, never kameo's
     default bounded(64);
  7. **bare-JID resource/priority selection parity (RFC 6121 §8.5.2.1.1)** —
     `ConnectionRegistry` today owns resource selection for bare-JID
     routing, including the negative-priority exclusion (a resource at
     priority `-1` must never receive bare-JID-addressed stanzas); an
     equivalent `UserActor` test must assert the same selection outcome
     (highest-priority resource(s), negative-priority resources excluded)
     before the DashMap delivery path is deleted, so the retire gate covers
     routing *selection* and not only frame delivery.
  All fan-out paths use `try_send` + typed drop outcome.
- **Phase 2 — remote subsystem spike:** build the swarm subsystem (event
  loop; **keypair-pool Secret management with the Postgres CAS slot lease
  and duplicate-PeerId rejection** per element 3; peer-ID allowlist
  enforcement with read-only runtime grants and **live-connection
  revocation on refresh** per element 3; headless-DNS peer dialing
  with re-dial on pod churn; swarm-level configuration — listen addrs,
  dialing, transports; explicit `messaging::Config` limits:
  request/response size maxima, **`with_request_timeout` sized per
  element 5's timeout hierarchy** with the reply_timeout ≤
  request_timeout invariants, and `max_concurrent_streams` set against
  the modeled per-peer concurrent-ask load) behind a config flag — this
  is a new long-lived networking subsystem, not a feature toggle.
  Kademlia parameters themselves are **hardcoded by kameo 0.20** (query
  timeout 10s, replication 5, record TTL 1h, republish 30min, Server
  mode; the `kademlia` field is private, no accessor): they are
  documented, not configured — nothing about kademlia is tunable without
  the fork this ADR already rejected. Build the remote
  codec (XML-text serde wrappers for `Stanza`/`Element`, typed re-parse
  errors, nesting-depth/attribute caps, drop metrics) and the per-peer relay
  actors, **supervised with mandatory same-name re-registration on respawn
  and the `ActorNotRunning` re-lookup trigger** (element 6). Spike **exit
  criteria**: cross-node `UserActor` ask round-trip;
  **ordering verified under concurrent large/small stanzas** (libp2p
  per-substream flow control reorders naively-parallel requests); **an ask
  whose receiver handler exceeds `request_timeout` fails sender-side with
  `OutboundFailure(Timeout)`** — proving the transport cap, not
  `reply_timeout`, is the binding bound; kademlia
  re-discovery after all bootstrap peers churn in a rolling restart;
  **measured visibility window of a dead publisher's provider+metadata
  records against an explicit acceptance threshold** (the window is
  dominated by the hardcoded 1h record TTL / 30min republish and cannot
  be tuned — if it fails the threshold the only options are the rejected
  fork or upstreaming a config surface, so this is a go/no-go
  measurement; graceful stops proactively unregister, so the bound
  applies to hard-killed nodes); partition behavior. Deliverables include
  swarm observability (connected-peer gauge, kademlia routing-table size,
  bootstrap retry counter) and a **multi-process cluster test harness**
  (spawned processes or containers + shared Postgres via testcontainers,
  with fault injection: dropped tells, paused heartbeats, stale node
  records, **relay-actor panic and recover** — asserting re-registration
  under the same name and peer re-resolution after `ActorNotRunning`,
  **revoked-peer-with-live-connection** — asserting disconnect within one
  allowlist refresh interval, **lone-survivor at N=2 keeps serving while
  a node isolated from all live swarm peers with Postgres reachable
  fences**, and **a single dead link between two of three nodes degrades
  routing to the durable fallback without fencing either endpoint**) —
  kameo's `init_global()` is a process singleton, so two in-process
  swarms cannot be tested; single-process multi-swarm testing is
  unavailable by construction.
- **Phase 3 — ownership claims:** `nodes` (with the `expired` flag) +
  `claims` + `steal_intents` schema with the exact CAS/fencing SQL
  contract from element 4 (the expire CAS, the `NOT expired`
  lease-freshness heartbeat CAS, the `FOR SHARE` fencing transaction, the
  expired-flag LEFT-JOIN stale predicate, the consent/epoch-only steal
  variant, and the per-heartbeat demotion-reconciliation query), behind
  the `ClaimStore` trait, running on the **dedicated control-plane pool**;
  **DB pool-size configurability ships here** (element 12 — a drain
  prerequisite, not a Phase 4 nicety); the **Postgres-only fenced
  `SmPersistenceStorage` implementation** (a full second trait
  implementation on `Database::begin`, element 1) alongside the untouched
  portable single-node layer; epoch fencing on **every**
  `sm_sessions`/`sm_unacked`/promotion write, with the
  recipient-scoped `(recipient/stream_id, origin_stream_id, inbound_seq)`
  UNIQUE dedup constraint (so one source stanza fans out to N recipients
  as N durable rows, while retries to a fixed recipient collapse) and
  `ON CONFLICT DO NOTHING` inserts (element 5). The `pending_delivery`
  schema is specified in **one place** here to prevent dropping a column
  group: it carries (a) the per-`(origin stream → recipient)` **sequence
  number** for sticky-failover ordering gap detection, (b) the dedup
  dimensions `recipient + origin_stream_id + inbound_seq` under the
  recipient-scoped UNIQUE above, and (c) the **original ingress timestamp**
  for XEP-0203 `<delay/>` stamping on janitor flush; `sm_unacked` already
  carries `original_receipt_at_ms`, so only `origin_stream_id`/`inbound_seq`
  and the per-pair sequence are net-new there.
  SM-claim creation at `<enable/>`; claim-scoped `restore_from_persistence`,
  SM-expiry janitor with `pending_delivery` sweep-flush, and shutdown drain
  (element 9) plus the orphan reaper; cross-node SM resume steal (detached
  fenced-CAS path and live owner-handshake path with `<conflict/>` close and
  the bounded held-response retry window, element 8) with the
  authenticated-identity binding; `RoomActor` ownership with durable
  occupant roster **and durable room state — configuration, affiliation
  lists, subject — restored before the new owner accepts joins**, the
  Demote/fenced-broadcast backstop, and the re-election
  protocol (element 7); steal-intent/owner-veto unwedge path for RoomActor
  and UserActor claims (element 4); Postgres-backed ISR token store with the
  two-case failure handling (element 10). Graceful drain sequence,
  **per-entity, not phase-ordered** (phase ordering would self-fence the
  draining node's own final writes): (1) mark the node draining in `nodes`
  so it stops acquiring claims **for entities it is not already serving
  locally** — a draining node continues to hold and write under the SM
  claims of its own draining sessions, which exist since `<enable/>`;
  (2) for each owned entity, complete all final fenced writes (detach
  snapshot, Q6 promotion or explicit durable-queue handoff, occupant-roster
  flush) **while still holding the claim**, and only then release that
  entity's claim — triggering immediate re-election, not TTL wait — in
  parallel with connection drain but completing before process exit. There
  is no global "promote what remains" step after release: releasing first
  and promoting second is a fencing violation by this ADR's own rules.
  **Claim release is batched**: the ordering constraint is
  writes-before-release *per entity*, which batching preserves — entities
  whose final fenced writes have committed are released in fenced
  multi-row statements (or the release piggybacks on the entity's final
  fenced write's transaction), because a per-entity release tail of ~18k
  claims (the modeled 50k users + 5k rooms over ~3 replicas) through a
  small pool cannot fit a seconds-scale budget one statement at a time.
  If the budget still overruns, the kubelet SIGKILLs with claims
  unreleased — fencing keeps that safe, but the affected entities stall
  until lease-TTL expiry, silently degrading the "~1 move per entity per
  deploy" property, so the overrun must be *visible*: **drain
  observability is a deliverable** — a drain-duration histogram,
  `claims_released_on_drain` / `claims_abandoned_on_drain` counters, and
  an alert on nonzero abandonment — and the multi-process harness gains a
  **drain-at-modeled-scale measurement** (drain thousands of claimed
  entities; assert wall clock fits `claimReleaseBudget`) as an exit
  criterion. Rollout-aware claim placement: the `nodes` row carries the pod's
  `pod-template-hash` (downward API); during a rollout, pods whose hash
  matches the newest generation acquire released claims without backoff
  while old-generation pods back off first, so each entity moves
  approximately once per deploy instead of up to N times. Observability
  deliverables: claim acquire/steal/release/expire counters labeled by
  entity type, steal-intent filed/vetoed/expired counters, heartbeat age
  gauge **plus heartbeat-write-latency histogram + alert (the cause, not
  just the symptom — element 4)**, routing-cache hit/miss counters,
  NotOwner NACK counters (sent and received, by entity type),
  claims-table point-read rate (element 12's model inputs), per-node
  owned-entity gauges (wired into the existing
  `room_registry_gauge.rs` / `state_inventory_metrics.rs` patterns), remote
  ask latency histogram + failure counter by reason, **per-peer relay
  queue-depth and in-flight gauges, a sticky-failover activation counter
  plus a gauge of (origin stream → recipient) pairs currently diverted to
  the durable queue, a durable-queue flush-lag histogram, the
  `stalled_pending_delivery` gauge** (element 5), and the drain metrics
  above. **Test deliverables
  (per the XEP test-suite hard rule):** Postgres-backed integration tests
  for acquire/steal/heartbeat/fencing races, **including a race test that
  interleaves a steal commit inside a fenced multi-statement transaction**
  (the cross-node resurrection/double-promotion case that lockless join
  fencing fails), **the renewal-vs-expire interleaving** (renewal
  evaluated pre-expiry, committed post-steal, must return 0 rows — the
  expired-flag ordering point, element 4), steal-from-vanished-node
  (missing `nodes` row), the lapsed-lease heartbeat CAS (paused node must
  observe fencing loss on wake), steal-intent veto vs expiry, and the
  **deposed-owner-with-live-socket case** (wedged UserActor, steal at
  `intent_ttl`, reconciliation conflict-closes the socket within one
  heartbeat interval — no indefinite inbound blackhole behind a
  healthy-looking stream); a two-registry
  (two-node-simulating) XEP-0198 suite covering h-counter integrity across
  steal, `<conflict/>` close of the old stream, deferred-`h`/handoff
  coupling (**`<r/>` answered immediately with `h` excluding unresolved
  handoffs; `h` advances only after handoff ack or durable commit** —
  never an unanswered `<r/>`, per §4's MUST NOT),
  duplicate-promotion (double-janitor) prevention, dedup under
  at-least-once retry **including the resume-retransmit race (old owner's
  in-flight relay envelope vs the resumed client's retransmission through
  the new owner — same SM-ID-scoped key), the recipient-claim-move retry
  (dedup ledger travels in the snapshot), retry-after-ack-lost-post-commit
  and retry-to-new-owner-after-steal (absorbed by the durable UNIQUE
  key), and the groupchat fan-out case (one source stanza with a single
  `(origin_stream_id, inbound_seq)` reflected to N recipients all taking
  the durable fallback must persist N distinct rows, not collapse to one —
  proving the recipient dimension is in the UNIQUE key)**, the
  **un-divert reorder case** (divert, heal relay, resume —
  per-pair gap detection holds the premature direct send), the
  **multi-node janitor flush** (sender→owner handoff fails, recipient's
  socket on a third node, delivery within one sweep interval over the
  relay), the **two-simultaneous-live-resume race** (owner acks the first
  handshake; the second requester loses the consent epoch CAS and falls
  back to the detached path), and the forged-previd-wrong-identity case
  returning `not-authorized` without stealing; reconnect-storm sizing
  (claim-steal QPS for the largest tenant).
- **Phase 4 — cross-node routing GA:** DM routing across nodes over the
  ordered relay channel; MUC proxying via the remote-safe message set with
  per-node fan-out aggregation and recipient-list chunking; presence
  fan-out + anti-entropy; Helm changes: introduce `clustering.enabled`
  (requires `database.driver=postgres` — validated; replaces the Phase 0
  hard-fail on any `clustering` value), drop the RWO PVC requirement in
  favor of the existing `object_store` S3 path (**no migration of on-PVC
  uploads; breaking change per project policy** — previously issued XEP-0363
  GET URLs against the PVC die), validation that
  `clustering.enabled || replicaCount > 1` requires
  `config.s3Endpoint`+`s3Bucket` or RWX persistence (never per-pod
  emptyDir), a **dedicated headless Service with
  `publishNotReadyAddresses: true`** and a distinct swarm `containerPort`
  (readiness must NOT gate on swarm membership — bootstrap tolerates an
  empty peer set and retries continuously, avoiding cold-start deadlock).
  The **client-facing readiness probe is a distinct signal** from swarm
  membership: it reads healthy on cold start (per above) but flips to
  not-ready whenever the node enters the self-fenced state (element 4), so
  the client Service stops directing new connections at a node that has
  dropped its claims and is refusing to serve, and clears only on successful
  re-registration + claim re-acquire.
  Swarm connectivity feeds the **liveness** signal only under the same
  Postgres-relative isolation condition as heartbeat fencing (element 4):
  liveness fails only after **sustained** (minutes, ≥ M intervals — never
  one probe period) inability to reach **any** of two-or-more peers that
  the `nodes` table shows as live, **never** for a single unreachable
  peer (partial unreachability is degraded routing plus an alert, not
  restart), **never** at N=2 on swarm signal alone, and **never** for 0
  connected peers when no other live node rows exist (cold start,
  scale-down to 1, lone survivor) — otherwise a slow bootstrap, a single
  peer failure, or one bad link produces restart loops. Also: a
  **NetworkPolicy restricting ingress on the swarm port to pods matching
  the waddle-server selector** (required deliverable, defense-in-depth
  behind peer authorization, not instead of it) — shipped with a
  **CI/pre-flight chart test that positively asserts the policy admits
  intra-selector traffic on the swarm `containerPort`**, so a mis-scoped
  policy that would total-fence the cluster while Postgres stays reachable
  (element 4) cannot ship silently, a PodDisruptionBudget
  (`maxUnavailable: 1`) plus soft podAntiAffinity so node drains cannot
  evict multiple replicas at once and stampede claim-steals, RollingUpdate
  strategy (documenting expected deploy churn: with rollout-aware placement,
  ~1 re-election per room per deploy; without it, up to N — the keypair
  slot lease, not pod identity, is what prevents duplicate PeerIds during
  surge), and the **keypair-pool Secret templating plus the enrollment
  Helm hook job with its own admin-role Secret** (element 3; scale-up
  beyond the enrolled pool requires a pipeline enrollment run first —
  documented). DB pool-size configurability shipped in Phase 3 (element
  12). **GA gates:** dashboards + alerts for claim churn (including
  a deploy-window claim-churn panel), swarm partition, relay backlog,
  durable-queue diversion, drain abandonment, NotOwner NACK rate, and
  cache-miss ratio vs the pool-sizing assumption exist; XEP-0045
  re-election kick tests (status 332/110, gap-bounce) pass, **plus
  room-state survival tests: an outcast is still denied entry after an
  ownership steal (§7.2.9) and password/members-only configuration still
  holds — asserting the restore-before-joins path (element 7)**; XEP-0397
  cross-node consume tests pass,
  including **both failure cases: authenticated-but-resume-impossible
  returns `<success/>`+`<inst-resume-failed/>`+`<failed/>` and continues
  session establishment, and failed token authentication returns a XEP-0388
  `<failure/>` and asserts the detached session state and claim are
  destroyed** — all in the multi-process harness.

## Consequences

### Positive

- Coherent with ADR-0008: one concurrency model (actors) locally and across
  nodes; no new message-bus dependency (NATS/Redis) to operate.
- Typed messages end-to-end; serde only at the remote boundary, with an
  explicit codec and typed re-parse failure path instead of an assumed one.
- Postgres claims + row-locked epoch fencing give a single, transactional
  source of truth for ownership **and routing**; kademlia carries only node
  discovery, so stale DHT state can delay a dial but can never misroute an
  entity, and the design fits stock kameo 0.20 (no fork to lift the
  1024-record registry limits).
- At-least-once + dedup + ordered relay channels preserve RFC 6120 §10.1
  ordering and the server's deliver-or-error obligation across nodes, in
  **both directions**: receiver-side failures are converted to
  NACK/retry/bounce, and sender-side crash loss is excluded by coupling
  inbound `h`/`<r/>` acknowledgment to handoff-ack-or-durable-write — a
  stanza acked to the origin client is by construction already in the
  recipient's SM queue or durable storage.
- A lone survivor that can reach Postgres keeps serving (partition
  detection is Postgres-relative, not peer-count-absolute), so losing one
  of two replicas degrades capacity instead of self-inflicting a total
  outage; a node genuinely isolated from all of two-or-more live peers
  still fences, while partial link failures degrade to durable-queue
  routing instead of amputating healthy nodes (element 4).
- Rolling deploys re-elect ownership immediately via proactive per-entity
  claim release instead of stalling every owned room/user for a heartbeat
  TTL, and rollout-aware placement moves each entity approximately once per
  deploy.
- Phase 1 is a self-funding refactor (retires dead-code-in-waiting and
  external locking) even if clustering never ships — but it is a delivery-
  path rebuild with preserved invariants, not a pure move.

### Negative

- Kameo's `remote` feature is the youngest part of a young library; libp2p
  is a heavy dependency, and we own a hand-built swarm subsystem (keypair
  provisioning, allowlist, event loop) — a real operational surface.
- **An enrolled node is fully trusted.** Receiver-side claim re-validation
  and the "only originate for owned JIDs" rule bound *accidents*, not a
  hostile peer: a compromised enrolled node can acquire routing claims via
  the ordinary CAS and thereby intercept and originate traffic for any user
  (identity binding gates only SM-resume steals). This is the standard
  trust model for shared-database XMPP clusters and is stated here so it is
  not mistaken for containment; the enrollment-authority split (element 3)
  exists to keep "holds the runtime DSN" from being sufficient to *join*
  the swarm, and the steal-intent owner veto keeps a healthy owner from
  being evicted, but neither contains a fully compromised enrolled peer.
  Revocation is the containment lever, and it acts on live connections
  (element 3): a revoked peer is disconnected within one allowlist
  refresh interval, not merely refused future dials.
- The socket can never be remote: delivery always terminates at the node
  owning the TCP connection, so node death always drops live sockets and
  relies on client reconnect.
- **The single-node phases (0–3) incur a brief per-deploy connection gap.**
  Under `strategy: Recreate` with the RWO PVC, every routine release — not
  just node death — terminates the old pod before the new one starts, so
  each deploy drops all live sockets, absorbed by SM resume + client
  reconnect. Zero-downtime rolling deploys arrive only at Phase 4, once the
  RWO PVC is dropped for the `object_store` S3 path and `RollingUpdate`
  returns alongside `clustering.enabled`.
- **Hard node death, and resume-handshake timeout, forfeit live sessions'
  resume.** We chose owner-mediated handoff over write-ahead persistence of
  live streams (which would put a durable write on the hot path of every
  stanza). Consequence: sessions that were live on a crashed node have no
  durable snapshot; `<resume/>` returns `<failed/>` `<item-not-found/>`.
  When the owner is alive-but-unreachable past the bounded held-response
  window, the client receives `<failed/>` `<resource-constraint/>` instead —
  and either `<failed/>` is terminal for the client (clients clear SM state
  and bind fresh), so those sessions are likewise forfeited; clients recover
  via fresh session + MAM catch-up. Transient blips shorter than the window
  are absorbed by the held-response retry. Detached sessions and graceful
  drains are fully covered.
- Ordered acked handoff is RTT-bound per relay channel; cross-node
  throughput per (node pair) is serialized by design. Coupling inbound `h`
  to handoff means `h` advances in stream order behind the slowest
  outstanding handoff — but every client `<r/>` is still answered
  immediately with the current `h` (XEP-0198 §4's MUST NOT withhold), so
  the client-visible cost is a later-advancing ack horizon: stanzas stay
  in the client's unacked queue one relay RTT (or one durable write)
  longer and may be retransmitted on resume, which the dedup layer
  absorbs — never a withheld keepalive or a starved ack timer.
  Acceptable in-cluster; revisit with pipelining-with-reorder-buffer if it
  becomes the bottleneck. The relay backlog and diversion gauges (Phase 3)
  exist so saturation is diagnosable rather than inferred from ask latency.
- MUC ownership re-election causes occupant re-joins (visible as presence
  churn in large rooms), now bounded by jittered backoff and made *correct*
  (locally synthesized 332/110 kicks; configuration, affiliations, and
  subject restored from Postgres before the new owner accepts joins, so
  bans, passwords, and member lists survive takeovers and rolling deploys)
  at the cost of a durable write path for the occupant roster on
  join/leave and for config/affiliation/subject changes — plus, for
  archive-disabled rooms, one `FOR SHARE` fencing SELECT per broadcast as
  the deposed-owner backstop (archive-enabled rooms pay nothing extra: the
  fenced MAM insert doubles as the backstop).
- Every SM session costs one claims-row insert at `<enable/>` — a durable
  write per session establishment (not per stanza), the price of giving
  live sessions a real ownership substrate for cross-node resume.
- Per-node heartbeats mean claim staleness is detected at node granularity;
  a node that is alive but has wedged one actor holds that entity only
  until the steal-intent `intent_ttl` expires (the owner's failure to veto
  is the evidence), for RoomActor and UserActor claims alike.
- Claims, fencing, occupant rosters, room state, steal intents, and ISR
  tokens all add Postgres write paths; capacity is modeled (element 12)
  but Postgres is now availability-critical for routing decisions, not
  just storage — which is why the liveness control plane gets a dedicated
  pool and its own latency alerting.
- Swarm identity comes from a finite pre-enrolled keypair pool: scaling
  beyond the enrolled pool requires a pipeline enrollment run before the new
  pod can join the swarm — an explicit operational gate, traded for per-pod
  revocable identity on a Deployment topology. That per-key revocability is
  real for **honest decommission/rotation**, but the pool Secret is
  **shared across every pod** (the Deployment template is identical for all
  replicas — the reason the pool approach was chosen over per-pod Secrets),
  so any pod with code execution holds the private-key bytes for the *whole
  pool*, not just its CAS-leased slot. A compromised process can therefore
  present any allowlisted PeerId directly from the pool bytes without
  leasing a slot (the swarm-layer duplicate-PeerId rejection only blocks a
  *currently-connected* PeerId, not an unused pool slot), so revoking a
  single key does not contain a compromised pod — it reconnects under
  another pool key. **The practical unit of revocation under key-material
  compromise is thus the entire pool** (rotate the pool, roll every
  replica); per-key revocation contains cleanly-decommissioned/rotated
  peers, not an actively-compromised pod. The recorded StatefulSet /
  per-ordinal-Secret alternative (element 3) would dissolve this, at the
  cost of the rollout-placement and storage rework noted there.
- With exactly two nodes, a swarm-only link failure no longer fences
  either side; the pair serves in durable-queue degraded mode
  (janitor-sweep delivery latency for cross-node traffic) until the link
  heals. This trades bounded staleness for availability: the alternative
  — both endpoints fencing on one link fault, then oscillating through
  re-registration — converted a routing degradation into a total outage
  plus a repeating kick storm.

## XEP Conformance Notes

- **XEP-0198 (Stream Management):** resume semantics unchanged on the wire;
  cross-node resume is a server-internal claim steal. `h` counter integrity
  across steal is preserved from the durable snapshot for detached sessions
  and from the owner-flushed snapshot for live handoffs; when the old stream
  is still open at resume time, the old owner closes it with a `<conflict/>`
  stream error per §5. "Handled" for inbound `h` means responsibility taken:
  a cross-node-destined stanza is counted in `h` only after the receiving
  node acked the handoff or the durable fallback committed — permitted by
  the XEP and required for the deliver-or-error obligation — while every
  client `<r/>` is **answered immediately with the current `h`** per §4's
  MUST NOT (the `<a/>` response "MUST NOT be withheld for any condition
  other than a timeout"; a pending remote handoff is not a timeout). A
  stanza whose handoff is unresolved simply remains outside `h`, kept in
  the client's unacked queue; any resume-time retransmission is absorbed
  by the server-side dedup key. Negative `<resume/>` responses are
  `<failed/>` with RFC 6120
  conditions only (`<item-not-found/>` when the session is gone,
  `<resource-constraint/>` when the owner is unreachable past the bounded
  held-response window); no custom retry-after child is ever emitted, and
  the held response itself is conformant because the XEP mandates no
  response deadline. SM-IDs are never reusable after steal because the
  `complete_claim` delete is epoch-fenced and stale-owner snapshot writes
  are fenced out by the row-locked claims check. At-least-once inter-node
  retry preserves §5's no-duplication guarantee via the server-internal
  `(recipient, origin stream id, inbound sequence)` dedup key, where the
  stream id is the resumption-stable SM-ID and the inbound sequence is the
  origin stream's SM inbound-`h` counter value (both resume-stable, so a
  client retransmission re-lands on the identical key), the key is durably
  enforced by a UNIQUE constraint **scoped by recipient** on the
  fallback/promotion tables (one source stanza fans out to N recipients as
  N rows; only retries to a fixed recipient collapse), and the
  receiver-side per-(recipient, origin-stream)
  dedup ledger travels inside the recipient's SM snapshot across claim
  moves (element 5) — so the guarantee holds across stream resumption,
  resume-steal, and recipient-claim movement, not only under static
  ownership; XEP-0359 origin-ids are
  passed through for client-side dedup and never used as a server-side
  suppression key. XEP-0198 acks are hop-scoped; end-to-end responsibility
  is carried by the acked handoff + durable fallback, never attributed to
  client acks.
- **XEP-0045 (MUC):** single owning `RoomActor` preserves total ordering of
  broadcasts per room; the pre-fan-out fenced statement guarantees a deposed
  owner broadcasts nothing after a steal commits. On ownership-epoch change,
  each occupant's local node synthesizes `<presence type='unavailable'/>`
  with status **332** (and **110** on self-presence) from the durable
  occupant roster, satisfying the service-shutdown obligation and triggering
  standard client re-join; gap-window messages bounce with
  `<resource-constraint/>`. Room configuration, affiliation lists, and
  subject are durable under the room claim and restored before the new
  owner accepts joins, so §5.2's long-lived affiliations and §7.2.9's
  outcast denial hold across takeover (no ban evasion via re-election),
  password/members-only protection persists, and §7.2.15
  subject-after-join is served from the restored subject.
- **XEP-0397 (ISR) / XEP-0388 (SASL2):** not advertised until the
  Postgres-backed, epoch-fenced, single-use token store ships in Phase 3
  (lookup by the non-secret SM-ID, constant-time comparison in Rust,
  fenced delete — never a token-matching SQL `WHERE` clause, which is a
  timing oracle). Failure handling
  follows the XEP's two distinct cases: authenticated-but-resume-impossible
  returns `<success/>` containing `<inst-resume-failed/>` wrapping the
  XEP-0198 `<failed/>` (client continues normal session establishment);
  failed ISR token authentication returns a XEP-0388 `<failure/>` Nonza
  **and destroys the detached session state and claim identified by the
  SM-ID**, per the XEP's anti-brute-force MUST — never a preserved session
  or a fallback to ordinary resume.
- **XEP-0313 (MAM) / XEP-0160 (offline):** unchanged on the wire; Q6
  promotion and MAM writes become epoch-fenced and claim-scoped. Q6
  promotion path already stamps XEP-0203 `<delay/>`; the new durable
  `pending_delivery` cross-node fallback + janitor-flush path (element 5)
  stamps `<delay/>` identically, using each row's persisted original
  ingress timestamp, so a stanza delayed by a sweep interval is marked as
  deferred delivery on the wire exactly like a promoted offline stanza.
- **RFC 6120 §10.1 / RFC 6121 §8.5.2:** in-order per-stream processing is
  guaranteed by the per-peer relay channels + sticky durable-queue failover;
  resource selection moves from `ConnectionRegistry` into `UserActor`, which
  owns all resources for a bare JID regardless of node, and receives state
  mutations only via acked asks.
- Every phase that changes XEP behavior carries that XEP's dedicated Rust
  test suite in the same PR, per the project hard rule (multi-node cases run
  in the Phase 2 multi-process harness).
