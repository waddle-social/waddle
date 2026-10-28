# Detached roster push delivery (#2039)

The original three warnings cannot establish their failure mode or delivery
outcome: the old logs and traces lack that evidence. They do identify the
subscription fanout path. This change repairs a reproducible gap in that path.

## Reproduced gap and repair

An interested resource can have a full forwarding mirror on its user-owner node
while its real socket on another node still accepts frames. Subscription fanout
previously retried only the full mirror and a stale detached source. Roster-set
fanout already used explicit registered remote delivery. All three roster-IQ
fanout callers now share that route, local queuing, detached replay, interest
checks, source exclusion, deduplication, and correlation IDs.

Remote backpressure is not acceptance. An uncertain remote reply ends this pass
without another local or SM allocation that could duplicate the push. A fresh
same-JID replacement must be roster-interested before receiving a fallback.
These rules preserve [RFC 6121 roster interest](https://www.rfc-editor.org/rfc/rfc6121.html#section-2.2),
XEP-0237 versioning, and XEP-0198 ownership fencing.

## Operational evidence

- `Failed to record detached roster push` has one shared schema and emits
  immediately, with resource, push ID, version, error, and `pending_fallback`.
- `Detached SM append failed after fence recovery was unavailable` includes the
  specific bounded recovery `reason`, stream ID, and backend error. Reasons
  distinguish failed/timed-out claim reads, foreign ownership, identity changes,
  missing/failed source reads, pending reservations, and recovery failure.
  Successful portable unfenced writes do not emit this warning.
- `Detached roster push fallback outcome` and remote outcome events distinguish
  local/remote queue acceptance (INFO), backpressure, uncertain remote acceptance,
  and unresolved delivery (WARN). A missing final event leaves outcome unknown.
- `xmpp.sm.detached.append.rejected` counts actual fenced append errors by
  `reason=missing_fence|not_owner|other`, with startup zero samples. `other` may
  include an uncertain commit. Recovery attempts alone do not increment it.

Queue acceptance is not client receipt. Historical delivery remains unknown;
no production changes were made. The code gap is established by native tests
that route an actual roster frame into the remote socket handler, not by assuming
that every prior warning represented a dropped push.
