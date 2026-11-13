# Durable stream-management session model

Issue: #2139 under #2046 package F. This record fixes the durable XEP-0198
session model that the store and authority layer implements. It is the
contract for the domain types in `waddle-xmpp`
(`stream_management::durable`) and the repository in `waddle-server`
(`sm_authority`). The connection actor calls the authority; it does not own
any of the state below.

## Decisions where the planning notes differ

| Question | Decision | Reason |
| --- | --- | --- |
| Is the client-visible resume id the storage key? | No. `SmSessionKey` (a UUID) keys every row. The resume id is a separate random `SmResumeToken`; only its SHA-256 is stored. | `2139-p33-sm-lifecycle-transition-model.md` forbids a plaintext previd as durable identity. The legacy store keys `sm_sessions` by the previd. |
| Which generation fences writes? | `SessionGeneration`, a checked nonwrapping counter stored on the session row, paired with the exact `BindingRef` of the bind or resume attempt that owns it. | `2046-binding-ref-identity.md` states `BindingRef` is equality evidence only. `PhysicalTransportGeneration` and `LocalConnectionBindingId` are process-local and never stored. |
| Is `EnablingLive` a stored state? | No. Enable is one transaction that inserts the row as `Live`. `Enabling` exists only in the pure state machine, for the caller between starting the commit and holding its witness. | The lifecycle fixture models a commit receipt because it has no transaction. A stored pre-commit state would need its own recovery and adds nothing a single commit lacks. |
| Is `TransferHydrating` a stored state? | Yes. | The resume commit and the endpoint publication are separate events on the new node (D4 in `2046-completion-contracts.md`); a crash between them must be recoverable from the row. |
| Per-frame journal (`sm_custody`) or session window? | A session window: nonwrapping `acked`, `exposure_ceiling` and `appended` ordinals on the session row, with one `sm_unacked` row per retained frame. | `2046-sm-production-custody.md` requires a stream-wide ordinal for every countable stanza, not only prepared direct messages. The `sm_custody` candidate admits one frame class and stays unselected. |
| Does a resume from `Live` wait for the old transport? | No. The resume transaction advances the generation; the old binding's next write fails its fence. Closing the old transport is cleanup the caller may attempt. | Cross-node transfer cannot observe the old pump (D4). A fence that depended on a close message would not hold when the message is lost. |

## Identity

- **Session**: `SmSessionKey`. It is the `stream_id` of the session's
  `sm_sessions` and `sm_unacked` rows and the id of its `sm_session` claim.
- **Resume token**: `SmResumeToken`, 32 random bytes, base64url on the wire as
  the `id` of `<enabled/>` and the `previd` of `<resume/>`. The store holds
  `SHA-256(token)` under a unique index. A digest collision rejects the enable
  as a known failure; the caller mints a new operation.
- **Generation**: starts at 1 on enable. Each committed resume stores
  `generation + 1` together with the new attempt's `BindingRef`. The owner
  fence is `(session, generation, binding)`; every owner write names all three
  and the store compares them under the row lock.
- **Claim**: on a clustered node every write also asserts the exact
  `clustering_claims` row `(sm_session:<session>, node id, node epoch, claim
  epoch)` under `FOR SHARE`, before the session row lock. A node whose claim
  was taken, by a resume elsewhere or after its lease lapsed, has no such
  row and loses write authority with it. Single-node SQLite has no claim.
- **Operation**: enable, resume and each admission carry a caller-minted
  `SmOperationId`. One row per operation in `sm_durable_operations` records
  its outcome.

## Principal reference

Stored: `AuthenticatedPrincipalRef` only: bare JID, auth-context id,
auth-context version and principal auth epoch. No credential, token, SASL
payload or permission snapshot is stored.

Compared at resume, in this order:

1. The stored reference resolves as current against the session authority
   (`PrincipalStanding::Current`). A missing reference rejects as
   `PrincipalUnavailable`; a diverged one as `PrincipalDiverged`.
2. The presenting connection's principal has the stored bare JID.
3. The presenting principal has the stored principal auth epoch. The auth
   context may differ: a client may authenticate the resuming transport in a
   new context.

A committed resume stores the presenting reference. Admission compares the
supplied current principal with the stored one for exact equality.

## Stored record

`sm_durable_sessions`, one row per session:

