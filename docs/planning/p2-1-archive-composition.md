# P2.1 archive-after-pin composition

Status: investigation in progress, stacked on #2157. This slice is dark and
does not activate P2.1 or change server startup, pools, migrations, or XMPP.

## Plan and gates

1. Extract the existing archive-after-pin transaction body into a private
   helper without changing its transaction boundary, return values, or lock
   order.
2. Exercise canonical-before-room and room-claim-before-canonical/takeover
   ordering against real PostgreSQL with bounded waits and deterministic
   coordination. Distinguish measured results from conclusions derived from
   source inspection.
3. Only after the lock-order evidence and an independent design review admit it,
   add a closed, dark archive operation to the typed authority. Its private
   transaction must retain the exact room fence, archive ordinal, and receipt
   settlement together, with schema-qualified PostgreSQL MAM SQL and SQLite
   semantics that use the same shared backend. Do not expose a raw executor,
   transaction, or arbitrary callback.

Verify the relevant PostgreSQL inventory, focused real-PostgreSQL tests, Rust
formatting, and strict Clippy. Report unverified assumptions and stop at the
extraction and test if the operation cannot satisfy either backend safely.

## Lock-order investigation

On disposable PostgreSQL 17.11, the repository-level queued-takeover test
observed a claim takeover waiting for a replay's shared room claim, and a
nested child waiting for an archive's canonical row. In that same schedule the
archive acquired a matching shared room claim while the takeover remained
queued: the hypothesized third blocking edge was absent. The old exact claim
was rejected after takeover committed. A separate test observed an archive
waiting on an already-held takeover claim and returning the typed timeout after
the test's four-second lock bound. Production currently uses 100 ms; the longer
test bound lets the observer establish the blocking edge. These results do not
prove that all application paths or schedules are deadlock-free.

The current dark authority and ingress unit of work use different private
transaction types. Existing MAM and ingress repositories issue unqualified
SQL, and the authority's separate SQLite constructor does not establish that it
uses the runtime's shared backend. These remain design blockers for a closed
archive operation; no such operation is added by this slice.
