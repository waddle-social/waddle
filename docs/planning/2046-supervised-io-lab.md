# P3.1 supervised I/O laboratory

This dark, test-only slice characterizes transport ownership and bounded work
before any connection-actor or endpoint cutover. It does not alter production
transport wiring, XMPP protocol behavior, or ownership of `WsConnState`.

## Plan

- Model retained reader and writer tasks around owned WebSocket halves and join
  both results on retirement; inspect close or drop from the peer.
- Exercise count and byte limits, explicit overload handling, write-attempt
  milestones, and cancellation, panic, shutdown, and force-detach schedules.
- Characterize failed resumption followed by a fresh bind on the same physical
  WebSocket separately from retirement of that socket.
- Probe a bounded Kameo control scheduling design if it can be isolated from
  production authority, and record its exact limitations.
- Run focused tests, formatting, strict Clippy, and the required inventory.

No actor extraction, endpoint DDL, or activation is authorized before the
DEC-3 and P2 gates. A detached binding is cleanup evidence, not proof that the
physical WebSocket has terminated.
