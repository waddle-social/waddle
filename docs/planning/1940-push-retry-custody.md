# Bounded push retries and failure telemetry

Implements #1940 and #1846 together because retry disposition and failure
classification must agree across the notification outbox and provider queue.

## Plan

1. Review provider response contracts and the existing custody/claim fences.
2. Classify failures before send, explicit refusals, and unknown sends without
   erasing earlier uncertainty.
3. Bound automatic scheduling. Exhausted unresolved work is parked and retains
   its acceptance, candidate lineage, and canonical evidence. An attempt budget
   is not a custody expiry.
4. Preserve the per-node admission bound with explicit backpressure and safe
   pruning only; never evict accepted unresolved work to admit new work.
5. Use authoritative registration revocation for explicit retirement, including
   parked work, and preserve the normal retention tail after settlement.
6. Export allowlisted failure, capacity, and disposition signals and log raw
   diagnostics only in structured events.

## Verification

Test at the existing store/drain and provider dispatch boundaries, plus the
in-memory exported telemetry reader. Cover fresh/prior-uncertain 5xx,
throttling, pre-send errors, lost responses, budget exhaustion, saturated
admission, restart, stale claims, revocation, and custody/GC retention. Run
formatting, Rust typechecking, targeted tests during implementation, the server
workspace suite and doctests at completion, Clippy with warnings denied, and an
independent final code review.

## Provider contract references

- [RFC 8030](https://www.rfc-editor.org/rfc/rfc8030.html), sections 5 and 7.1.
- [APNs notification responses](https://developer.apple.com/documentation/usernotifications/handling-notification-responses-from-apns).
- [FCM error codes](https://firebase.google.com/docs/cloud-messaging/error-codes).

The implementation must document which specific responses prove refusal;
unrecognized failures remain conservative rather than inferring certainty
from the HTTP status family.

## Adversarial review follow-up

1. Distinguish connection establishment failures (including connect timeouts)
   from requests that may have reached a provider. Carry typed certainty from
   provider outcomes to worker scheduling; preserve earlier uncertainty.
2. Replace the fixed provider retry delay with bounded exponential backoff based
   on each durable row's failure count, including repair and expired-claim paths.
3. Export current parked counts, maximum per-node occupancy, near-cap node count,
   and snapshot freshness through the existing create-at-use telemetry pattern.
   Keep identity-bearing node inventory out of metric labels; use structured logs
   and read-only inventory for exact nodes. Add capacity and parked-work alerts.
4. Correct lost-claim certainty and retirement event semantics. Preserve terminal
   history, and verify explicit retirement either commits fully or returns a
   retryable error without partial cancellation on lock contention.
5. Provide an operator-authorized XMPP retirement procedure and obtain a
   read-only production inventory of legacy rows before any merge/deployment.

Use the existing sender, queue/drain, exported telemetry, and authenticated XMPP
command seams. Verify refusal/connect/post-connect-timeout distinctions, sticky
uncertainty, persisted backoff, falling and zero gauges, alert boundaries,
retirement history, authorization, and rollback. Keep this a focused repair;
consolidate duplicated classification only where needed to enforce the typed
boundary and avoid broad file splitting.
