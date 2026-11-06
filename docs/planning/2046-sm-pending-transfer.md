# Dark source-marked SM to pending transfer for #2046

## Scope

Prove one co-located, unselected direct-message handoff from retained SM
custody to the existing `pending_delivery` store. Keep source custody per full
resource while accepting at most one offline copy per canonical direct route
and validated bare recipient. This does not select live writers, recovery
replay, tombstones or production GC.

## Coherent implementation group

1. Bind both candidate custody and the real pending store to the same attested
   PostgreSQL database, schema and lexical transaction. Never dual-write
   separate pools or infer success from a matching URL. Only a committed exact
   source-handoff marker is eligible; an admitted but unbound or unmarked source
   stays retained and discoverable. A reclaimed or stale source attempt cannot
   complete another operation's handoff.
2. Define an immutable recipient-wide acceptance identity from the canonical
   message, receipt kind, semantic identity and validated bare recipient. Keep
   each full-resource operation, gate, slot and attempt distinct. Persist an
   accepted-pending outcome with one stable, time-sortable physical row ID and
   an exact mapping from every transferred resource operation to that outcome.
   A generic effect receipt or `AlreadyCompleted` is not pending proof. Retain
   the outcome after the physical pending row is ACKed or deleted so retries
   cannot create a second row. Treat archived-row reuse only with its existing
   locked archive/dispatch evidence.
3. Under a reviewed claim → gate → sorted canonical → source-attempt → pending
   recipient quota → child order, verify the prepared direct route, full
   payload, recipient, resource, original receipt time and source marker. Before
   first recipient-wide acceptance, account for every sibling's active send/
   replay authority; refuse and retain custody if the shared offline result
   would duplicate an unresolved sibling. Commit actual pending row, typed
   acceptance, exact per-operation transfer evidence and terminal atomically.
   Retries and successor readback compare the full immutable identity.
4. Use the existing pending count/quota and FIFO rules. Preserve UUIDv7 row
   order and explicitly normalize pending's millisecond timestamp without
   losing the original exact time in the durable transfer proof. On quota,
   cancellation or COMMIT uncertainty, retain the candidate payload and do
   not infer a negative result. A fresh exact positive readback may reconcile
   an accepted transition only after current claim and sealed-gate checks.
5. Keep ACK versus transfer mutually exclusive under the gate lock. Do not
   change cumulative ACK across tombstones, authorize replay, mark any socket
   write, or collect immutable journals in this slice.

## Verification and deferred gates

Use PostgreSQL 17 separate-session races and native-process COMMIT-loss tests:
marker present/absent and delayed stale attempt; co-location failure; exact
retry after pending claim/flush/ACK/delete; changed payload/time/resource/
recipient/token; two sibling resources and two gates produce one pending row or
explicit retained refusal while replay authority exists; pending quota race,
claim/principal/epoch turnover, ACK versus transfer, partial group recovery,
and source/canonical GC interleavings. Add SQLite tests only for shared pending
identity/quota behavior, without claiming a SQLite SM gate. Run formatting,
scoped Clippy and independent final review.

Authenticated XEP-0424/0425 tombstone acceptance, ordinal behavior across an
exposed tombstone, pending scrub/retention, global GC, old writer drain and all
live/relay/batch/detached source conversion require later coherent gates. No
serving selector or registered migration is activated here.
