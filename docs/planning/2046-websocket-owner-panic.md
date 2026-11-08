# WebSocket owner panic containment (#2046)

An unexpected panic can drop a live WebSocket owner's connection state before
its SM replay and cleanup obligations settle. This change contains that
incident; it does not reconstruct the lost state or prove a successful drain.

## Containment

The owner retains an outside clone of its connection guard through its normal
return or, on panic, until a typed panic latch and fatal/stop signals have been
published and the lifecycle failure has been recorded. Admission, readiness,
existing permits, and final shutdown status can observe the panic latch without
waiting for that lifecycle writer. A start that passed its final authority
check before the signal remains admitted and can complete afterward.

Early clustering retirement now requires process stop, Q6 task exit, an
observed connection-guard drain, zero current guards, and a final fatal-fence
check. Q6 task exit is not proof of SM settlement. If these conditions are not
met, the identity is left to lease expiry; this can delay takeover after a
clustering-only stop or a timed-out drain.

The fatal and process-stop signals are sent before waiting for the synchronous
lifecycle write lock. A blocked runtime or an already-dispatched database
operation has no new completion guarantee from this change.

## Verification

- Native WebSocket admission suite: 15/15, including owner panic before guard
  release, waiter loss during shutdown, and normal cleanup. Loopback access was
  required outside the sandbox.
- Clustered shutdown deadline suite: 11/11, including completed and stalled
  drains, fatal fencing, both retirement identities, and the Q6 timing bound.
- Eleven targeted clustered library tests passed, including the blocked-send
  panic latch, permit revocation, shutdown status, and retirement matrix. A
  pre-existing test-only call was updated to the current helper name so the
  library test crate could compile. The build emitted existing test warnings.
- Strict `waddle-server` library Clippy passed with `-D warnings` in both
  default and `clustering` modes. Formatting and diff checks passed. The Rust
  structure checker reported 12 violations in untouched files.

The full workspace, hosted CI, deployment, and cross-node recovery were not
verified by this slice.

Panic-abort, task cancellation, process loss, unresolved in-flight writes,
SM replay custody, force-detach completion, and durable recovery remain outside
this containment boundary. Restricted startup and production P3 selectors stay
unchanged. GitHub Actions and workflow files remain disabled until #2046 is
complete.
