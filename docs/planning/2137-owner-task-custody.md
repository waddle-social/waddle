# P3.1 actor-owned driver task and retained completion custody

Status: dark driver and endpoint cleanup integration implemented on branch
`codex/2137-lifecycle-custody` in draft PR #2211, based on hosted main
`ff88ae7ca5b9077d1d0d0bf0b1aa47f77cf61823`. This does not activate the
driver in `/ws`.

## Objective

Move the dark `ConnectionActorCore` frame and peer-recipient handlers into a
single actor-owned task. The task is the only mutable owner of
`WsConnState`. A caller may cancel its wait without cancelling accepted work
or losing its eventual typed result.

## Bounded design

- Admit at most 256 unresolved data operations, with a separate bounded byte
  semaphore and control queue. Accepted operations retain count and byte
  reservations through queued, running, and completion states. The byte charge
  is an admission estimate: typed input debug size (or captured original
  `wire_bytes`) plus a 64 KiB result reservation. It is not a hard memory
  bound. The task measures the returned typed result after the handler; when
  it exceeds its reservation, it retains the result as `CompletionUnknown`,
  seals new admission, and stops the worker. `ResultReservationExceeded`
  takes precedence over `AuthorityRevoked` when both apply to the same
  returned result. Establishing a hard output bound or durable oversized-result
  custody remains a blocking activation gate.
- Use a separate small bounded control queue. Its barrier marker proves
  admission and service after a stalled handler returns; controls that require
  worker-owned state do not progress during the handler. Exact-ID lookup and
  acknowledgement are synchronous operations on the exact-ref ledger, not
  mailbox control messages. Out-of-band stop and lease-loss watchers seal
  admission immediately but do not interrupt an effectful handler.
- Carry a monotonic operation ID and exact `ConnectionRef` through admission,
  completion lookup, acknowledgement, and task cleanup. The task handle can
  reacquire an unresolved operation by exact reference and ID. Once attached,
  the app-owned endpoint record resolves lookup and acknowledgement after the
  caller handle is dropped; dropping that attached handle does not stop work.
  A standalone, unattached handle still requests stop when dropped.
- Keep the typed input and completion in an independently retained custody
  record. A late stop or authority loss after handler return retains its result
  as `CompletionUnknown`; a forced worker abort retains the source input and
  records that no result was observed. Queued work that never started is
  represented separately.
- The detached supervisor retains and joins this task's worker handle. The
  existing app-owned `ActorEndpointCleanupIndex` retains the driver cleanup
  handle beside the physical IoCustodian under one exact-`ConnectionRef`
  record; this composes with the existing record instead of adding a second
  index. Attachment and endpoint shutdown share one record lock, so a late
  attachment is rejected. The record is not joined or pruned until both the
  worker supervisor and physical pumps join and retained work is settled. The
  direct-stanza queue remains separate because it cannot represent
  `ResponseBatch` or `DriveOutcome` custody.

## Boundaries and activation gates

Keep `/ws` unselected, do not publish bindings, write frames, advance stream
management, or change XMPP wire behavior. Do not edit workflow files or enable
GitHub Actions.

The worker services queued control requests between handlers. Control that
requires worker-owned state cannot progress while an effectful handler is
stalled; this remains a blocking activation gate. The owner applies a bounded
shutdown grace and forced-abort policy when stop is observed. Forced abort joins
the worker and leaves the active input as `CompletionUnknown`; queued inputs
remain `NotStarted`. This package does not claim process-crash recovery.

The data command accepts already-parsed `ParsedFrame` values and a typed
`jid::BareJid`. Its producer must parse at the I/O boundary and pass the
original wire byte length, which is checked against `MAX_FRAME_SIZE`; the
actor does not accept raw XML strings. This API is dark and unselected, so the
size check does not prove a future `/ws` adapter passes the measured wire size.
The typed command currently covers RFC 7395/SASL/classic stanzas and peer
recipient-pass work. XEP-0198 nonzas and malformed-frame response mapping still
need typed commands before the driver can cover the live `/ws` input surface.

