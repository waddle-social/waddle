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
