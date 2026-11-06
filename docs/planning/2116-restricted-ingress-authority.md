# P2.1 restricted ingress authority

Status: draft implementation plan for #2116 and #2046. The fixed definer API
candidate is unregistered, and restricted serving remains disabled.

## Boundary

One selected ingress transaction protocol must cover normal submission,
pooled receipt completion, and scheduled descendant work on the same attested
main pool. A runtime login with no direct DML or lock-taking read privilege on
the guarded ingress graph may call only the exact operation-specific definer
API installed by a distinct NOLOGIN owner. No caller-provided Boolean, GUC,
search path, or raw transaction is an authority capability.

The capability issuer must verify the physical PostgreSQL identity on every
connect and reconnect, the runtime login and effective role, schema and epoch
relation identities, the complete definer function inventory, ownership,
security settings, execution grants, table grants and trigger profile. The
lexical transaction keeps the epoch lock and node/claim fences through commit
or an explicit uncertain-result handoff. SQLite keeps its existing serialized
path; it does not emulate PostgreSQL privileges.

## Implementation sequence

1. Define and independently review a versioned manifest of expected definer
   operations and the role/catalog properties that fail closed on drift. Keep
   V1009 and historical migration checksums unchanged.
2. Issue a private, physically bound definer capability only after that
   manifest and the selected ingress lane pass attestation. Replace the
   test-only Boolean selector with an explicit protocol choice that cannot be
   manufactured from an arbitrary transaction.
3. Route ordinary ingress UOW preparation, pooled receipt completion, and
   scheduler/descendant transactions through the same choice. Convert raw
   scheduler entrants to typed transaction-taking operations and preserve
   operation identity and non-advancing unknown-commit outcomes.
4. Test real normal callers under distinct DML-denied runtime and NOLOGIN API
   owner roles, including wrong lineage/role/schema, reconnect, epoch advance,
   hostile temporary schema, catalog/ACL drift, pool contention, rollback and
   commit uncertainty. Record SQLite regressions separately.

## Activation gates

This plan does not register the candidate SQL, remove the restricted-startup
refusal, provision production roles, advance the epoch, install GC v1, enable
mailbox lanes, or switch live traffic. Those changes require the reviewed
provider-managed role transition and old-backend drain, a global lock-order
proof, complete store attachment, durable pre-count SM custody for every
outbound path, and a GC/reconciliation contract that survives unknown commits
and late detached writers. No production readiness is inferred from a dark
capability or a passing fixture.
