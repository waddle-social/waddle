# Exact pending-flush claim through local SM output

Status: implementation plan for #2046 and #2139. This branch is a production-path replacement-claim safety change, not the cross-writer route/settlement cutover or a serving selector.

## Failure

`flush_for_resource_inner` claims and offers a pending row with `(row_id, SM session, claim_token)`, but the local `OutboundStanza` keeps only `row_id`. Live output and both detach drains assign an SM sequence, then `record_pushed_at` updates by row ID alone. A delayed old writer can stamp a row that a janitor released and another session or token reclaimed. On storage error, those paths can delete or release the replacement by row ID. An `Ok(0)` result is currently ignored.

## Implemented boundary to prove

1. Carry the existing typed `PendingClaim` intact through the production local offer, outbound envelope, direct writer, both detach drains and terminal drain. Keep tokenless legacy/test callers explicit; never infer a token from a row ID or session. Account for this metadata in actor mailbox bounds. No relay wire change: remote forwarding already refuses pending-tagged envelopes.
2. Add a storage operation that binds one wrapping SM sequence only to the exact offered `(row_id, session, token)`. The atomic PostgreSQL/SQLite conditional update must accept an unbound row or the same sequence; a same-claim/same-sequence retry is idempotent. Return a typed applied/already-same versus stale/conflicting result. An adapter must not silently fall back to row-ID DML. Implement equivalent exact matching in the in-memory store.
3. Replace production row-ID stamp and fallback DELETE/RELEASE for token-bearing frames. A stale or uncertain stamp must not mutate the replacement claim or silently count as success. Retain the already-counted frame and exact offered row for the existing recovery lifecycle, and prevent immediate old-transport exposure when the stamp is not positively bound. Exact-owner release in terminal cleanup must include the token, including same-session replacement. Keep historical tokenless APIs only for their explicitly legacy path.
4. Do not claim a stop after post-count refusal makes the frame unexposable: replay, promotion and process death remain separate. Do not move the stamp before count without a durable retry-addressable output intent; an uncertain commit could create a phantom sequence. Unknown COMMIT cannot be treated as rollback from a fresh absent read. Leave pending acceptance, GC, arbitrary-node resume and restricted serving gated.

## Required verification

- PostgreSQL 17 concurrent old stamp versus release/reclaim for same row with new session and new token on the same session; both winner orders, idempotent retry, conflicting sequence and `Ok(0)` classification.
- Positive exact readback and unknown COMMIT/cancellation schedules, including ACK, promotion and row-consumption races. Prove no stale or unknown path deletes/releases/stamps a replacement claim.
- Actual local live output plus first/second and terminal detach drains; counted frame retention and no immediate transport attempt on refused stamp. SQLite and in-memory equivalents.
- Strict scoped Clippy with `-D warnings`, formatting, independent exact-head review. Report full-workspace/hosted CI and durable output-intent limits separately.

Old binaries retain unconditional row-ID writes and must be drained or fenced before this is treated as a mixed-version authority guarantee. GitHub Actions and workflow files remain disabled under #2046.
