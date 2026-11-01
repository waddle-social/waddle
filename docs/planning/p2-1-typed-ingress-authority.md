# P2.1 typed ingress transaction authority

Status: dark implementation plan stacked on #2153. This work does not install a
guard migration, change normal startup or credentials, unify pools, or activate
an epoch. The installed V1009 trigger still takes the epoch row `FOR SHARE`.

## Boundary

The reusable dark service may be shared through `Arc`. Its private,
non-cloneable transaction capability is created only inside an operation and
cannot escape that operation. PostgreSQL must select READ COMMITTED, establish
bounded transaction-local timeouts, and take `ACCESS SHARE` on the qualified
epoch relation before application locks or transaction reads. A separate read
then validates the supported live epoch and installs the epoch/xid proof. Bind
the schema to independently supplied physical identity; qualify protected SQL.
SQLite keeps its shared database and `BEGIN IMMEDIATE` semantics.

The intended consumer is a closed set of typed database operations. No caller
may receive a pool, connection, raw SQL executor, or caller-owned transaction.
An arbitrary async callback is not by itself proof that the transaction contains
only database awaits. Where conversion cannot be completed coherently, keep the
new service unreachable from normal startup and state the remaining callers.
The closed receipt operation is one isolated example, not a compositional UOW:
atomic alias, message, stream-management, archive, and receipt writes still
need a reviewed set of closed domain operations or another design that preserves
their shared commit without exposing raw transactions or arbitrary async work.

## Migration and review

1. Provide a lexical authority and focused real PostgreSQL and SQLite tests,
   including fail-closed identity, missing foundation, timeout and isolation
   behavior. Verify the candidate against the unregistered guard laboratory.
2. Convert UOW, substrate, receipt, descendants and GC together with their
   notification and push transaction owners; preserve their atomicity, timeout
   accounting and parent/claim lock ordering. Do not leave a public raw route
   from the new authority to guarded writes. Standalone stores need an explicit
   mode rather than an unqualified transaction-time foundation probe.
3. Remove raw correctness-pool escape paths only when their consumers and role
   boundaries have moved. The current generic `Database` and `ConnectionGuard`
   APIs, owner startup, and separate store pools remain outside a dark component
   claim until then. Attestation must run on each physical connection/reconnect.

### Contracts requiring review before changes

- **MAM:** ingress archive writes and the corresponding effect receipt must
  share one transaction; preserve canonical ordering of archive sequence locks
  and claim fencing. MAM reads, standalone writes, schema ownership and lineage
  probes currently use a separate pool. A server-owned `MamStorage` adapter or
  shared private repository must preserve those semantics and SQLite's same
  pool, while removing raw pool and connection escapes. Design admission and
  capacity before unifying pools; do not silently reduce existing capacity.
- **Room replay:** the current outer transaction holds the exact room claim
  through child acceptance, including nested asynchronous work. A replacement
  must preserve that takeover fence through the actual durable child commit,
  retain actor/node authority and cancellation semantics, and avoid waiting on
  an external action or another pool checkout while holding the transaction.
  It needs an independent lock-order review and one-slot/saturated-pool tests.

## Activation gates

The relation-guard candidate is not a registered migration. A production
cutover additionally requires a non-owner runtime role, owner-only bootstrap,
an independent expected-identity handoff, all protected SQL bound to its schema,
old-session and old-binary drain, AX-first transitions, and quiescent guarded-DDL
installation with an enforced migration lock order. Physical reconnect,
commit-indeterminate authoritative lookup, and shared-pool starvation and
capacity policy also remain unproven. A GUC, a pool wrapper, or the dark fixture
alone cannot establish P2.1 completion.
The transaction-local epoch and XID GUCs are consistency checks, not
authentication against arbitrary SQL run with a runtime writer's credentials.
