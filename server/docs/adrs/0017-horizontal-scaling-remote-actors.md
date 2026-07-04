# ADR-0017: Horizontal Scaling via Remote Actors with Postgres-Authoritative Ownership

## Status

Draft (revised after adversarial review council, round 2)

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
   fenced implementation of `SmPersistenceStorage`** (a fencing decorator
   that wraps each write in the claims-row-locked transaction of element 4),
   chosen when `clustering.enabled`. Cluster mode never routes SM writes
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
   - Each replica's libp2p static keypair is provisioned from a mounted
     Kubernetes Secret; the cluster maintains an **allowlist of enrolled peer
     IDs** (rows in Postgres, refreshed periodically). Connections from peer
     IDs not on the allowlist are rejected at the swarm behaviour layer —
     completing the Noise handshake is necessary but never sufficient.
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
   - `nodes (node_id, node_epoch, heartbeat, pod_template_hash)` — one
     liveness row per replica, one heartbeat CAS per node per interval.
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
     expired, **missing**, or its `node_epoch` predates the node's current
     registration. The stale predicate is written as a `LEFT JOIN` against
     `nodes` with `nodes.node_id IS NULL OR heartbeat expired OR node_epoch
     mismatch`, so a claim whose owner row never registered or was reaped is
     definitionally stale rather than permanently unstealable.

   The exact SQL contract (so "transactional" cannot be implemented as a
   broken READ COMMITTED SELECT-then-UPDATE):
   - *Acquire*: `INSERT ... ON CONFLICT (entity) DO NOTHING` +
     `rows_affected == 1` check.
   - *Steal*: single-statement CAS —
     `UPDATE claims SET node_id=$me, node_epoch=$my_node_epoch,
     claim_epoch = claim_epoch + 1 WHERE entity=$e AND claim_epoch=$observed
     AND <owner-stale LEFT-JOIN predicate over nodes>` with
     `rows_affected == 1` required. Losers observe 0 rows and give up.
     The snapshot read of `nodes.heartbeat` inside this CAS is safe only
     because lease expiry is **monotone** (see *Heartbeat*): an expired
     owner cannot concurrently refresh itself back to fresh.
   - *Heartbeat*: renewal is itself a CAS on lease freshness —
     `UPDATE nodes SET heartbeat = now() WHERE node_id=$me AND
     node_epoch=$mine AND heartbeat >= now() - $lease_ttl`.
     `rows_affected == 0` is **fencing loss**, and now unambiguously means
     "your lease lapsed (or your epoch was superseded)": the node must
     immediately demote all local actors, drop all claims, stop writing,
     and may return to service only by re-registering with a fresh
     `node_id`/`node_epoch` and re-acquiring claims from scratch. This makes
     expiry monotone — once a node row is expired it stays expired until
     explicit re-registration — so a GC/VM pause longer than the pause
     budget cannot resurrect liveness mid-reap (half-reaped state, ghost
     synthesized-unavailable presence, staleness flapping under the reaper
     are all excluded by the predicate, not by node-local timers). All time
     predicates use **Postgres `now()`**, never node-local
     `chrono::Utc::now()`, so only one clock matters. Stored timestamps that
     feed those predicates are also DB-stamped: `detached_at` is derived
     from `now()` inside the fenced detach write, never bound from a
     node-supplied value (node clocks stamp timestamps only in single-node
     SQLite, where one clock exists by construction). Lease TTL ≥
     heartbeat-interval × N-missed + max GC/pause budget.
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
     stealable (guaranteed by the TTL formula). Heartbeat renewal is
     additionally coupled to self-health, with the condition detecting
     **partition, not peer absence**: a node refuses to renew only when the
     `nodes` table shows at least one *other* node with a fresh heartbeat
     (live per Postgres, the authority) that this node cannot reach over the
     swarm for M consecutive intervals, or when the actor runtime fails an
     internal ping. A node whose peers are all heartbeat-expired in Postgres
     — the lone survivor at `replicaCount: 2`, or the first pod of a rolling
     restart — **keeps renewing and serving**: Postgres, not the swarm, is
     the ownership authority, and a survivor that can reach Postgres is safe.
     "Partitions degrade to slower lookups" is true only because of this
     rule; a total pairwise swarm partition with Postgres still reachable
     fences every side, an accepted, conservative, and rare failure mode
     (swarm and Postgres largely share network fate). The intended replica
     count is never configured into the node — it is read as live `nodes`
     rows.
   - *Unwedge (steal-intent with owner veto — replaces "forced steal")*:
     no node may evict an owner whose lease is fresh on its own unverifiable
     say-so ("I saw N failures" cannot be attested in a CAS, so it would be
     a room/user-takeover primitive for any enrolled node). Instead, after N
     consecutive failed/NACKed remote deliveries to a fresh-lease owner, the
     frustrated node writes a `steal_intents (entity, reporter_node,
     created_at DEFAULT now())` row. Every owner's heartbeat loop reads
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
     discovery, reusing element 8's demote machinery), bounding a
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
   - **Inbound `h` accounting is coupled to handoff.** A cross-node-destined
     inbound stanza counts as "handled" for the origin client's stream —
     included in inbound `h` and eligible to satisfy the client's `<r/>` —
     only after the handoff ack arrives or the durable fallback write
     commits. XEP-0198 defines "handled" as the server having taken
     responsibility; we take responsibility exactly at owner-acked or
     durable handoff, never before. `h` advances in stream order, so an
     unresolved handoff for stanza k defers acking k and everything after
     it. This closes the sender-node-crash loss window: a stanza the server
     has acked to the origin client is, by construction, already in the
     recipient's SM queue or in durable storage. (Latency consequence
     recorded in Consequences.)
   - Whenever a node inserts `pending_delivery` rows for a user owned
     elsewhere, it sends a **flush poke** to the owning `UserActor` — as an
     **acked ask with bounded retries** feeding a `stalled_pending_delivery`
     gauge/alert on exhaustion, because the poke travels the same possibly
     broken path that forced the durable fallback. The poke is an
     optimization, not the guarantee: the **guaranteed flush path is the
     owning node's claim-scoped janitor**, which periodically sweeps
     `pending_delivery` for bare JIDs whose `UserActor` claim it holds and
     flushes rows to any locally connected resource. Both sides of a partial
     swarm partition still reach Postgres, so delivery delay for an online
     user is bounded by one sweep interval regardless of swarm reachability
     (the existing presence-triggered flush in `pending_delivery/flush.rs`
     remains the fast path on reconnect). Persistent poke failure also files
     a `steal_intents` row for the UserActor claim (element 4), bounding the
     wedged-owner case.
   - Retries are idempotent via a dedup key **exclusively** the
     server-internal `(origin stream-id, inbound sequence)` carried in the
     envelope, preserving XEP-0198's no-duplication guarantee under
     at-least-once. XEP-0359 `origin-id` is client-controlled input: it is
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
     durable queue, all subsequent stanzas for that pair divert too until
     the queue has flushed in order.
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
   - The owning `RoomActor` persists the room's **occupant roster (real JID,
     nick, occupant-id)** in Postgres alongside the room claim, epoch-fenced
     like all claimed-entity writes.
   - **Deposed-owner demotion is a two-part protocol**, because epoch
     fencing alone only fires on durable writes and a quiet room might never
     perform one: (1) after any steal CAS succeeds, the new owner sends a
     best-effort acked `Demote { entity, new_epoch }` to the old owner via
     its node relay; the recipient tombstones the entity and NotOwner-NACKs
     subsequent traffic. (2) As the guaranteed backstop, the owning
     `RoomActor` passes **every broadcast through one fenced statement
     before local fan-out** — the MAM archive insert where archiving is on
     (already fenced, so free), otherwise a conditional epoch-check `UPDATE`
     touching the claims row. Because fencing takes a `FOR SHARE` lock on
     the claims row (element 4), this statement is ordered against the steal:
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
   - **Detached session owned elsewhere**: the receiving node steals the
     claim via the fenced CAS, loads the durable snapshot, and resumes. The
     final durable delete in `complete_claim` is epoch-fenced, so of two
     simultaneous `<resume/>` attempts on different nodes exactly one wins;
     the loser returns `<failed/>`.
   - **Live session owned elsewhere** (the common mobile roaming case — old
     socket still open, no durable snapshot yet): steal is a **handshake,
     not a bare table write**. The stealing node resolves the owner from the
     claims row and asks it (remote ask with timeout) to detach-flush a
     snapshot and close the old stream with a `<conflict/>` stream error per
     XEP-0198 §5; only on ack does the fenced epoch-bump CAS on the claims
     row commit and the snapshot get loaded. This also prevents the old node
     from continuing to count/send on a half-open socket and diverging `h`.
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
     promote them. Monotone lease expiry (element 4) guarantees a reaped
     node cannot flip back to fresh mid-sweep.
   - A stanza that arrives at a node whose claim was stolen mid-flight is
     re-routed: the node re-reads the claims table and relays to the new
     owner (element 6's `NotOwner` path applies symmetrically).

10. **XEP-0397 ISR becomes cluster-correct or is not advertised.**
    `IsrTokenStore` moves to Postgres, keyed to the SM claim: token consume
    is an atomic single-use `DELETE ... RETURNING` with constant-time token
    comparison, epoch-fenced like SM steals, and bound to the same
    authenticated-identity check as resume (element 8). A wrong-node ISR
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
      fenced-expiry event that triggers SM drain. Monotone lease expiry
      (element 4) guarantees the reaped owner cannot return mid-sweep and
      contradict the synthesized presence.
    - Presence **probes are answered only by the authoritative owner**, never
      from a cached remote view.
    - The remote presence message set is typed end-to-end:
      `UpdatePresenceState`'s `show: Option<String>` is converted to
      `xmpp_parsers::presence::Show` **before** any remote message set is
      defined, per the typed-payloads rule.

12. **Database capacity is planned, not discovered.** Pool size becomes a
    `DatabaseConfig` field surfaced through Helm values (both adapters
    currently hardcode `.max_connections(10)`). The ADR's load model per
    replica: claims point-reads at (stanza rate × in-process-cache-miss
    ratio + NotOwner NACK rate) — the cache is process-local with NotOwner
    invalidation (element 6), so the miss ratio, not a DHT hit rate, is the
    modeled variable; one heartbeat CAS per interval (per node, element 4);
    claim CAS on enable/resume/join/steal; per-broadcast fencing statements
    for archive-off rooms (element 7); and claim-scoped janitor batches —
    plus the existing per-subsystem pools. Deployment docs must budget total
    connections (replicas × pools × size) against Postgres
    `max_connections`; PgBouncer in transaction mode is compatible with this
    design **because** the contract is single-statement CAS plus
    transaction-scoped `FOR SHARE` row locks, never session advisory locks —
    one more reason advisory locks are prohibited here.

## Implementation Plan

Phased so each step ships value independently. `replicaCount: 1` remains the
default until the final phase, **enforced by the chart, not by convention**.

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
  - Set `strategy: Recreate` in the deployment while persistence uses a RWO
    PVC (the default RollingUpdate surges a second pod that deadlocks on
    Multi-Attach with attach-based storage); flip to RollingUpdate with surge
    only in Phase 4 when the PVC requirement is dropped.
  - `preStop` hook via the **Kubernetes native Sleep lifecycle action**
    (`lifecycle.preStop.sleep: {seconds: 5}`, GA in Kubernetes 1.30;
    document the minimum Kubernetes version in the chart). An exec `sleep 5`
    cannot work: the production image is a Nix `streamLayeredImage`
    containing only the waddle-server binary, cacert, and iana-etc — no
    shell, no coreutils — so an exec hook would fail with
    executable-not-found and the kubelet would proceed to SIGTERM
    immediately, silently skipping the endpoint-removal propagation bridge.
    The chart template currently has **no lifecycle block or value**; Phase
    0 adds it. Re-derive the budget: `terminationGracePeriodSeconds ≥
    preStop(5) + WADDLE_DRAIN_TIMEOUT_SECS (30) + claimReleaseBudget(5,
    Phase 3+) + kill margin(5)` and extend the existing `validations.yaml`
    grace≥drain check to encode the full formula (current 35s grace + 30s
    drain leaves no room for either hook or claim release).
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
     default bounded(64).
  All fan-out paths use `try_send` + typed drop outcome.
- **Phase 2 — remote subsystem spike:** build the swarm subsystem (event
  loop, Secret-mounted keypair management, peer-ID allowlist enforcement
  with read-only runtime grants per element 3, headless-DNS peer dialing
  with re-dial on pod churn, kademlia config, explicit `messaging::Config`
  request/response size maxima) behind a config flag — this is a new
  long-lived networking subsystem, not a feature toggle. Build the remote
  codec (XML-text serde wrappers for `Stanza`/`Element`, typed re-parse
  errors, nesting-depth/attribute caps, drop metrics) and the per-peer relay
  actors. Spike **exit criteria**: cross-node `UserActor` ask round-trip;
  **ordering verified under concurrent large/small stanzas** (libp2p
  per-substream flow control reorders naively-parallel requests); kademlia
  re-discovery after all bootstrap peers churn in a rolling restart;
  **measured visibility window of a dead publisher's provider+metadata
  records** (the true staleness bound for node discovery, element 6);
  partition behavior. Deliverables include swarm observability
  (connected-peer gauge, kademlia routing-table size, bootstrap retry
  counter) and a **multi-process cluster test harness** (spawned processes
  or containers + shared Postgres via testcontainers, with fault injection:
  dropped tells, paused heartbeats, stale node records, **lone-survivor at
  N=2 keeps serving while a swarm-partitioned node with Postgres-live
  unreachable peers fences**) — kameo's `init_global()` is a process
  singleton, so two in-process swarms cannot be tested; single-process
  multi-swarm testing is unavailable by construction.
- **Phase 3 — ownership claims:** `nodes` + `claims` + `steal_intents`
  schema with the exact CAS/fencing SQL contract from element 4 (including
  the `FOR SHARE` fencing transaction, the lease-freshness heartbeat CAS,
  and the LEFT-JOIN stale predicate), behind the `ClaimStore` trait; the
  **Postgres-only fenced `SmPersistenceStorage` implementation** (fencing
  decorator) alongside the untouched portable single-node layer (element 1);
  epoch fencing on **every** `sm_sessions`/`sm_unacked`/promotion write;
  SM-claim creation at `<enable/>`; claim-scoped `restore_from_persistence`,
  SM-expiry janitor with `pending_delivery` sweep-flush, and shutdown drain
  (element 9) plus the orphan reaper; cross-node SM resume steal (detached
  fenced-CAS path and live owner-handshake path with `<conflict/>` close and
  the bounded held-response retry window, element 8) with the
  authenticated-identity binding; `RoomActor` ownership with durable
  occupant roster, the Demote/fenced-broadcast backstop, and the re-election
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
  Rollout-aware claim placement: the `nodes` row carries the pod's
  `pod-template-hash` (downward API); during a rollout, pods whose hash
  matches the newest generation acquire released claims without backoff
  while old-generation pods back off first, so each entity moves
  approximately once per deploy instead of up to N times. Observability
  deliverables: claim acquire/steal/release/expire counters labeled by
  entity type, steal-intent filed/vetoed/expired counters, heartbeat age
  gauge, per-node owned-entity gauges (wired into the existing
  `room_registry_gauge.rs` / `state_inventory_metrics.rs` patterns), remote
  ask latency histogram + failure counter by reason, **per-peer relay
  queue-depth and in-flight gauges, a sticky-failover activation counter
  plus a gauge of (origin stream → recipient) pairs currently diverted to
  the durable queue, a durable-queue flush-lag histogram, and the
  `stalled_pending_delivery` gauge** (element 5). **Test deliverables
  (per the XEP test-suite hard rule):** Postgres-backed integration tests
  for acquire/steal/heartbeat/fencing races, **including a race test that
  interleaves a steal commit inside a fenced multi-statement transaction**
  (the cross-node resurrection/double-promotion case that lockless join
  fencing fails), steal-from-vanished-node (missing `nodes` row), the
  lapsed-lease heartbeat CAS (paused node must observe fencing loss on
  wake), and steal-intent veto vs expiry; a two-registry
  (two-node-simulating) XEP-0198 suite covering h-counter integrity across
  steal, `<conflict/>` close of the old stream, deferred-`h`/handoff
  coupling (client `<r/>` unanswered until handoff ack or durable write),
  duplicate-promotion (double-janitor) prevention, dedup under
  at-least-once retry, and the forged-previd-wrong-identity case returning
  `not-authorized` without stealing; reconnect-storm sizing (claim-steal
  QPS for the largest tenant).
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
  Swarm connectivity feeds the **liveness** signal only under the same
  Postgres-relative condition as heartbeat fencing (element 4): liveness
  fails only after **sustained** (minutes, ≥ M intervals — never one probe
  period) inability to reach peers that the `nodes` table shows as live,
  and **never** for 0 connected peers when no other live node rows exist
  (cold start, scale-down to 1, lone survivor) — otherwise a slow bootstrap
  or a single peer failure at N=2 produces restart loops. Also: a
  **NetworkPolicy restricting ingress on the swarm port to pods matching
  the waddle-server selector** (required deliverable, defense-in-depth
  behind peer authorization, not instead of it), a PodDisruptionBudget
  (`maxUnavailable: 1`) plus soft podAntiAffinity so node drains cannot
  evict multiple replicas at once and stampede claim-steals, RollingUpdate
  strategy (documenting expected deploy churn: with rollout-aware placement,
  ~1 re-election per room per deploy; without it, up to N), and configurable
  DB pool size. **GA gates:** dashboards + alerts for claim churn (including
  a deploy-window claim-churn panel), swarm partition, relay backlog, and
  durable-queue diversion exist; XEP-0045 re-election kick tests (status
  332/110, gap-bounce) pass; XEP-0397 cross-node consume tests pass,
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
  outage; a genuinely partitioned minority still fences.
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
- The socket can never be remote: delivery always terminates at the node
  owning the TCP connection, so node death always drops live sockets and
  relies on client reconnect.
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
  throughput per (node pair) is serialized by design, and coupling inbound
  `h` to handoff means a client's `<r/>` for cross-node-destined stanzas is
  answered one relay RTT (or one durable write) later than today, with `h`
  advancing in stream order behind the slowest outstanding handoff.
  Acceptable in-cluster; revisit with pipelining-with-reorder-buffer if it
  becomes the bottleneck. The relay backlog and diversion gauges (Phase 3)
  exist so saturation is diagnosable rather than inferred from ask latency.
- MUC ownership re-election causes occupant re-joins (visible as presence
  churn in large rooms), now bounded by jittered backoff and made *correct*
  (locally synthesized 332/110 kicks) at the cost of a durable occupant
  roster write path on join/leave — plus, for archive-disabled rooms, one
  conditional epoch-check statement per broadcast as the deposed-owner
  backstop (archive-enabled rooms pay nothing extra: the fenced MAM insert
  doubles as the backstop).
- Every SM session costs one claims-row insert at `<enable/>` — a durable
  write per session establishment (not per stanza), the price of giving
  live sessions a real ownership substrate for cross-node resume.
- Per-node heartbeats mean claim staleness is detected at node granularity;
  a node that is alive but has wedged one actor holds that entity only
  until the steal-intent `intent_ttl` expires (the owner's failure to veto
  is the evidence), for RoomActor and UserActor claims alike.
- Claims, fencing, occupant rosters, steal intents, and ISR tokens all add
  Postgres write paths; capacity is modeled (element 12) but Postgres is
  now availability-critical for routing decisions, not just storage.

## XEP Conformance Notes

- **XEP-0198 (Stream Management):** resume semantics unchanged on the wire;
  cross-node resume is a server-internal claim steal. `h` counter integrity
  across steal is preserved from the durable snapshot for detached sessions
  and from the owner-flushed snapshot for live handoffs; when the old stream
  is still open at resume time, the old owner closes it with a `<conflict/>`
  stream error per §5. "Handled" for inbound `h` means responsibility taken:
  a cross-node-destined stanza is counted (and the client's `<r/>` answered)
  only after the receiving node acked the handoff or the durable fallback
  committed — permitted by the XEP and required for the deliver-or-error
  obligation. Negative `<resume/>` responses are `<failed/>` with RFC 6120
  conditions only (`<item-not-found/>` when the session is gone,
  `<resource-constraint/>` when the owner is unreachable past the bounded
  held-response window); no custom retry-after child is ever emitted, and
  the held response itself is conformant because the XEP mandates no
  response deadline. SM-IDs are never reusable after steal because the
  `complete_claim` delete is epoch-fenced and stale-owner snapshot writes
  are fenced out by the row-locked claims check. At-least-once inter-node
  retry preserves §5's no-duplication guarantee via the server-internal
  `(origin stream-id, inbound sequence)` dedup key; XEP-0359 origin-ids are
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
  `<resource-constraint/>`.
- **XEP-0397 (ISR) / XEP-0388 (SASL2):** not advertised until the
  Postgres-backed, epoch-fenced, single-use (`DELETE ... RETURNING`,
  constant-time compare) token store ships in Phase 3. Failure handling
  follows the XEP's two distinct cases: authenticated-but-resume-impossible
  returns `<success/>` containing `<inst-resume-failed/>` wrapping the
  XEP-0198 `<failed/>` (client continues normal session establishment);
  failed ISR token authentication returns a XEP-0388 `<failure/>` Nonza
  **and destroys the detached session state and claim identified by the
  SM-ID**, per the XEP's anti-brute-force MUST — never a preserved session
  or a fallback to ordinary resume.
- **XEP-0313 (MAM) / XEP-0160 (offline):** unchanged on the wire; Q6
  promotion and MAM writes become epoch-fenced and claim-scoped. Q6
  promotion path already stamps XEP-0203 `<delay/>`.
- **RFC 6120 §10.1 / RFC 6121 §8.5.2:** in-order per-stream processing is
  guaranteed by the per-peer relay channels + sticky durable-queue failover;
  resource selection moves from `ConnectionRegistry` into `UserActor`, which
  owns all resources for a bare JID regardless of node, and receives state
  mutations only via acked asks.
- Every phase that changes XEP behavior carries that XEP's dedicated Rust
  test suite in the same PR, per the project hard rule (multi-node cases run
  in the Phase 2 multi-process harness).
