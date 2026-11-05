# P2.1 gated relation-lock ingress writer pilot

This plan follows hosted main `7610378cd98c87bfad34f02e931ef2ae2067543d` (#2202). It proposes one dark, production-compiled writer path for #2116. It does not establish database authorization against arbitrary SQL issued by the runtime login, and it does not authorize restricted server startup, a runtime role cutover, an epoch advance, deployment changes, or production activation.

## Decision

**GO** for a private relation-lock capability on the ordinary ingress transaction path, with real PostgreSQL 17 tests through normal Waddle APIs against a disposable database. Compile the explicit capability-taking UOW API for production code, but keep its verifier-issued capability opt-in and unwired in normal serving startup. `IngressUnitOfWork::begin()` remains on V1009.

**NO-GO** for registering or auto-applying the candidate guard, changing the default writer protocol, or enabling restricted serving in this PR. The old-writer cutoff and epoch activation remain separate gates.

A PostgreSQL-only `UPDATE (id)` grant can make V1009's `FOR SHARE` work while denying updates to the epoch columns. The current disposable pilot already exercises that grant. It is a compatibility workaround for the installed row-lock guard; it retains the global epoch-row lock and does not advance #2116's target transaction authority. The candidate relation lock is the more relevant next boundary.

## Current source and the missing path

- `server/crates/waddle-server/src/db/migrations/waddle.rs` registers V1009's invoker guard. Its trigger reads `ingress_protocol_epoch ... FOR SHARE` before checking the epoch proof.
- `server/crates/waddle-server/src/ingress_uow/mod.rs::begin_inner` normally selects `EpochPreparation::InstalledGuard`. The attested path calls `prepare_attested_receipt_transaction`; the non-attested path calls `acquire_epoch_lock_first`.
- `server/crates/waddle-server/src/ingress_substrate/substrate.rs::acquire_epoch_lock_first` and `read_live_epoch_for_share_in_prepared_transaction` still use the V1009 row lock. The ordinary pooled receipt path in `ingress_substrate/authority.rs` and raw descendant path in `ingress_uow/effect_descendants.rs` also remain on that protocol.
- The relation path is test-only today: `EpochPreparation::RelationGuardLab`, `begin_in_relation_guard_lab`, `prepare_relation_guard_lab_transaction`, and special repository adapters selected by `test_support/relation_guard_lab.rs`. It calls the test-support-only `prelock_epoch_relation` and `read_epoch_after_prelock`.
- `db/migrations/owner/ingress_epoch_relation_guard_candidate.sql` is unregistered lab SQL. It requires READ COMMITTED, an epoch-table ACCESS SHARE prelock, then a separate live epoch read. `db/migrations/owner/transition_lab.rs` replaces the candidate guard, revokes runtime `UPDATE (id)`, and records a marker, but deliberately leaves the migration ledger and epoch unchanged.
- `db/migrations/owner/epoch_acl.rs` already distinguishes the V1009 SELECT + UPDATE(id) stage from the candidate SELECT-only stage. It is a read-only diagnostic, not a serving capability; it cannot establish catalog quiescence or old-writer drain.
- The lab covers the candidate guard and a real repository chain, but its runtime UOW path is the alternate test-only preparation and its ingress lane has one slot. It does not prove that ordinary `IngressUnitOfWork::begin`, pooled receipts, or raw descendants use the candidate path, nor that two independent runtime writers proceed concurrently.

The functional gap is therefore the common writer entrypoint, not another SQL-only guard experiment.

## Proposed PR boundary

Promote the candidate preparation to one private, production-compiled `RelationGuardCapability` path:

1. Keep `db/migrations/owner/ingress_epoch_relation_guard_candidate.sql` unregistered. Do not add a V1029 ledger entry or change the auto-applied migration set in this pilot. The focused fixture may install the candidate SQL in a disposable database after the committed baseline migrations. Old binaries reject unknown Waddle-owned versions at or above 1000 on restart, so SQL compatibility at epoch zero does not make a registered V1029 expand-compatible. The candidate guard also has no old/new writer discriminator; do not install it while old writers remain active. Any future production installation must follow an explicit old-login cutoff and backend drain, or use a separately reviewed migration that is gated behind that cutoff.
2. Add a read-only relation-guard verifier for an operator-prepared database using the exact attested global attachment. It must verify the owner-held marker, exact trigger/function/manifest shape, runtime role and effective ACLs on the epoch and guarded relations, and physical/schema identity in the same pool. Return a private capability bound to that pool, role, schema OID/name, and verified guard marker. No URL or caller-supplied epoch may construct it. The capability proves the verifier observed the expected preparation state; it does not make the trigger's transaction-local GUC proof unforgeable.
3. Add `IngressUnitOfWork::begin_with_relation_guard(capability)` as an ordinary, production-compiled but explicitly selected UOW API. Keep `begin()` on the installed V1009 path and leave normal serving startup unchanged; startup must not mint or select this capability. In relation mode only, set READ COMMITTED and bounded timeouts, acquire the schema-qualified epoch relation's ACCESS SHARE lock before domain/claim/advisory locks, then read the epoch in a separate statement and install the transaction-local proof. This establishes lock ordering for callers using this capability path; it makes no new lock-order claim about the V1009 path.
4. Carry the prepared relation authority through the normal repository methods reached from that UOW. Raw descendant helpers in that transaction must consume the prepared token or fail closed; they must not silently reacquire V1009's row lock. Keep the standalone pooled receipt path on its current protocol until a later slice converts it.
5. Refactor the relation-guard lab to provision and verify the disposable roles/marker, then call the ordinary UOW and repository APIs. Retire the parallel lab-only UOW and repository adapter for this chain.
6. Keep `main.rs` and `server/lifecycle.rs` unchanged. Do not mint this capability during normal startup; `RestrictedStartupError::UnconvertedStores` remains in front of listeners.

If the verifier cannot construct the capability from a coherent operator-prepared/attested path, stop: do not promote the fixture token into a production type. The verifier and constructor must be production-compiled, even though this pilot does not install candidate objects into a serving database.

## Focused PostgreSQL 17 evidence

Use a disposable database with separate NOLOGIN schema owner, migrator login, runtime login, and attestor definer. Apply the committed baseline migrations, then install the unregistered candidate guard and marker only in its dedicated disposable fixture. The runtime must connect as its own login (no `SET ROLE` fixture shortcut). The baseline V1009 fixture grants only SELECT plus `UPDATE (id)` needed for `FOR SHARE`; the candidate fixture grants SELECT and its exact DML, but no UPDATE on the epoch relation. In both fixtures the runtime has no owner membership, schema CREATE, database CREATE/TEMP, or ability to change guard objects.

The focused test should cover:

- The normal `IngressUnitOfWork::begin_with_relation_guard` entrypoint followed by actual message/effect/descendant repository methods. Confirm READ COMMITTED is set before the fresh epoch read and the gate lock precedes domain or advisory locks.
- Verify the unchanged default `begin()` path against the baseline V1009 fixture, then separately verify the explicit relation-capability path against the candidate-guard fixture at epoch zero and epoch one. Do not claim the two guards coexist in one expanded production schema. Test missing, stale, and mismatched application-prepared proof on the capability path. Do not require a caller-forged GUC proof to fail: PostgreSQL custom GUCs are writable by the session, so this trigger proof is cooperative transaction state, not an unforgeable identity or authorization token. If resistance to direct SQL that forges this GUC is part of the security requirement, this candidate does not satisfy it and the restricted-authority work remains NO-GO until the DB protocol uses an unforgeable or separately authorized proof.
- Two independent runtime writers can both hold the epoch ACCESS SHARE relation lock. Then an owner ACCESS EXCLUSIVE transition waits for both. A writer queued behind that transition starts only after commit, reads epoch one in its separate READ COMMITTED statement, and uses the new proof. Use two ingress slots or independent sessions; the current one-slot lab cannot establish writer concurrency.
- Cancellation while waiting releases the lock and produces no completion evidence. Unknown commit remains indeterminate and is read back on a fresh authoritative session before any retry.
- Capability rejection for wrong pool/schema/role, relation-guard or marker drift, and any unexpected privilege. The runtime cannot alter the epoch, assume owner/migrator, or disable triggers. Application raw-descendant helpers must fail without the prepared token; this does not prevent a SQL client using the runtime login from attempting direct DML or forging the custom GUC described above.

The epoch-one case is a disposable concurrency test, not an activation. Do not wire the standalone pooled receipt writer or all server stores in this pilot.

## Rollout and remaining gates

This pilot makes no expand-compatibility claim for candidate SQL. A later production guard install must occur only after old binaries have been cut off and drained because old binaries reject an unknown Waddle-owned migration version, and the candidate trigger cannot distinguish old from new writers. Any later epoch transition must use ACCESS EXCLUSIVE on the same epoch relation; tests must prove queued writers observe the committed epoch with a fresh READ COMMITTED statement. V1009 is checksummed history and must remain unchanged.

The relation path is incompatible with unmodified writers after guard installation/epoch activation. The present operator assertion in `RuntimeStartupIdentity` certifies only an asserted direct, fenced primary; it does not prove old sessions are drained. Before guard installation, role cutover, or epoch activation, a separate reviewed transition must disable the old login, prove/terminate existing backends (including idle ones), and verify no old writer can reconnect. Do not claim this pilot supplies that cutoff.

The owner login must remain separate from the runtime login; runtime must not inherit owner/migrator/definer authority. The new verifier and capability bind the runtime UOW to the exact pool and schema, but they do not establish that all startup stores or writers use this path. Nor does the relation-lock capability stop an authorized runtime SQL session from setting the candidate trigger's GUC directly.

Full all-store restricted startup remains impossible on this head: `db/runtime_startup.rs` attaches the attested global database and `db/bootstrap.rs::verify_global_runtime` verifies global lineage, ledger, and global ACLs, while `server/lifecycle.rs::start_with_config` still rejects attested pools with `UnconvertedStores` before listeners. #2202 gives the legacy owner path one shared main pool; it does not provide all-store attested attachment or epoch/role/old-writer authority. Keep those as later gates.