| Field | Meaning |
| --- | --- |
| `session_id` | `SmSessionKey` |
| `resume_token_sha256` | unique lookup digest |
| `full_jid` | the bound resource; unchanged by resume |
| `state`, `terminal_reason` | lifecycle state |
| `generation`, `binding` | current owner fence |
| principal columns | the reference above |
| `owner_node_id`, `owner_node_epoch`, `claim_epoch` | the claim the current generation was committed under; absent on single-node |
| `inbound_handled` | server-side handled count last reported by the owner |
| `outbound_acked`, `outbound_exposure_ceiling`, `outbound_appended` | nonwrapping ordinals, `acked <= ceiling <= appended` |
| `outbound_unacked_bytes` | payload bytes of retained frames |
| `max_resume_secs`, `detached_at_ms` | resume window |
| `enable_operation`, `last_operation` | operation identities |
| `updated_at_ms` | last write, used to age released tombstones |
| `occupancy_session` | occupancy generation the resource is published under; set at enable and detach (V1036) |

Reused relations: `sm_unacked` holds the retained frames (exact wire bytes,
receipt time, ingress receipts, provenance) with `sequence` equal to the
ordinal's 32-bit wire value; `sm_sessions` holds the resource snapshot
(presence, carbons, roster and blocklist interest, occupancy session) from
the detach until a resume is completed. Under the durable selector the
authority is the only writer of those rows for the sessions it enabled.

The inbound handled count for stanzas that entered canonical ingress remains
owned by `ingress_sm_streams`. `inbound_handled` is the value the owner last
reported; a resume returns it as a floor and the caller joins the ingress
checkpoint as it does today.

## Counters and exposure bounds

Ordinals are nonwrapping `u64`. The wire value is the ordinal modulo `2^32`.

- `appended`: last ordinal with a committed frame.
- `exposure_ceiling`: last ordinal the owner may have written to a
  transport. It is committed before the write. A `Live` admission raises it
  with the append; a detached or hydrating admission does not.
- `acked`: last ordinal the peer acknowledged.

Bounds: at most `max_outstanding` retained frames (never above `2^31 - 1`)
and `max_outstanding_bytes` retained payload bytes. An admission that would
exceed either is refused before mutation; the caller keeps the frame out of
the memory queue and routes it as undeliverable to this session.

## Handled-count validation

For a wire `h`, `delta = h - wire(acked) (mod 2^32)`:

| `delta` | Live `<a/>` | `<resume/>` |
| --- | --- | --- |
| `0` | unchanged | unchanged |
| `1 ..= ceiling - acked` | advance to `acked + delta` | advance |
| above that, below `2^31` | `HandledTooHigh` | `HandledTooHigh` |
| exactly `2^31` | `Ambiguous` | `Ambiguous` |
| above `2^31` | ignored as stale | `HandledBehind` |

Nothing is deleted on any error row. An advance deletes exactly the
`sm_unacked` rows in `(acked, acked + delta]` and the admission outcomes of
those frames. An admission is idempotent under its operation id while its
frame is retained; once the frame is acknowledged the same operation would be
admitted as a new frame.

## State machine

States: `Enabling` (not stored), `Live`, `Detached`, `TransferHydrating`,
`Terminal(reason)` with reasons `EnableAbandoned`, `Expired`, `Replaced`,
`Closed`.

| From | Event | To |
| --- | --- | --- |
| `Enabling` | enable committed | `Live` |
| `Enabling` | enable abandoned | `Terminal(EnableAbandoned)` |
| `Live` | detach | `Detached` |
| `Live`, `Detached`, `TransferHydrating` | resume claimed | `TransferHydrating`, generation + 1 |
| `TransferHydrating` | resume completed | `Live` |
| `TransferHydrating` | detach (compensation or transport loss) | `Detached` |
| `Live`, `TransferHydrating` | owner lost | `Detached` |
| `Detached` | expire | `Terminal(Expired)` |
| `Live`, `Detached`, `TransferHydrating` | replace | `Terminal(Replaced)` |
| `Live`, `Detached`, `TransferHydrating` | close | `Terminal(Closed)` |

Every other pair is a typed error and changes nothing. `Terminal` absorbs.

Recovery of each intermediate condition:

- **Enable commit unknown**: the caller keeps its `EnableOperation` and asks
  the store to resolve it. Resolution inserts an abandonment marker under the
  operation id; the unique key lets exactly one of the original commit and the
  marker exist. Committed returns the same witness; abandoned is a known
  failure. While the original backend still holds the key the resolution
  stays unresolved. The caller never issues a second enable for the binding
  until resolution is known.
