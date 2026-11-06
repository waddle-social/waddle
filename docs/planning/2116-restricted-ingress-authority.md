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

The restricted capability issuer is distinct from the relation-guard pilot,
which verifies a different direct-DML profile. It must verify physical
PostgreSQL identity on every connect and reconnect, the runtime login and
effective role, schema and epoch relation identities, and an exact versioned
definer manifest: function signatures and bodies, ownership, security settings,
execution grants, table grants, trigger profile and row-security behavior.
Issuance alone is insufficient if a privileged actor can change a body or ACL
afterwards. A selected transaction must either refresh that profile after its
epoch prelock or hold a trusted generation barrier that covers catalog/ACL
mutations through commit. The transition must take the epoch barrier before
changing any covered catalog object. The lexical transaction keeps the epoch
lock and node/claim fences through commit or an explicit uncertain-result
handoff. SQLite keeps its existing serialized path; it does not emulate
PostgreSQL privileges.

## Implementation sequence

1. Define and independently review a versioned manifest of expected definer
   operations and the role/catalog properties that fail closed on drift. Add
   a read-only restricted issuer and hostile catalog/ACL mutation tests. Keep
   V1009 and historical migration checksums unchanged.
2. Specify the trusted AX-first catalog-mutation protocol and exact
   per-transaction profile freshness rule. Issue a private, physically bound
   definer capability only after that rule, the manifest and the selected
   ingress lane pass attestation.
3. Replace the test-only Boolean selector with an explicit pool-bound protocol
   choice and lexical owned transaction wrapper. A bare transaction and a
   separate proof must never be movable across one another.
4. Route ordinary ingress UOW preparation, pooled receipt completion, and
   scheduler/descendant transactions through the same choice. Convert raw
   scheduler entrants to typed transaction-taking operations. Distinguish an
   unknown commit from a definitive refusal, retain its exact operation
   identity, and suppress all effect/frontier progress until authoritative
   reconciliation.
5. Test real normal callers under distinct DML-denied runtime and NOLOGIN API
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
