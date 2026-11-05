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
stalled; this remains a blocking activation gate. Out-of-band stop and lease
signals seal admission during that interval, but a stuck handler still needs
an explicit process shutdown deadline and forced-abort policy. Forced abort
joins the worker and leaves the active input as `CompletionUnknown`; queued
inputs remain `NotStarted`. This package does not claim process-crash recovery.

The data command accepts already-parsed `ParsedFrame` values and a typed
`jid::BareJid`. Its producer must parse at the I/O boundary and pass the
original wire byte length, which is checked against `MAX_FRAME_SI