- **`TransferHydrating`, new owner alive**: it completes (to `Live`) or
  compensates (to `Detached`) under its exact fence.
- **`TransferHydrating` or `Live`, owner gone**: a later authorized resume
  claims the row directly. `detached_at_ms` is set by the first detach or
  owner-lost record, or by a resume that takes the session from a live owner,
  and is cleared only when a resume completes. A failed hydration does not
  renew it. Expiry applies to `Detached` rows; a row left `Live` or
  `TransferHydrating` by a dead owner becomes `Detached` through an
  owner-lost record: by a successor holding the session claim on a clustered
  store, or for every owned row after a single-node restart.
- **Resume commit unknown**: resolved like enable. Only a known rejection
  yields the permit for a fresh bind; an unresolved operation yields none.
- **Late compensation or cleanup**: it names an old generation and fails the
  fence without effect.
- **Terminal with retained frames**: the rows stay in `sm_unacked` as custody
  until the consumer that transfers them to offline delivery releases them.

## Resume

One transaction: look up the digest; lock the row; authorize the principal;
validate `h` against `(acked, exposure_ceiling)`; settle the acknowledged
prefix; store `generation + 1`, the new binding, the presenting principal and
the claim; set `TransferHydrating`; read the detached resource snapshot;
record the operation. The witness carries the new fence, the restored
resource state, the inbound floor, the superseded binding and the replay
range `(acked, appended]`. The snapshot row stays until the resume
completes, so a compensated or abandoned resume leaves it for the next one
and a witness rebuilt after an unknown commit still carries it.

Replay precedes live output by ordinal. Admissions that commit during
hydration append above the replay range without exposure. Completing the
resume moves the session to `Live`, raises the ceiling to `appended`, and
returns that tail; the caller writes the replay range, then the tail, then
accepts live frames. Completion also deletes the resource snapshot.

## Resume on a node that does not hold the claim

The session claim stays with the node that enabled or last resumed the
session until the session is terminal; a detach keeps it. A resume on
another node moves the claim before its transaction, because the transaction
asserts the claim row. The connection decides from the claim row and the
session row together
(`W/stream_management/durable/claim.rs`, `W` =
`server/crates/waddle-server/src/server/routes/websocket`):

| Claim holder and session row | Action |
| --- | --- |
| Holder's node lease is committed as expired | Take the claim by the owner-stale swap, record the owner as lost, resume |
| Row is `Detached` | Take the claim by the identity-bound resume swap, resume. The holder is not asked: its snapshot is stored and no connection owns the session |
| Row has an owner committed under a claim that is no longer on file | That owner is fenced. Take the claim by the resume swap, record the owner as lost, resume |
| Row has an owner under the claim on file | Ask the holder to detach the owning connection (`resume_steal.v1`). It stores the resource snapshot and admits the frames it recorded without a transport; then the row is `Detached` |

The swap is bound to the claim epoch observed first and is issued once. A
swap that loses means another resume or the orphan reaper moved the claim;
the resume is refused. The claim is recorded in the registry's fence
inventory and the session is not hydrated into the in-memory registry.

The holder is asked inside the resume-handshake bound
(`WADDLE_CLUSTERING_RESUME_HANDSHAKE_TIMEOUT_MS`). A holder that does not
answer has its node lease committed as expired only if its heartbeat lapsed
past the lease TTL, which is the stale-node watchdog's own transition.
Otherwise the claim is not taken: **a live holder that does not answer, or
that answers without the row becoming `Detached`, keeps the session, and the
resume is refused** with `<failed/>` (`resource-constraint`). Nothing was
written, the client binds afresh, and the session stays resumable on its
holder until its window ends. Taking the claim there would be safe for
committed frames, which the fence protects, but would drop the frames the
holder's connection recorded without a transport and resume without its
resource snapshot.

A session whose owner was recorded as lost has no resource snapshot: the
owner never detached. The occupancy generation the resource was bound under
is stored on `sm_durable_sessions.occupancy_session` (migration V1036) when
the session is enabled and again at every detach, so the resuming
connection publishes the resource under it and the resume succeeds whichever
node recorded the loss: a surviving node, the orphan reaper, or the same
node restarted on the same database. The frames the client had not handled
are replayed once, in order, ahead of live output. The presence, carbons and
roster and blocklist interest the lost owner held are not knowable, so the
resumed resource starts as a freshly bound one does: unavailable, carbons
off, no roster or blocklist push interest, and the once-per-session
subscription flush not yet consumed. The client re-establishes them as it
would after a fresh bind, by sending initial presence and enabling what it
uses. A session enabled before V1036 and never detached since has no stored
generation; its resume is compensated to `Detached` and answered
`<failed/>` (`item-not-found`), the client binds afresh, and its retained
frames reach offline delivery when it expires.

