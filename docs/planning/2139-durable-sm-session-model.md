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
  epoch)` under `FOR SHARE`, before the session row lock. A node that lost its
  lease has no such row, so it loses write authority with the lease.
  Single-node SQLite has no claim.
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

## Conflict close

The witness names the superseded binding. The caller may close that
transport. Whether or not the close arrives, the superseded owner cannot
admit, acknowledge, detach, complete or compensate: each of those names its
old generation and binding and fails the fence.

## Not in this model

- Retention interlock between a retained frame and its canonical
  `ingress_messages` parent. The frame's full payload is in `sm_unacked`, so
  replay does not read the parent; the cross-writer route and settlement
  interlock is a separate #2046 item.
- Transfer of a terminal session's frames to offline delivery. The store
  retains them and exposes release; the pending-delivery promotion writer is
  not converted here.
- Replacing a resource's sessions when the resource is bound again on a
  clustered store. The binder holds the account claim, not each session's
  claim, and no fence joins the two; the store refuses the operation there.
