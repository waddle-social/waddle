# P3.1 actor-owned driver task and retained completion custody

Status: implemented dark package on branch `codex/2137-owner-task-custody`, based on hosted
main `cddd3870c786a20fd16fab2be3cf9c54c6517713`.

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
  seals new admission, and stops the worker. Establishing a hard output bound
  or durable oversized-result custody remains a blocking activation gate.
- Use a separate small bounded control queue. Its barrier marker proves
  admission and service after a stalled handler returns; controls that require
  worker-owned state do not progress during the handler. Exact-ID lookup and
  acknowledgement are synchronous operations on the exact-ref ledger, not
  mailbox control messages. Out-of-band stop and lease-loss watchers seal
  admission immediately but do not interrupt an effectful handler.
- Carry a monotonic operation ID and exact `ConnectionRef` through admission,
  completion lookup, acknowledgement, and task cleanup. The task handle can
  reacquire an unresolved operation by exact reference and ID. Dropping a
  ticket alone does not lose custody; recovery after dropping the task handle
  is not established because this package adds no app-lifetime lookup index.
- Keep the typed input and completion in an independently retained custody
  record. A late stop or authority loss after handler return retains its result
  as `CompletionUnknown`; a forced worker abort retains the source input and
  records that no result was observed. Queued work that never started is
  represented separately.
- The detached supervisor retains and joins this task's worker handle, and all
  ledger entries carry the exact `ConnectionRef`. This package does not yet
  register that join with the existing `ActorLocalEndpointOwner`/IoCustodian
  cleanup inventory; connecting those lifetimes is required before activation.
  The direct-stanza queue remains separate because it cannot represent
  `ResponseBatch` or `DriveOutcome` custody.

## Boundaries and activation gates

Keep `/ws` unselected, do not publish bindings, write frames, advance stream
management, or change XMPP wire behavior. Do not edit workflow files or enable
GitHub Actions.

The worker services queued control requests between handlers. Control that
requires worker-owned state cannot progress while an effectful handler is
stalled; this remains a blocking activation gate. Out-of-band stop and lease
signals seal admission during that interval, but a stuck handler still needs
an explicit process shutdown deadline and forced-abort policy. Forced abort
joins the worker and leaves the active input as `CompletionUnknown`; queued
inputs remain `NotStarted`. This package does not claim process-crash recovery.

The data command accepts already-parsed `ParsedFrame` values and a typed
`jid::BareJid`. Its producer must parse at the I/O boundary and pass the
original wire byte length, which is checked against `MAX_FRAME_SIZE`; the
actor does not accept raw XML strings. This API is dark and unselected, so the
size check does not prove a future `/ws` adapter passes the measured wire size.
The typed command currently covers RFC 7395/SASL/classic stanzas and peer
recipient-pass work. XEP-0198 nonzas and malformed-frame response mapping still
need typed commands before the driver can cover the live `/ws` input surface.

## Verification plan

- Deterministically cancel a caller wait while a handler is paused; retrieve
  the same completion by operation ID and prove the handler ran once.
- Fill the data mailbox and retained ledger to the 256-operation limit; prove a
  further operation returns typed overload while the separate control path
  remains available. Separately revoke authority during a stalled operation
  and prove out-of-band admission sealing.
- Revoke authority during a handler and force worker shutdown during another;
  verify returned, unknown, and never-started custody remains distinguishable and
  the detached supervisor joins the worker. Registering that join with endpoint
  and I/O cleanup remains an activation gate.
- Run focused actor-owner tests, default and clustering server checks, format,
  and scoped Clippy with warnings denied.
