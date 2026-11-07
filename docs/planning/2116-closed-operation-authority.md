# P2.1 closed operation authority group

Status: draft implementation plan. The restricted startup `UnconvertedStores`
refusal and the ordinary serving selector remain closed.

This group starts from merged main after #2252. It exercises a real read-only
caller of the pinned main-pool authority and prepares generic SM
frame-occurrence custody. Neither dark candidate selects serving.

## Scope

1. Add a closed, production-compiled PubSub v2 profile diagnostic on the
   exact pinned pool. One bounded READ COMMITTED read-only transaction takes
   the shared catalog fence first, then verifies pinned lineage, complete
   V1030 ledger, epoch/manifest catalog and epoch, followed by the PubSub
   role, owner, relation and function profile. Contention retries only after
   acknowledged rollback. Return an observation only: no raw connection,
   transaction, pool, SQL callback or reusable proof may escape. Keep the
   legacy SQLite and owner-startup paths unchanged.
2. Add a separate immutable frame-occurrence identity to dark SM custody.
   Preserve the effect/resource key as the source obligation. Bind each frame
   UUID to exact serialized bytes, receipt time, complete provenance,
   absolute ordinal and wrapping handled count under captured principal,
   resource, binding, claim, session and generation. Exact-key retry preserves
   the original result; conflicting reuse fails. An ambiguous commit or
   failed readback retains the UUID and bytes as Unknown without a fresh
   identity or deletion. If the current persistence adapter remains
   test-support-only, report that boundary rather than claiming live custody.
3. Test actual PubSub diagnostic selection, same-backend ordering, stale
   pins, role drift, cancellation and bounded waits on PostgreSQL 17. Test
   candidate frame identity, two frames from one obligation, byte-exact
   readback, ordering and wrap, stale generation, quota and lost-COMMIT
   uncertainty. Preserve SQLite behavior where supported.

## Integration gates

The selected serving switch waits for all enabled stores and the remaining
24 production-compiled legacy attestor call sites, source/recipient SM
pre-count and settlement across every live, batch, relay and detached
producer, exact durable claim and publication recovery, old-writer and
privileged-mutator cutoff, and whole-startup qualification. A one-producer
SM selector is unsafe while other countable producers share its stream.
The v3 quiescence handoff is an operator procedure, not independent proof or
a continuously held fence. This group must not relax either startup refusal
or wire a production SM producer, ACK, pending, resume or GC selector.

The exact integrated head needs formatting, strict scoped Clippy, focused
PostgreSQL/SQLite tests, and independent adversarial review. GitHub Actions
and workflow files remain disabled until #2046 is complete.
