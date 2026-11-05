# P2.1 notification outbox owner preparation and runtime attachment

The notification outbox currently mixes schema upgrades, missing candidate-ID
assignment and legacy ancestry adoption in `try_new`. Its PostgreSQL schema
gate ends before the data repairs finish. A restricted runtime must not run
those operations or open the detached owner connection used by schema work.

## Dark implementation boundary

- Add an explicit trusted-owner preparation path that holds one exclusive
  startup gate across schema migration and every bounded candidate/ancestry
  repair batch. Preserve the detached owner lock session needed by one-slot
  pools, existing job identities and settlement evidence. A durable revision
  marker is written only after the whole preparation succeeds. Cancellation
  leaves a discoverable task or a retryable incomplete revision.
- Add read-only `attach_existing` on the exact supplied attested PostgreSQL
  main-pool handle. It verifies physical/schema identity, the complete
  preparation revision, relation/index/constraint shape, runtime role and
  effective ACLs in a bounded transaction. It does not reconnect, migrate,
  backfill, adopt ancestry or repair rows. Missing or stale proof fails closed.
- Treat `push_service.adopt_notification_ancestry` as owner-side repair. Do not
  wire a restricted runtime path until that adoption has an owner-prepared
  completion boundary and the old writer/session cutoff is defined.
- Preserve legacy SQLite and owner-startup behavior while the attested serving
  guard remains fatal. This work does not activate restricted startup.

## Required evidence

Disposable PostgreSQL tests must use separate owner and restricted runtime
roles. Verify successful same-pool attach with no DDL or data changes; reject
wrong identity, stale/missing revision, malformed relations and unsafe grants.
Race owner preparation with attachment and a second initializer, and cancel
between schema and ancestry batches: attachment cannot observe partial state,
retry remains idempotent, and retained jobs/candidate IDs/lineage/descendant
rows keep their identities. Exercise the existing one-slot and rollback seams.

The other P2.1 startup stores, epoch/ACL rollout and physical old-writer
fencing remain separate gates. `RestrictedStartupError::UnconvertedStores`
must remain fatal before listeners.

## Coordinated push-store handoff

The push store also creates and upgrades tables during its HTTP constructor,
then adopts notification ancestry before the outbox has finished upgrading.
Prepare its schema and data under a separate trusted-owner gate, followed by
notification outbox owner preparation, then exact legacy push ancestry adoption.
The gate order must not nest push preparation while waiting on the outbox gate.
Publish a push completion revision only after the applicable owner work is
durable. Attach the push runtime to the supplied attested main pool with a
bounded read-only shape and ACL check; preserve the existing legacy startup
path until owner orchestration and old-writer fencing are ready.

The push and outbox attachments remain dark. Push acceptance, delivery and
retirement also touch notification lineage/outbox rows, so their runtime grants
and custody contract need a joint check before serving. Restricted startup
must still fail before listeners while registration, activity, RoomEffect and
other owner constructors remain unconverted.
