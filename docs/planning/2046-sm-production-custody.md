# Production SM frame and recipient custody

Status: draft plan for a dark production-caller capture slice of #2046. This
work stays unselected until #2116's single-node authority and #1986/#2118's
durable pending sink are ready. Neither restricted-startup refusal may be
removed by this PR.

## Boundary

The `sm_custody` occurrence and settlement work merged in #2253 is a
`test-support`/`clustering` candidate. Its tests construct frames manually.
It does not record frames from the live socket, shared batch writer, detached
queue drain, or retained resume replay, and its transfer table is not the
production pending-delivery sink. This PR establishes a dark call-site capture
contract and cannot count production settlement or pending transfer as done.

## Plan

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
4. Keep this path observational/test-selected. Grant no new producer
   completion, pending transfer, ACK or tombstone authority. Preserve legacy
   behavior outside the dark selector; do not use candidate-local transfer
   rows as the production pending sink.

## Verification and release gates

Exercise actual `/ws` producer entries under pre-count cancellation, DB wait
and unknown COMMIT, write start and flush. Cover multiple recipient frames and
sources, IQ/presence interleaving, byte-identical siblings, both detach drains,
queue refill, repeated delay rendering, and no replay recount. A fresh
process must recover without supplied occurrence IDs. Synthetic occurrence
tests do not suffice. Run affected SQLite regressions, formatting, strict
Clippy and independent exact-head review. Cumulative ACK, production pending
transfer, tombstone and universal old-writer fencing remain subsequent
composition gates with #2116 and #1986/#2118. Remote-room and clustered
serving lanes, old backend drain, and exact-release qualification stay open.
GitHub Actions and workflow files stay disabled until #2046 is finished,
then `cuenv sync -A` will restore them.
