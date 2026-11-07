# Exact local binding for ordinary outbound writes

Status: implementation plan. This does not select the replacement connection actor, durable SM custody, or restricted database serving.

The active `/ws` writer checks node admission at `start_send`, but only the resumable `<enabled/>` batch frame carries the exact local binding permit. A replaced same-resource socket can therefore start an ordinary frame after its registry binding has been revoked.

## Scope

1. Capture the current exact local binding for post-registration ordinary single-frame and response-batch starts. Pre-bind, failed-resume, and closing control traffic retain their existing path. A socket that was published but loses its binding fails closed.
2. Hold node admission, resume control where required, local binding, then physical interruption only across synchronous `start_send`. Do not hold these locks during readiness, flush, database work, or registry lookup.
3. Distinguish binding revocation from transport failure. A frame counted only in the old process's SM memory cannot settle a producer or receipt on revocation; retain the exact unstarted frame and batch tail for the existing recovery path. Replay-suppressed frames must not be counted twice.
4. Qualify same-resource replacement before `start_send`, a revocation while `poll_ready` waits, and a replacement after an authorized start. Cover direct, peer, and batch frames, SM and non-SM paths, pre-bind and failed-resume control, producer acceptance, and replay accounting.

The permit proves only local transport-start authority. It does not prove byte completion, durable replay ownership, remote endpoint publication, cross-writer settlement, or peer receipt. The broader #2046 gates and disabled workflows remain unchanged.
