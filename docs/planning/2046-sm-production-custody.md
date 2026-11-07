# Durable SM output intent and session sequencer design

Status: design blocked for #2046. This draft PR has no production caller,
stream-wide ordinal, ACK, pending transfer or tombstone authority. It stays
unselected until #2116's single-node authority and #1986/#2118's durable
pending sink are ready. Neither restricted-startup refusal may be removed.

## Boundary

The `sm_custody` occurrence and settlement work merged in #2253 is a
`test-support`/`clustering` candidate. Its tests construct frames manually.
It does not record frames from the live socket, shared batch writer, detached
queue drain, or retained resume replay, and its transfer table is not the
production pending-delivery sink. The shared batch writer owns its vector
before its first poll and awaits receipt work before recording, so cancellation
can drop its only copy. Direct output and ordinary IQ/presence can lack a
message ingress source; concurrent observer completion can reorder a vector
on re-execution. There is no universal durable pre-writer owner in the current
producer contracts. A writer-entry insert, caller-held UUID or producer-set
ordinal would leave a crash/retry hole.

## Required two-stage contract

First, freeze a retry-addressable output intent in the same authoritative
transaction as its source disposition, or under another proven durable source
contract. Key it by stable canonical operation, target logical SM session and
emission slot. Store exact output cardinality including zero and each child's
stable index, stanza/control kind, original serialized bytes and time, and
optional verified source association. Re-execution must return the committed
vector, including distinct byte-identical siblings.

Second, a fenced per-session sequencer must assign checked nonwrapping
absolute ordinals to every countable stanza in actual socket order, after
validating the immutable intent and before exposing bytes. IQ, message and
presence count; SM controls do not. Persist unique session/ordinal and
intent/child mappings. Replay reuses them. Retained, allocated,
transport-attempted and peer-ACK frontiers remain distinct, and an unattempted
tail cannot become peer-ACK proof. Unknown COMMIT needs original backend and
primary reconciliation; fresh-process inventory cannot depend on a lost ID.

The pending-flush row is only a narrow possible source: its archived payload
can change on re-materialization and its offered marker stores no output
bytes. A disposable/test-selected inline transient pending-offer experiment
would not prove production or stream-wide custody.

## Proposed future production plan

1. Review the XEP-0198 wire contract and exact production producer graph:
   live direct output, every countable frame in a shared batch, detached and
   cleanup drains, and resume replay. Define one stream-wide nonwrapping
   ordinal for every countable stanza, including ordinary IQ and presence,
   with optional verified ingress provenance. SM controls do not count.
2. Specify an independent durable owner before any await can drop an uncounted
   frame vector. Record immutable original serialized bytes, original time,
   occurrence identity, source association and ordinal. A fresh process must
   inventory every retained occurrence without being handed a lost ID; source
   re-execution must find existing occurrences, including byte-identical
   siblings. Treat a lost commit acknowledgement as unknown.
3. Carry the dark typed occurrence through direct, batch, detached, both
   cleanup drains and replay. Replay retains its original ordinal and
   occurrence; deterministic XEP-0203 delay rendering derives replay wire
   bytes without mutating the original stored payload or recounting it.
   Define retained, admitted, transport-attempted and peer-ACK frontiers.
4. Keep any early path observational/test-selected. Grant no new producer
   completion, pending transfer, ACK or tombstone authority. Preserve legacy
   behavior outside the dark selector; do not use candidate-local transfer
   rows as the production pending sink.

## Verification and release gates

Exercise actual `/ws` producer entries under pre-count cancellation, DB wait
and unknown COMMIT, write start and flush. Cover multiple recipient frames and
sources, IQ/presence interleaving, byte-identical siblings, both detach drains,
queue refill, repeated delay rendering, and no replay recount. A fresh
process must recover without supplied occurrence IDs. Synthetic occurrence
tests do not suffice. Current replay can append a server XEP-0203 delay
alongside a foreign delay; review conformance before selecting that behavior.
Run affected SQLite regressions, formatting, strict
Clippy and independent exact-head review. Cumulative ACK, production pending
transfer, tombstone and universal old-writer fencing remain subsequent
composition gates with #2116 and #1986/#2118. Remote-room and clustered
serving lanes, old backend drain, and exact-release qualification stay open.
GitHub Actions and workflow files stay disabled until #2046 is finished,
then `cuenv sync -A` will restore them. No implementation or tests have run
for this plan-only PR.
