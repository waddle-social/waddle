# Dark restricted receipt transaction for #2116 and #2046

## Scope

Extend merged #2216 and #2221 from a rollback-only read probe to one fixed,
fixture-only candidate effect-receipt write and exact readback. Keep the entry
behind `test-support` and disconnected from startup and production UOW callers.
The operation is a proof of one closed transaction shape, not permission to
select restricted serving.

## Coherent implementation group

1. Keep an exact existing main `Database`, its private attested pool capability,
   the pinned restricted profile and a lexical PostgreSQL transaction together
   in a private non-Clone factory/wrapper. Do not reuse the separable relation-
   guard test Boolean or `(Transaction, PreparedAttestedReceiptWriter)` as this
   authority. Expose only a typed effect-receipt operation and result; no raw
   transaction, SQL callback, proof GUC setter, or general-purpose writer.
2. Begin clean READ COMMITTED READ WRITE with the existing tighter timeout
   behavior. Take shared lineage then migration-ledger advisory locks, then
   epoch ACCESS SHARE. After a wait, reattest the same physical/session
   identity and complete restricted profile, including the epoch OID and kind,
   before reading the supported epoch or invoking a candidate function.
3. Call the actual candidate `waddle_ingress_record_effect_receipt` and exact
   `waddle_ingress_effect_receipt_exists` functions under the DML-denied runtime
   role. They must retain their own epoch/XID proof setup. Bind the immutable
   `(MessageKey, kind, semantic_identity_hash)` outside cancellable work.
   Duplicate writes are idempotent; a fresh exact positive readback may prove
   committed effect-receipt existence. An absent or failed readback remains
   indeterminate and never advances a source frontier or starts a new effect.
4. Exercise real PostgreSQL wire loss/cancellation around COMMIT, including
   original backend terminal state and safe pool reuse/closure. Do not infer
   rollback merely from dropping a future. Keep SQLite behavior unchanged.

## Verification and activation boundary

Use PostgreSQL 17 fixtures with the actual candidate API and runtime role:
direct guarded-table SELECT/DML denied, exact-pool mismatch, role/schema/
lineage/OID drift, reconnect, AX-wait mutation, concurrent wrappers, write,
duplicate, rollback, lost COMMIT reply, positive reconciliation, negative
indeterminate result and backend cleanup. Run focused SQLite regression,
formatting, scoped Clippy and independent final review. Report skipped tests
separately from executed PostgreSQL tests.

Do not select ordinary ingress UOW, pooled production receipt entrances,
scheduler/descendant/GC paths, or a serving startup role. Those callers must
move as a coherent future group with provider/role mutation quiescence,
physical-attestor integrity, old-writer/backend drain, all-store custody and
release qualification. The broader submission commit-uncertainty lock-set
proposal remains blocked; this exact receipt pilot does not solve it.