A detach or a resume on the holder leaves the claim epoch unchanged, so a
session resumed on the holder between this node's last read of the row and
its swap is taken from a live owner. That owner is superseded like any
other: its next write fails.

A write refused because the claim row is gone (`ClaimFenceMissing`) retires
the binding that named it. A claim never returns to a binding that lost it,
so the binding reads the session: one that continues is left to the node
holding its claim, as a superseded generation is; one that is terminal is
treated as ended. Only an unreadable store keeps the binding waiting.

## Delivery to a detached session

A `Detached` session inside its resume window is a recipient, with the
rules a detached legacy session has (`S/sm_authority/detached.rs`):

- **Selection.** The stream-management session registry's detached
  readers (`detached_resources_for_user`, the carbon, presence, roster and
  block-list variants, `detached_presence_state`) list the account's
  `Detached` rows whose window has not elapsed, with the flags and presence
  of the stored resource snapshot. A session whose owner was lost before
  it detached has no snapshot and is listed as unavailable, without
  carbons or interest. Every routing path that reads those registry calls,
  including the RFC 6121 recipient inventory, the relay receiver and the
  direct-lane worker, sees these sessions as it sees a legacy detached
  session: a full JID that names one selects it, and a bare-JID message
  selects the live resources of the highest non-negative priority and
  every detached session.
- **Admission.** The registry's detached appenders admit the stanza on
  `AdmissionAuthority::Detached` at the generation read with the row. The
  frame is retained without raising the exposure ceiling, so a resume
  replays it in ordinal order ahead of anything admitted later. A copy an
  ingress transaction owns is admitted under an operation id derived from
  the session and the copy's ingress key, so a repeated offer while the
  frame is retained returns the same ordinal; the frame carries the copy's
  ingress receipt and provenance.
- **Claim.** On a clustered store the admission asserts the session claim.
  The admitting node is the account's owner, which need not hold it: it
  uses the claim it holds, takes a free one, or moves one another node holds
  by the swap a resume uses for a `Detached` row (`OwnerReleased`, or
  `OwnerLeaseExpired` for a holder whose lease lapsed). The holder is not
  asked; the row has no connection. A resume on any node later moves the
  claim back by the same rule, and the expiry sweep runs on whichever node
  holds it.
- **Refusal.** A known refusal (the session resumed, ended, is full, or its
  claim could not be held) retains nothing and is reported as no session,
  so the caller takes its offline path. An admission whose commit outcome
  stays unknown after three offers is reported as a storage failure, and
  the caller does not deliver the stanza another way.
- **Expiry.** An expired session's retained frames, these included, are
  transferred to offline delivery by the janitor once and its custody is
  released (`settle_terminal_session`).

Limits: a frame that arrives while the session is `TransferHydrating` is
refused and takes the offline path; it is not replayed by that resume. A
claim moved here right after the holder detached refuses the holder's
admission of the frames its endpoint accepted after the detach
(`admit_detached_tail`); those frames are in no custody and only the
ingress-owned ones are redelivered from their canonical rows. Each detached
read is a database read per routed stanza.

## Replacement on a clustered store

Binding a resource again ends its durable sessions with `Replaced`
(`W/cleanup/durable_sm.rs`, `replace_durable_resource_sessions`). On a
clustered store each session is ended under its own claim:

1. The binder reads the resource's non-terminal sessions.
2. For each, it takes the session claim with the resume's claim rules
   (`claim_for_replacement` in `W/stream_management/durable/claim.rs`). The
   binder's authenticated bare JID is the identity proof, and it must equal
   the session's account. A holder whose connection owns the session is
   asked to detach it (`resume_steal.v1`); that connection is closed with a
   conflict and stores its resource snapshot and the frames it recorded
   without a transport. A lost owner is recorded as lost.
3. It commits `Replace` under that claim (`DurableSmStore::replace_session`),
   which asserts the claim row in the same transaction.
4. It settles the terminal session: its retained frames go to offline
   delivery once each, and custody and the claim are released.

