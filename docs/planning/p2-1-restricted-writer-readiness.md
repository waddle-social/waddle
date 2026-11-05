# P2.1 restricted-writer readiness

This #2046/#2116 tranche advances dark owner and runtime evidence on the
attested main pool. It does not enable restricted serving, transition the
epoch, provision live roles, or claim old-writer exclusion. The fatal
`UnconvertedStores` guard remains before listeners.

## Plan

1. Obtain an independent architecture gate for notification ownership,
   Push/notification custody and the epoch-zero ingress writer boundary.
   Resolve blocking findings before implementation.
2. Give notification activity and the remaining observation stores an
   owner-prepared completion boundary and exact, read-only attested runtime
   attachment where the current schema and callers permit it. Preserve
   legacy startup behavior.
3. Add a dark joint Push/notification role, ACL and custody diagnostic over
   the same attested main handle. It must not return functional handles or
   promote the existing Push composition result to serving readiness.
4. Close only the epoch-zero ingress guard path that can be proved under the
   installed V1009 contract. Do not register a new epoch candidate or flip
   the live epoch without direct-primary, role and old-session evidence.
5. Verify focused PostgreSQL role and race cases, SQLite compatibility where
   affected, formatting, scoped strict Clippy and independent final review.

The architecture gate may narrow this grouped PR. Any deferred work will be
recorded with its concrete prerequisite. GitHub Actions and workflow files
remain disabled until #2046 is complete, per user direction.
