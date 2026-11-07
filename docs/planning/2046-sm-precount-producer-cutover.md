# SM pre-count producer cutover

Status: implementation plan under #2046 and #2139; no serving selector.

## Boundary

Every actual live, batch and detached producer must retain a stable immutable
frame occurrence and exact serialized payload before the first cancellable
await or memory-only SM count. A counted frame cannot be discarded because a
later durable append, pending transfer, ACK or route guard refuses it. The
existing one-operation-per-effect-and-resource custody journal is an
obligation key, not a per-frame identity: one recipient pass can yield several
counted frames. Preserve both keys separately, including a nonwrapping
absolute ordinal and the stream's wrapping XEP-0198 counter.

The frame record must bind the verified canonical source and complete frozen
fanout to the effect kind/hash, bare recipient, exact resource, principal,
binding, claim, session and generation. `SmFrameProvenance::Ingress` is a claim
that must be checked against canonical evidence; `Unknown` or incomplete
sources cannot authorize deletion or pending acceptance. Direct messages are
only one frame kind. IQ, presence, MAM and batch output, and payloads within
the supported inbound 1 MiB limit need explicit admission or retained refusal
semantics. Detached persistence currently reserializes XML; the cutover must
either preserve original bytes or establish an explicit semantic-equivalence
contract before promising byte-exact replay.

## Route and settlement order

The selected PostgreSQL lock order is principal, exact claim, sorted canonical
sources, sorted `(source, bare recipient)` guards, sorted bare recipient
guards, sorted session gates, attempts, slots and terminals, then quota and
journal. Discover a bounded complete candidate prefix and sibling set first;
after waits, re-read it under the locks and retry on change. Quiesce the exact
published pump and in-flight output outside database locks, then revalidate
before terminalization. A trigger or legacy path that discovers another
source after a gate is a blocker, not a fallback.

The first grouped implementation can be production-compiled and exercise real
call sites while the authority remains unselected. It must preserve the
selected SQLite and PostgreSQL behavior and keep real pending acceptance,
GC, resume, and restricted startup gated until every competing live, batch,
detached, drained, raw and old-role writer participates or is fenced. The
recipient guard's laboratory `closed` state cannot be adopted as permanent
bare-JID closure: retain route-scoped terminal evidence and support unrelated
later messages with bounded guard retirement.

## Verification

Use native PostgreSQL 17 independent-process races through the actual
producers and ACK/transfer paths: both route/settlement winners, two siblings
and different canonical sources, reversed multi-source ACK, changed prefix
while waiting, late counted drain and GC, stale claim/binding/generation,
Unknown/forged provenance, quota refusal, lost COMMIT and cancellation.
Positive exact-key readback must decide a historical commit; absence remains
indeterminate. Assert that refused counted payload and terminal evidence stay
recoverable. Run SQLite detach/snapshot/wrapping-ACK and cancellation
regressions. No fixture-only marker or synthesized already-counted predecessor
proves the real producer-to-sink interlock.

## Replay-byte preparation

The batch writer now uses one serialized frame for both its SM entry and
transport write. Replay rows retain that frame alongside the parsed stanza,
including across portable and claim-fenced snapshot, incremental and promotion
storage. The second detach drain passes its captured frame into both keyed and
unkeyed detached appends when its XML structurally matches the typed producer
stanza. A malformed or mismatched drain frame takes the existing typed append
path with a warning; it is not covered by the exact-byte claim.
The retained storage frame must end with its one stanza; additional bytes,
including a second root or trailing whitespace, are rejected.
Selected promotion retries retain their existing semantic XML comparison; a
matching retry keeps the first frame's bytes rather than replacing them.

This preserves the existing serializer's output, not the spelling originally
submitted by a client. It does not establish durable custody before counting.
The live writer and batch tail still count in memory before persistence, while
detached and drained paths can await before a supervised immutable occurrence
is committed. Per-frame operation identity, durable nonwrapping ordinal, exact
claim and session binding, complete canonical source and fanout verification,
positive commit reconciliation and supervised failure retention remain required.
No pending, GC, resume or serving authority is selected by this preparation.
