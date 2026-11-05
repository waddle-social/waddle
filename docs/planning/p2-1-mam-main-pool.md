# P2.1 MAM exact-main-pool handoff

This is a dark implementation slice of #2116 under #2046. Restricted startup
continues to fail at `UnconvertedStores` until the remaining correctness
stores, role handoff and transition gates are complete.

## Plan

1. Inventory every MAM read, write and caller-owned transaction path in
   `waddle-xmpp` and `waddle-server`, including archive effects inside ingress
   transactions. Settle a narrow cross-crate storage boundary that retains the
   exact physically attested `Database` main pool and its admission permits;
   do not create or expose an independently usable raw PostgreSQL pool.
2. Reuse the existing dark owner-only MAM bootstrap and add a read-only runtime
   attachment contract for exact PostgreSQL identity, owner and runtime roles,
   schema/ACL/sequence shape and completed owner preparation. Owner migrations
   and historical repair remain outside restricted runtime attachment.
3. Preserve configuration provenance. Restricted PostgreSQL accepts only
   `InheritedGlobal` for MAM and rejects explicit or fallback selections before
   opening a connection. Alias MAM lineage to Global only on the attached path.
   Preserve existing SQLite and legacy owner startup behavior.
4. Keep MAM and ingress transaction boundaries coherent, including one-slot
   pools, cancellation and unknown completion. Verify real PostgreSQL role
   separation, exact-pool behavior, archive read/write and caller-owned
   transaction paths, schema/ACL drift, SQLite behavior, formatting, scoped
   Clippy and independent adversarial review.

## Decisions before implementation

- Independent architecture review must settle the cycle-free cross-crate
  adapter and how every MAM operation borrows the admission-aware main pool.
- A trusted operator/bootstrap phase must supply the expected physical
  identity, NOLOGIN owner and exact runtime grants. Existing dark owner
  bootstrap does not establish an active deployment transition by itself.
- Any changed XMPP wire behavior or feature advertisement requires its own
  specification review and activation gate; this slice plans neither.
