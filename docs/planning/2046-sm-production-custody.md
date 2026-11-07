# Production SM frame and recipient custody

Status: draft plan for #2046. This work stays dark until #2116's selected
single-node authority and #1986/#2118's durable pending sink are ready.
Neither restricted-startup refusal may be removed by this PR.

## Boundary

The `sm_custody` occurrence and settlement work merged in #2253 is a
`test-support`/`clustering` candidate. Its tests construct frames manually.
It does not record frames from the live socket, shared batch writer, detached
queue drain, or retained resume replay, and its transfer table is not the
production pending-delivery sink. This PR must establish those production
entrypoint contracts before counting custody as implemented.

## Plan

1. Independently review the XEP-0198 wire contract and exact production
   producer graph: live direct output, every recipient frame in a shared
   batch, detached/drained frame expansion, and replay of already retained
   bytes. Define a stable occurrence identity, provenance, payload and
   nonwrapping ordinal before any count or socket await can lose the frame.
2. Record/reconcile each production occurrence on the durable backend before
   producer completion. Carry its identity through actor/endpoint cleanup and
   resume without recounting replay. Treat lost commit acknowledgement as
   unknown and retain original backend/primary custody for reconciliation.
3. Build one recipient-wide settlement protocol for cumulative ACK, pending
   transfer and tombstone: principal/claim, all sorted canonical source
   locks, source-recipient guards, bare-recipient guards, session gates, then
   attempts/slots/terminal/quota/journal. Recheck the discovered source set
   after locking. Fence legacy writers and the delayed
   `insert_or_withhold` path before accepting a pending transfer.
4. Use the production pending-delivery authority when it exists. Keep
   candidate-local transfer rows and actor fixtures isolated; do not wire a
   success route to an incomplete sink. Preserve existing SQLite behavior and
   transport timing until the full authority is composed.

## Verification and release gates

Exercise actual `/ws` producer entries under pre-count cancellation, DB wait
and unknown COMMIT, write start and flush. Cover multiple recipient frames and
sources, exact-byte replay, detach/drain versus admission, stale generation,
queue refill, wrapping cumulative ACK, ACK/transfer/tombstone races, siblings
and reversed lock order. PostgreSQL 17 witnesses must use the actual producer
and pending sink; synthetic occurrence tests do not suffice. Run affected
SQLite regressions, formatting, strict Clippy and independent exact-head
review. Remote-room and clustered serving lanes, old writer/backend drain,
the selected P2.1 authority, and exact-release qualification remain open.
GitHub Actions and workflow files stay disabled until #2046 is finished,
then `cuenv sync -A` will restore them.
