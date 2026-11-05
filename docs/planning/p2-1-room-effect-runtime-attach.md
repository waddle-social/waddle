# Room-effect outbox restricted-runtime handoff

This dark slice of #2046/#2116 addresses the specialized room-effect outbox
that startup still constructs through owner-capable `try_new` on the global
database. It does not activate new room routing or durable mailbox lanes.

## Plan

1. Reconcile the existing owner preparation and read-only
   `VerifiedRoomEffectOutbox` contract with every enqueue, claim, drain,
   retry, settlement and arm-supervisor caller. Identify the exact authority
   and custody proof needed before a restricted runtime may use those methods.
2. Obtain an independent architecture review of claim fencing, origin and
   lease identities, transactional effects, failure/cancellation outcomes,
   old-writer exclusion, schema/role grants and lock order. A read-only schema
   match must not be treated as delivery authority.
3. If the gate is clear, make functional attachment use only the exact
   physically attested global main pool, with no runtime DDL. Select it from
   startup while preserving SQLite and legacy owner behavior, and keep the
   broader `UnconvertedStores` startup refusal in place.
4. Verify owner/NOLOGIN/runtime separation, exact pool and one-slot behavior,
   role/ACL/schema/identity drift, bounded enqueue/drain and cancellation,
   recovered custody, existing SQLite paths, formatting, scoped Clippy and
   independent final review. Record any activation dependencies separately.

The implementation scope depends on the architecture gate. If functional
attachment cannot yet preserve the existing authority contract, keep this
slice at a non-serving preparation boundary and identify the prerequisite
under the existing P2/P3 roadmap.

## Architecture outcome

The functional attachment gate remains closed. The current claim and lease
handoff does not yet prove exclusive custody, recover unknown completion, or
exclude the old writer across owner transfer. This slice therefore adds only
an exact, read-only verifier for the MUC room, lifecycle, admin receipt,
destroy completion, and room-effect tables on the physically attested main
pool. Its opaque proof cannot enqueue or drain effects. Startup continues to
fail closed under `RestrictedStartupError::UnconvertedStores`.
