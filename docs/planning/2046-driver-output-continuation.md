# Dark bounded output continuation for #2046 P3.1

## Scope

Prove one synthetic, test-support-only connection actor producer that yields
bounded output incrementally through the reserved loopback pumps and one
ordered writer. Keep generic inbound/peer handlers rejected and the serving
selector unchanged. This is a local scheduling and custody proof, not a
production control-latency or durable-resume claim.

## Coherent implementation group

1. Define a typed retained continuation with a unique exact `ConnectionRef`,
   effect-once state, next output ordinal and terminal state. Generate each
   bounded frame from the cursor; do not split a fully materialized
   `ResponseBatch` or `DriveOutcome`. Reserve hard frame-count and serialized-
   byte credits before effects, and retain or atomically transfer those credits
   through writer admission and settlement without a full-budget deadlock.
2. Keep one owner-selected arbiter for continuation chunks, existing direct
   frames and permitted SM control. Admit at most one continuation frame to
   the writer at once. Control may progress at a safe yield only after the
   handled prefix and checkpoint are durable; a stalled effect or physical
   writer is not assigned a false latency bound.
3. Retain the exact frame, cursor and `WriteReceipt` through admission,
   attempt and flush. Cancellation of a pending `WebSocketOutput::submit`
   future cannot drop the only frame. Distinguish not attempted, attempted or
   uncertain, and locally flushed; after uncertainty stop the suffix and
   forbid an unproven retry. A local flush is not client ACK. Cloneable ticket
   snapshots cannot confer ownership or release credits early.
4. Preserve SM handled-prefix, outbound append/exposure, replay and ACK as
   separate milestones. Count XEP-0198 control elements as uncounted. Keep
   existing direct-frame pending/ingress obligation references intact.

## Verification and activation boundary

Use deterministic loopback tests for effect-once cancellation before and
between chunks, exact/over-limit count and byte reservations, duplicate owner
claims, direct-frame ordering, a handled-prefix hole, `<r/>` at a safe yield
under output pressure, `<a/>` during a full window, and stop/revoke before
admission, at writer readiness, after attempt and after flush. Assert retained
input/output credits and joined cleanup at each terminal state. Run relevant
XEP-0198 tests, formatting, scoped Clippy and independent final review.

This fixture does not enable generic `process_inbound_frame` or peer dispatch,
replace the production socket loop, create P2 acceptance custody, establish
P3.3 durable session/frontier authority, guarantee control progress during a
stalled effect/socket, or activate a serving endpoint. Those remain #2046
gates. If the fixture cannot attach to the actual reserved `WebSocketInput`
and `WebSocketOutput`, label it a scheduler model rather than transport proof.

## Phase 1 implementation boundary

The current phase implements only the test-support scheduler model: typed lazy
frame production, pre-effect count and worst-case byte reservation, an exact
connection owner claim, a bounded handled-sequence window, and separate
prepared/admitted/attempted/uncertain/flushed milestones. Its tests exercise
that model without a socket. They do not prove async effect cancellation
custody, direct-frame or control ordering through a physical writer, actual
`WebSocketInput`/`WebSocketOutput` integration, receipt retention across
caller cancellation, or joined cleanup. Those transport guarantees remain
required before calling P3.1 complete.
