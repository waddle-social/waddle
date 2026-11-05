# P3.2 actor-owned driver task and retained completion custody

Status: scoped plan on branch `codex/2137-owner-task-custody`, based on hosted
main `cddd3870c786a20fd16fab2be3cf9c54c6517713`.

## Objective

Move the dark `ConnectionActorCore` frame and peer-recipient handlers into a
single actor-owned task. The task is the only mutable owner of
`WsConnState`. A caller may cancel its wait without cancelling accepted work
or losing its eventual typed result.

## Bounded design

- Admit at most 256 unresolved data operations. Each accepted operation keeps
  its count and byte permits through queued, running, and retained-completion
  states until explicit acknowledgement. No completion eviction is allowed;
  exhausted count or byte capacity returns a typed overload result.
- Use a separate small bounded control channel for completion lookup and
  acknowledgement. Stop and lease-loss signals remain out-of-band, seal data
  admission immediately, and remain responsive while a handler is stalled.
- Carry a monotonic operation ID and exact `ConnectionRef` through admission,
  completion lookup, acknowledgement, and task cleanup. Repeating an ID finds
  its existing operation and never runs the handler twice.
- Keep the typed input and completion in an independently retained custody
  record. A late stop or authority loss after handler return retains its result
  as `CompletionUnknown`; a forced worker abort retains the source input and
  records that no result was observed. Queued work that never started is
  represented separately.
- Attach worker join to the existing exact-connection
  `ActorLocalEndpointOwner`/IoCustodian cleanup lifetime. Its direct-stanza
  queue remains separate because it cannot represent `ResponseBatch` or
  `DriveOutcome` custody.

## Boundaries and activation gates

Keep `/ws` unselected, do not publish bindings, write frames, advance stream
management, or change XMPP wire behavior. Do not edit workflow files or enable
GitHub Actions.

The worker services queued control requests between handlers. Control that
requires worker-owned state cannot progress while an effectful handler is
stalled; this remains a blocking activation gate. Out-of-band stop and lease
signals must remain responsive during that interval, and forced shutdown must
join the worker after retaining typed unknown custody. This package does not
claim process-crash recovery.

## Verification plan

- Deterministically cancel a caller wait while a handler is paused; retrieve
  the same completion by operation ID and prove the handler ran once.
- Fill the data mailbox and retained ledger to the 256-operation limit; prove a
  further operation returns typed overload while the separate control path and
  out-of-band stop remain available.
- Revoke authority during a handler and force worker shutdown during another;
  verify returned, unknown, and never-started custody remains distinguishable
  and cleanup joins the exact connection task.
- Run focused actor-owner tests, default and clustering server checks, format,
  and scoped Clippy with warnings denied.
