# P2.1 closed operation authority group

Status: draft implementation plan. The restricted startup `UnconvertedStores`
refusal and the ordinary serving selector remain closed.

This group starts from merged main after #2252. It exercises a real caller of
the pinned main-pool authority without treating a dark receipt or an
operator-supplied cutoff record as proof that all writers are converted.

## Scope

1. Give one ordinary ingress operation and its receipt completion a closed,
   production-compiled API. Bind its immutable source and operation IDs, exact
   pool, READ COMMITTED transaction, pinned lineage, complete V1030 ledger,
   epoch, and store proof lexically. Do not return a raw connection,
   transaction, SQL callback, or proof that can be reused on another checkout.
2. Transfer accepted work to supervised ownership before the first
   cancellable await. A dropped caller must retain the operation identity and
   resolve a possibly committed result by positive readback. A timeout or
   missing row is not rollback proof, and receipt evidence alone cannot retry
   an external effect.
3. Establish a bounded single-node deployment lease and durable exact SM
   session-claim candidate. Each affected mutation must check and lock its
   principal, claim epoch, stream and generation in the same transaction;
   an earlier control-pool check cannot authorize a later mutation.
4. Inventory and deny alternate runtime-login callable writers for the
   selected operation. Prove the actual caller uses the closed API and that a
   stale claim, old credential/backend, or untrusted catalog premise refuses.
   Keep SQLite's current path available while it is still the configured
   backend.

## Integration gates

The selected serving switch waits for all enabled stores and the remaining
24 production-compiled legacy attestor call sites, the source/recipient SM
pre-count and settlement interlock, exact claim and publication recovery,
old-writer and privileged-mutator cutoff, and whole-startup qualification.
The v3 quiescence handoff is an operator procedure, not independent proof or
a continuously held fence. This group must not relax either startup refusal.

Focused PostgreSQL 17 and SQLite tests must cover both successful caller
selection and refusal, same-backend stale claim, ambiguous commit recovery,
bounded lock/cancellation behavior, and role/ACL denial. The exact integrated
head needs formatting, strict scoped Clippy, and independent adversarial
review. GitHub Actions and workflow files remain disabled until #2046 is
complete.
