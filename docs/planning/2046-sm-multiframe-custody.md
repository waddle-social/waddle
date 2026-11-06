# Dark ordered multi-frame SM custody for #2046

## Scope

Extend the unselected PostgreSQL custody repository and source handoff from one frame to a bounded ordered sequence in one captured SM generation. Keep the schema unregistered and the implementation behind `test-support` and `clustering`; no WebSocket, relay, batch, detached, GC or resume selector may use it. Bind commit order defines the immutable ordinal order. A second session generation remains rejected.

## Coherent implementation group

1. Admit distinct immutable operations under the exact gate and global quota; retry by operation ID with full identity comparison and reject a second ID for the same source obligation/attempt. Bind allocates the next nonwrapping ordinal atomically under the gate lock. Preserve unique operation, attempt, source and `(gate, ordinal)` keys, plus an exact slot-to-operation/gate relationship.
2. Make source handoff and candidate memory hydration ordered and bounded by rows and serialized bytes. Out-of-order committed handoffs may wait in memory but cannot skip a missing prefix for exposure. Roll back only newly inserted provisional entries after failure or cancellation. Per-receiver idempotence is not global endpoint ownership.
3. Expose only the next ordinal. Implement gate-level cumulative XEP-0198 ACK by wrapping `h` against a durable exposed window smaller than `2^32`; validate the entire contiguous prefix before one atomic frontier/terminal update. A repeated current-frontier ACK is idempotent. ACK does not seal the gate.
4. Seal the gate on abandonment/transfer, then transfer pending operations with full payload, original time and resource into the candidate sink. Preserve gaps and ordinals; a partial transfer remains discoverable. Provide an ordered, bounded replay inventory that reuses retained operations and slots without minting new count or send attempts. No live replay send permit is created.
5. Keep immutable journals charged against the retained admission quota: at most 1024 rows/16 MiB of admission payload plus at most that much copied transfer payload. Lock distinct canonical parents in deterministic order under the reviewed epoch → principal/claim → gate → canonical → attempt → quota order. Preserve late raw candidate writer and GC fences, exact positive unknown-commit reconciliation, and successor claim recovery.

The candidate receiver holds up to 1024 entries and 16 MiB of serialized
payload. It may hydrate later ordinals first, but exposure requires the exact
hydrated slot and advances only the durable next ordinal. A failed or cancelled
hydration removes only its own new entry. This is per receiver, not proof of a
unique live endpoint. The acknowledged frontier is advanced only after the
whole wrapping handled-count prefix is validated and its distinct canonical
parents have been locked in sorted order. An ACK leaves the gate open; the
first transfer seals it atomically and leaves remaining custody discoverable.

Sealed inventory lists bound operations by ordinal followed by unbound ones,
with at most 16 returned rows per page. Replay inventory includes pending bound
operations in ordinal order, preserving gaps and the original operation,
payload, timestamp, and resource. Neither inventory allocates a send attempt or
grants permission to replay on a transport. Immutable admissions stay charged
even after terminal settlement; an exact-copy constraint bounds all transfer
payloads by the retained admission total.

## Verification and later gates

Use required PostgreSQL 17 races for concurrent bind, identity conflict, quota/multibyte boundaries, three-slot out-of-order hydration/exposure, partial/repeated/wrapping/excess ACK, gaps, transfer conflict, cancellation/unknown COMMIT, two gates with opposite canonical-key order, late inserts versus collection, sealed multi-page recovery and successor turnover. Check XEP-0198 handled-count behavior, formatting, scoped Clippy and independent final review.

This dark proof does not authorize multiple physical receivers, production role/epoch migration, existing live/detached/batch writer conversion, actual pending sink/tombstone, real endpoint/pump replay ownership, production GC or activation. No quota is reclaimed while immutable journals remain.