The wait for holders is bounded per bind (five seconds, and never more than
the resume-handshake bound) so the bind keeps the rest of its retirement
budget. A session whose holder does not release it in time, or whose claim
another taker moved first, is left as it is: the occupancy authority retires
its generation at this bind, a later resume of it is refused, and it expires
through its own window, transferring its frames then.

Bind 2 binds a server-generated resource, so it never replaces an existing
session's resource.

## Conflict close

The witness names the superseded binding. The caller may close that
transport. Whether or not the close arrives, the superseded owner cannot
admit, acknowledge, detach, complete or compensate: each of those names its
old generation and binding and fails the fence.

## Sessions without a durable record

The durable authority is the only stream-management authority.
`WADDLE_XMPP_DURABLE_SM` may be unset or `durable`; any other value,
`legacy` included, stops startup, so a deployment that names another
authority does not serve with this one by surprise.

A session the in-memory registry enabled before that has rows in
`sm_sessions` or `sm_unacked` and none in `sm_durable_sessions`. Nothing
serving resumes, expires or releases it, and it is not converted into a
durable session: its stored counters and resource snapshot are not a
durable window, and its `previd` is its storage key, which a durable
session never is. It is drained once instead
(`S/sm_authority/legacy_drain.rs`):

- **Where.** The owner phase drains before a restricted runtime attaches
  (`prepare_restricted_runtime_owner`, after the durable relations are
  prepared). A startup that runs its own DDL (SQLite, or PostgreSQL without
  a restricted runtime role) drains when `DurableSmSelection::open` opens
  the store, before the in-memory registry restores anything.
- **One transaction per session**, opened through
  `begin_guarded_store_writer`. It settles the session's recorded
  acknowledgement windows (the offline rows the client acknowledged are
  deleted), releases every offline row the session still claims, and
  stores each retained message in `pending_delivery` that is neither the
  replay copy of a claimed row (same outbound sequence) nor already
  transferred (a `pending_delivery_sm_promotions` receipt). The message is
  classified as an offline-storage candidate with no resource online and
  the recipient's blocklist; it is inserted without the per-recipient quota,
  because the session had already accepted it. The ingress custody of each
  processed frame is marked promoted, and the session's `sm_sessions`,
  `sm_unacked`, `sm_acknowledged_windows` and promotion receipt rows are
  deleted.
- **Exactly once.** The transaction commits all of it or none of it, and a
  drained session leaves no row to find, so a rerun changes nothing. Custody
  of a frame the session no longer retained stays pending for the
  runtime's custody recovery, which promotes it once.
- **What is lost.** One resume per client at the cutover: the client gets
  `<failed/>` and binds afresh, and the retained messages arrive through
  offline delivery. Retained `<iq/>` and `<presence/>` frames, and messages
  offline storage does not keep, are dropped, as at the expiry of an
  unresumed session with no other resource online. A retraction the old
  process recorded only in memory does not scrub a retained message.

The drain reads `sm_sessions`, `sm_unacked`, `sm_acknowledged_windows`,
`pending_delivery_sm_promotions` and `sm_ingress_appends`; the durable
authority keeps using `sm_sessions` and `sm_unacked` for its own sessions.

## Schema and the epoch guard

On PostgreSQL, migration V1034 creates `sm_durable_sessions` and
`sm_durable_operations` with the ingress epoch guard triggers and enrolls
them in `ingress_epoch_guard_manifest`. The owner initializer
(`S/sm_authority/schema.rs`) issues the same `CREATE ... IF NOT EXISTS`
statements for SQLite and for a database without the migration catalog. A
later column is a new migration or a repeatable statement in the
initializer.

Every write transaction begins in `DurableSmStore::begin_write`, which calls
`begin_guarded_store_writer`: the epoch row lock and the transaction-local
proof precede the claim and session row locks, and a live epoch the binary
does not implement fails the write as `DurableSmError::EpochUnproven` before
any statement. Reads open a plain transaction. The runtime attachment
refuses either table unless it carries exactly its guard pair.

## Not in this model

- Retention interlock between a retained frame and its canonical
  `ingress_messages` parent. The frame's full payload is in `sm_unacked`, so
  replay does not read the parent; the cross-writer route and settlement
  interlock is a separate #2046 item.
- Transfer of a terminal session's frames to offline delivery. The store
  retains them and exposes release; the pending-delivery promotion writer is
  not converted here.
- A single transaction that replaces every session of a resource on a
  clustered store. `replace_resource_sessions` refuses there, because each
  session's write asserts that session's own claim; the binder replaces the
  sessions one at a time instead (see "Replacement on a clustered store").