## Endpoint cleanup integration tranche

Implemented in this PR:

The existing app-owned `ActorEndpointCleanupIndex` in
`actor_local_endpoint.rs` remains the lifecycle authority for a physical
connection. It owns the driver task's operation ledger and retains the driver's
independent supervisor/join on that same record. No second per-connection
index is added. Exact-reference lookup and acknowledgement survive caller and
task-handle drops; stale references cannot reach the record.

Stop and lease-loss seal admission immediately. The attached permit retains
the exact identity source revision captured by `SocketAdmission`, in addition
to the serving generation. Admission and completion both revalidate that
authority; a result within its reservation that returns after source-revision
loss is retained as `CompletionUnknown(AuthorityRevoked)` even if node
lifecycle still says `Serving`. An oversized returned result instead uses the
`ResultReservationExceeded` classification described above. The custodian
synchronously notifies its owner before either a pump
stop or graceful-close transition becomes visible. That notification seals
driver admission and completion on the shared ledger gate. After dequeue, the
worker must claim `Accepted` to `Running` under that same gate before entering
the handler; if stop wins first, the operation remains `NotStarted`. The core
lifecycle stop is recorded before the gate is released. The driver's timer
uses the first stop `Instant` captured under that gate, so asynchronous timer
scheduling cannot extend grace; it covers pump stop, graceful close without a
peer reply, and identity/lease revocation without waiting for pump retirement.
At the configured deadline, abort and join the worker, retaining the active
input as `CompletionUnknown` and queued inputs as `NotStarted`. Keep the cleanup
record until both driver and physical-pump cleanup have joined and retained
work is settled. Attachment and shutdown are serialized by the same
cleanup-record lifecycle lock. Direct ticket acknowledgement runs a weak,
exact-reference settlement callback after releasing the ledger lock, so it can
prune a record when the join task's earlier prune check saw unsettled work. The
callback owns neither the index nor the endpoint record. Preserve exact
operation identity and never replay from caller cancellation.

The new deterministic tests cover handle-independent lookup/ack, one handler
execution, stale references, duplicate and late attachment rejection, direct
ticket acknowledgement after the join task's prune check, stop versus submit
and result publication gates, and stop versus the dequeue/start claim,
including proof that a dequeued operation does not enter its handler when stop
wins. They also cover source-revision loss without node lifecycle demotion,
pump-triggered stop while another pump is held, graceful close with no peer
reply, identity-only deadline abort, concurrent join callers, and control
admission behind a stalled handler. Existing owner and pump tests
continue to cover bounded inventory, lease loss, panic, stalled physical
cleanup, and successor fencing.
Keep `/ws` selection, endpoint publication, stream-management advancement,
wire behavior, workflow files, P2 custody, and the remaining XEP-0198/malformed
frame paths out of this tranche.

Remaining activation gates include a hard output-memory bound, typed handling
for XEP-0198 and malformed input, control progress while a handler is stalled,
P2 effect custody, P3.2/P3.3, and explicit activation review. No route selector,
publication path, stream-management advancement, or wire behavior changed.

## Verification plan

- Deterministically cancel a caller wait while a handler is paused; retrieve
  the same completion by operation ID and prove the handler ran once.
- Fill the data mailbox and retained ledger to the 256-operation limit; prove a
  further operation returns typed overload while the separate control path
  remains available. Separately revoke authority during a stalled operation
  and prove out-of-band admission sealing.
- Revoke authority during a handler and force worker shutdown during another;
  verify returned, unknown, and never-started custody remains distinguishable.
  Verify pump stop, graceful close, and identity-only revocation start one
  deadline before physical I/O cleanup completes. Verify endpoint cleanup
  retains and joins the worker supervisor together with physical I/O cleanup
  before pruning.
- Run focused actor-owner tests, default and clustering server checks, format,
  and scoped Clippy with warnings denied.
