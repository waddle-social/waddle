# Dark source-to-SM pre-accept handoff for #2046

## Scope

Build one unselected, typed handoff for a prepared local direct frame. Carry a known committed full-payload custody operation and exact immutable SM slot from source delivery into a candidate memory admission. Source completion must wait for durable payload custody; count or queue admission must wait for the committed slot. Keep the original operation, source receipt time, resource, session generation and binding identity through cancellation or authority loss. Reject unsupported source classes, a second frame, and a replacement generation before effects.

This slice may use the #2218 test-support and clustering repository and unregistered PostgreSQL schema. Do not select it for live WebSocket, relay, batch, detached, GC or resume paths. The existing production routing and wire behavior remain unchanged.

## Implementation plan

1. Add a private, non-forgeable capability for committed payload admission and bound slot. Make the candidate source-completion path consume positive evidence, never enqueue success alone.
2. Add a deliberately unselected adapter at the source/recipient boundary. Use exact capture and current authority checks before hydrating the slot into an owned memory queue; preserve a durable recovery identity if the in-memory handoff fails after commit.
3. Prove with PostgreSQL 17 barriers that source completion and count/enqueue cannot precede commit; exercise lost reply, origin death, successor claim, stale binding, repeated handoff and unsupported source classes. Retain the #2218 one-frame/one-generation constraints.
4. Run focused PostgreSQL, owner and SM tests, formatting and scoped Clippy. Obtain independent read-only final review before marking the draft ready.

## Production gates

A real selector needs one acceptance boundary across live, relay, batch, detached and terminal writers, with multiple-frame allocation, replay and ordered exposure. It also needs authoritative ACK/transfer/tombstone settlement, pending sink adoption, canonical retention and GC, role/epoch migration, unknown-commit reconciliation, old-writer drain, P3 session authority and XEP-0198 qualification. The candidate cannot be described as production custody while any of these are open.
