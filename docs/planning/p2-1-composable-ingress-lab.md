# P2.1 composable ingress unit-of-work laboratory

This is a test-support-only #2116 experiment in a disposable PostgreSQL
database. It does not install a migration or change startup, role provisioning,
wire behavior, or production transaction selection. It is not P2.1 activation
or the P2.2 mailbox prerequisite.

The fixture installs the unregistered relation-guard candidate under a separate
owner before opening its restricted runtime pool. The same physically attested
pool is split into one main and one reserved ingress admission slot. A private
fixture token binds the exact ingress admission and attestation capability;
the alternative UoW entry rejects another pool, the main admission lane, and a
wrong deployment lineage. It selects READ COMMITTED, preserves tighter session
timeouts, bounds checkout and lock waits, takes the qualified epoch relation
lock before application locks, attests the transaction's physical connection,
and reads and proves the fresh epoch. The fixture bounds the entire chain and
classifies a timeout after COMMIT begins as indeterminate. Only the fixture can
construct the token.

The existing laboratory chain now uses an actual `IngressUnitOfWork` and one
caller-owned `IngressUowTransaction` on that ingress lane. It composes the
canonical message, effect-intent reconciliation, receipt, delivery identity and
descendant before a single commit. Intent and receipt use their ordinary UoW
repositories; the fixture-only canonical and descendant adapters reuse their
prepared writers without the installed guard's epoch row lock. The wrapper
does not return a pool, raw connection, SQL executor, or arbitrary callback.
Fixture-only observations and fault injection use the existing internal SQL
access and owner/admin connections; they are not part of the domain API.

The PostgreSQL laboratory checks the restricted grants and non-owner role,
admission isolation under saturation, qualified writes despite a shadow search
path, replay and conflict behavior, rollback, invalid proof, epoch transition
and lock ordering. It also cancels a blocked write and exercises a commit reply
both disconnected and held past the caller's deadline after the server commits.
Both commit outcomes must remain indeterminate to the caller; a later
independent read observes the test's durable rows and is not an automatic
recovery or retry policy.

The regular UoW entry and canonical/descendant paths still reacquire the V1009
epoch row lock. The SELECT-only runtime role in this experiment cannot use
those paths under the candidate guard. Other UoW repositories, callers,
pool escapes, owner migrations, cross-store custody, role provisioning,
commit-unknown reconciliation, and shared-pool capacity remain outside this
slice. The SQL guard itself cannot prove first-lock ordering for arbitrary
writers; all writers must migrate to a closed authority before activation.
