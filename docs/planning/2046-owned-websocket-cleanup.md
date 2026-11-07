# Owned WebSocket cleanup (#2046)

## Problem

The HTTP upgrade callback currently returns `LiveWebSocketConnection::run()` as
its own future. Dropping that callback waiter can drop the active connection
while an inbound frame, relay completion, force-detach request, or SM cleanup
is in progress. The connection guard and capacity hold then disappear without
proving the connection's detach and acknowledgement outcomes.

## Implementation plan

1. Start one independently owned connection task synchronously in the upgrade
   callback. Transfer the entire socket, admission, drain guard, channels, and
   connection state before returning an observational waiter. Bound owner count
   with the existing admission reservation and retain exact task observation.
2. Make caller-waiter loss request a soft stop at a connection-loop boundary.
   Finish the selected operation and run the existing ordered
   `finish_connection` path; do not cancel an in-flight handler or cleanup
   future. Keep process shutdown and admission revocation separate.
3. Test waiter loss before first poll and at representative registration,
   resume, relay, force-detach, and cleanup barriers. Verify retained capacity
   and guard, exact binding safety, SM replay order, and honest force-detach
   outcomes. Run formatting, focused tests, and strict scoped Clippy, followed
   by independent adversarial review.

## Boundary

This protects against cancellation of the upgrade callback waiter. A panic in
the owner task can still drop in-flight values; task completion alone does not
prove successful cleanup. Process loss, durable pre-accept SM custody, and the
local unregister mailbox-admission guarantee remain separate #2046 gates.
GitHub Actions and workflow files remain disabled until final qualification.
