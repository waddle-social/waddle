# P2.1 activity ownership and epoch-aware orphan GC

This #2046/#2116 tranche advances dark owner and runtime evidence on the
attested main pool and makes orphan-proof GC epoch-aware. It does not enable
restricted serving, transition the epoch, provision live roles, or claim
old-writer exclusion. The fatal `UnconvertedStores` guard remains before
listeners.

## Plan

1. Obtain an independent architecture gate for notification and room
   observation ownership, Push/notification custody and orphan-proof GC
   ordering. Resolve blocking findings before implementation.
2. Give notification activity an owner-prepared completion boundary and
   exact, read-only attested runtime attachment. Preserve legacy startup
   behavior.
3. Make orphan-proof GC acquire the existing epoch lock and transaction
   proof before scanning. `sm_ingress_appends` is outside V1009's guarded
   table manifest, so this establishes GC ordering, not guard coverage.
4. Verify focused PostgreSQL role and race cases, SQLite compatibility where
   affected, formatting, scoped strict Clippy and independent final review.

## Architecture outcome

The independent gate conditionally cleared the two dark implementation
workstreams above.
It did not clear a joint Push/notification custody diagnostic: the existing
per-store checks run in separate transactions and no cross-store completion
invariant or old-writer exclusion proof exists. That diagnostic and any
functional serving composition remain deferred. A room-observation owner and
structural verifier prototype was also removed from this PR after review
found that it omitted registered V1023 indexes, partial-index predicates,
`pg_monitor` grants and the migration-ledger boundary. The prototype is
deferred and is not verification evidence. V1009 role conflict,
TEMP-shadow exposure, raw transaction entry points, direct-primary fencing
and old-session cutoff remain activation gates. GitHub Actions and workflow
files remain disabled until #2046 is complete, per user direction.
