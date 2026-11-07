# WebSocket owner panic containment (#2046)

An unexpected panic can drop a live WebSocket owner's connection state before
its SM replay and cleanup obligations settle. This change contains that
incident; it does not reconstruct the lost state or prove a successful drain.

## Plan

1. Keep a clone of the connection drain guard outside the owner future. On an
   observed owner panic, latch a typed critical node failure and request process
   stop before releasing that guard. A normal return releases it normally.
2. Require a positively completed process-stop connection/SM drain and zero
   active connection guards before the normal clustering shutdown path retires
   its identity. Check the fatal fence after the guard observation; otherwise
   leave the identity to lease expiry. Do not infer recovery from a task exit.
3. Exercise real WebSocket owner panic and normal return, plus deterministic
   panic-versus-drain and lease-retirement schedules. Verify admission and
   existing permits fail closed, and preserve normal fully drained retirement.
   Run formatting, focused tests, and strict scoped Clippy, then obtain an
   independent review of the integrated change.

Panic-abort, task cancellation, process loss, unresolved in-flight writes,
SM replay custody, force-detach completion, and durable recovery remain outside
this containment boundary. Restricted startup and production P3 selectors stay
unchanged. GitHub Actions and workflow files remain disabled until #2046 is
complete.
