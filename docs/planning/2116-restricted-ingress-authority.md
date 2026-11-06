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

## Step 1 diagnostic contract

`AttestedPgPool::inspect_restricted_ingress_profile` issues a diagnostic
`RestrictedIngressProfileObservation`, not a transaction or serving capability.
It leases an existing hook-bearing physical pool, verifies its configured
lineage/schema/runtime identity again in a read-only repeatable-read
transaction, and requires an independently trusted epoch relation OID. It
normalizes retained connection search paths through the existing pool hook;
unexpected effective roles or physical identities must pass that hook before
any catalog observation is returned.

The version-1 PostgreSQL 17 manifest pins 78 functions: the unregistered fixed
API, inherited ingress guards, and the canonical pending recovery trigger
function. Function bodies are exact byte slices of checksum-pinned source
assets. Their catalog properties include input/output signatures, argument
names/defaults, language, security mode, volatility, configuration, owner and
execution grants. All non-internal ingress triggers are checked independently.
The structural manifest covers the thirteen guarded tables, epoch and guard
manifest tables, `sessions`, and the canonical owner-prepared `pending_delivery`
table. It pins columns/defaults, checks, unique/foreign-key constraints,
indexes, live parent/child inheritance edges, and internal/non-internal trigger
definitions. Dependencies must neither inherit nor have children, including
children outside the ingress inventory or foreign-table children. Physical
catalog OIDs and planner statistics are excluded from the portable structural comparison;
the attested schema identity and supplied epoch OID are checked separately.
The manifest producer executes the actual ordered migration SQL and canonical
pending owner DDL inside one rollback-only disposable transaction. The PG17
test independently runs `MigrationRunner` and the actual pending owner bootstrap
in a different schema before comparing the profile.

The diagnostic refuses direct runtime guarded-table access, unexpected table
or column grants, role memberships, elevated roles, row security/policies,
partitions/inheritance, additional API-owner functions anywhere in the
database, and alternate security-definer functions owned by roles with guarded
write authority. The last exclusion is conservative even when the runtime
cannot directly execute the alternate function: differently owned wrappers
could otherwise chain execution grants. The pending owner schema is required;
absence is not treated as a serving-compatible profile.

Independent quiescence of catalog, role and ACL mutation remains a precondition
throughout inspection. Repeatable-read does not turn PostgreSQL privilege
caches into a catalog-generation barrier, and the observation is stale when
returned. The separately installed physical-identity attestor is an independent
trust prerequisite; this diagnostic does not verify its body or fence its
installer. Its pool-hook calls also precede any future ingress epoch lock.
Trusted lineage/ledger then epoch-AX mutation, transaction-local profile refresh
after epoch AS, exact pool binding, lexical transaction ownership and explicit
commit-unknown custody remain prerequisites for the later grouped tranche.
No migration is registered, no epoch is advanced, and startup stays disabled.

## Scoped owner-rejection barrier correction

The existing dedicated owner-rejection grant transaction now takes lineage
then migration-ledger advisory locks, performs a catalog-only primary/system,
database/schema and epoch identity check, and acquires epoch ACCESS EXCLUSIVE
before reading lineage, migration ledger or guarded relations. After waiting,
it revalidates the owner role graph, physical identity and the same epoch OID.
It verifies the scoped V1028 ledger/trigger/ACL profile again before COMMIT.
Read-only inspection uses ACCESS SHARE and rolls back its transaction. The
owner must have permission to acquire epoch AX; the fixture provisions epoch
ownership explicitly.

This operation intentionally grants direct runtime SELECT/INSERT on the
owner-rejection mapping. Its fixed scoped verifier is therefore distinct from
the DML-denied restricted-definer manifest. Neither path selects serving.

This correction covers one cooperating mutator. It does not supply a generic
provider mutation entry, a caller-owned durable attempt/commit receipt, or a
runtime UOW that refreshes the restricted manifest in the same READ COMMITTED
transaction after epoch AS. The existing CommitUnknown refusal remains
non-advancing, but lacks an operation receipt: callers must fence the original
backend/primary before reconciliation or retry. A later matching grant alone
cannot establish which attempt committed. Ordinary migrations, legacy startup,
external provider/role mutators, the independent attestor and serving selection
remain gated.
