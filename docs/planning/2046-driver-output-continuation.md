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

## Phase 2 architecture gate

The independent review approved implementation subject to these blocking
conditions:

1. A worker taking physical I/O stays parked until the exact endpoint cleanup
   record accepts it. Validate the input, output, state and registered direct
   custody against the same complete `ConnectionRef` before release.
2. Keep the typed frame, cursor, reserved credits and actual write receipt
   discoverable through the exact cleanup record after a worker exits. An
   unresolved output cannot be acknowledged or pruned until the receipt is
   terminal, both physical pumps and the worker have joined, and the exact
   owner explicitly reconciles it.
3. SM control uses separately bounded capacity. The worker polls effect,
   admission, input and stop independently; a control response joins the
   same writer at a safe yield. No latency claim applies while a checkpoint
   or the current physical write is stalled.
4. Record each countable synthetic or direct frame exactly once in actual
   arbiter order. Keep outbound append, exposure, local flush and peer `<a/>`
   separate, including at a full send window. In-memory replay is not durable
   replay. A durable inbound-checkpoint claim additionally requires an enrolled
   resumable stream, a genuinely dirty handled-prefix tracker and ingress
   authority readback before the wire acknowledgement.

Do not hold lifecycle or ledger locks over an await or callback. Preserve the
existing index-to-lifecycle-to-ledger lock order.

The hard producer byte bound applies only when the synthetic producer's
maximum encoding is established before its effect starts. If a producer
violates that contract, the model seals and retains its typed result and the
full reservation for reconciliation. Retaining an unexpectedly oversized
result cannot retroactively bound the allocation made by an arbitrary effect;
generic effect-producing handlers remain unsupported.

## Physical arbiter redesign for review

An initial transport draft awaited the SM checkpoint inside a writer-select
branch; a stalled checkpoint could therefore prevent polling the already
pinned writer admission, receipt and stop. That draft was removed. The next
design requires an independent challenge before implementation:

- One parked driver loop holds separate pinned slots for the pre-reserved
  effect, an SM checkpoint, writer admission and the resulting receipt. Each
  turn polls those slots and stop/lease control independently; none of the
  select branches waits inline for another operation.
- Before starting a checkpoint, retain the original input permit and an
  independently bounded control reservation in the app-owned ledger. The
  checkpoint temporarily owns the sole connection core and returns it with its
  typed result. A drop guard must escrow that core on abort, including when the
  future has never been polled. The design must prove drop ordering and avoid
  holding any lifecycle or ledger lock across an await or callback.
- Keep an admission future pinned until it resolves or stop wins. Store its
  exact-connection receipt synchronously before any further await. While an
  admitted write is active, continue observing its attempt/terminal progress,
  checkpoint completion and stop. Control output uses the same writer at the
  next safe yield; a stuck sink has no promised control latency.
- Bound and retain typed replay independently from the producer and distinguish
  append, possible exposure at `start_send`, local flush and peer ACK. Do not
  accept an ACK past the actually attempted prefix. Explicitly review races
  between receipt progress and incoming ACKs, abort and joined reconciliation,
  exact owner replacement, and memory accounting of cloned frames.

The retained startup and model changes do not yet implement this state
machine, attach a continuation to the physical writer, or prove transport
custody. The PR remains a draft until those gates and their tests are complete.
