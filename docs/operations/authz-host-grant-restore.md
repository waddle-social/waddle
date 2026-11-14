# Host-grant authority restore (#2047)

PostgreSQL is the canonical store of host grants: the room affiliations of
communities enrolled under the host-grant authority
([design](../../server/docs/planning/host-grant-authority-2047.md), ADR-0020
D5). SpiceDB holds a fenced projection of it. Restoring PostgreSQL from a
backup, recovering it to an earlier point in time, or failing over to a replica
that had not received every commit brings back older grants: a member revoked
or banned after the restore point holds the grant again, and the scope's
`active` flag says so. This runbook keeps such a restore from reopening that
authority (gate G13).

No release configuration enrolls a scope, so a deployment without one has
nothing this procedure protects; its owner phase only binds the authority to
the database lineage, and its servers start as before. Apply the procedure to
any restore of a database that holds an enrolled scope.

## What the server checks

The authority incarnation is bound to three witnesses that a restore of the
database does not bring back:

- **The declared restore epoch**, `WADDLE_HOST_GRANT_RESTORE_EPOCH`: a decimal
  integer in the deployment configuration, `0` (or unset) until the first
  restore, raised for each restore. The owner phase and every server must run
  with the same value. The database records the epoch its current incarnation
  was established under.
- **The database lineage** (`_lineage`, [lineage runbook](../../server/docs/operations/db-lineage.md)).
  The owner phase binds the incarnation to it. A restore into another cluster
  or database must be adopted, which rotates the lineage.
- **The projection markers in SpiceDB**, one per enrolled scope, naming the
  incarnation and the generation its projection was last written for.

The owner phase (`waddle-server owner-phase`):

- with an epoch above the recorded one, **declares a restore**: in one
  transaction it establishes a new authority incarnation, records the epoch
  and the lineage, and closes every enrolled scope that is not retired or
  exhausted as `restored`, at its next generation. It logs a warning,
  `declared a host-grant restore`, with the epoch and the number of restored
  scopes;
- with the recorded epoch, changes nothing, except that it binds the lineage
  the first time it runs after migration 2002;
- with an epoch below the recorded one, or against a database that holds a
  scope and was adopted without a raised epoch, is refused (exit status 4,
  `restore_fence`) and changes nothing.

Each server, before it serves anything, verifies the **restore fence**. It
refuses to start, with `The host-grant authority failed its restore fence
(restore_fence): ...`, when the database holds a scope or SpiceDB a projection
marker and:

| Refusal | Meaning |
| --- | --- |
| `epoch_mismatch` | Its configured epoch is not the one the database records: its configuration predates or postdates the latest declared restore, or the database was restored to before it. |
| `lineage_changed` | The database was adopted after a restore or a clone and the restore was not declared. |
| `projection_ahead` | SpiceDB records an operation the database does not: the database lost operations and the restore was not declared. |
| `unknown_incarnation` | SpiceDB was written under an incarnation the database does not know: the database is older than a declared restore. |
| `orphaned_projection` | SpiceDB holds the projection of a scope the database does not: the database lost an enrollment. |

A running server binds itself, when it attaches to the grant store, to the
incarnation, the epoch and the start of the PostgreSQL server it reads. Once
any of them changes it refuses every message to a room of an enrolled scope
with `service-unavailable` and logs, at most once a minute, an error of kind
`authority_changed`. **Any PostgreSQL restart has that effect**, a planned one
or a failover included: restart the servers afterwards (after this procedure,
if the database was restored). Rooms no scope manages are not affected.

A restored scope admits nothing until reconciliation has reconstructed it: it
rebuilds the scope's projection from the database, reads it back from SpiceDB
and activates it under the new incarnation. Where SpiceDB shows that the
restored database lost projected operations, reconstruction first lowers the
database's grants to what SpiceDB holds, so a revoke or a ban the restore lost
stays in force; it never grants anything SpiceDB holds and the database does
not. In this release no server process starts reconciliation on its own; it
is wired into the server together with the grant authority's XMPP handlers.
Until then a restored scope stays closed.

## Restore procedure

1. **Stop every server.** Scale the deployment to zero and wait until no pod
   remains. Old processes, their queued work and their open transactions end
   with them. Do not restore underneath a running server.
2. **Leave SpiceDB as it is.** Its projection markers are what tells the
   servers what the restored database lost. Do not delete, edit or restore
   them. If SpiceDB has to be restored as well, see
   [What a restore can still lose](#what-a-restore-can-still-lose).
3. **Restore PostgreSQL** from the backup or to the chosen point in time.
4. **Rotate the credentials old processes held.** A physical restore brings
   back the roles with their old passwords: set a new password for the
   runtime login before any server starts, and give the servers only the new
   one. Rotate the SpiceDB preshared key as well if any process that held it
   (an operator tool, a pod outside the deployment) may still run. The server
   does not check that a credential was rotated.
5. **If the database now lives in another cluster or database**, prepare the
   lineage adoption (`WADDLE_DB_LINEAGE_ACTION=adopt=<old-lineage-uuid>`) for
   the owner phase run below, as the lineage runbook describes.
6. **Raise `WADDLE_HOST_GRANT_RESTORE_EPOCH` by one** in the configuration
   that both the owner-phase job and the servers read. Never lower it. If a
   previous restore attempt already raised it and the database was restored
   again, raise it above every value used before.
7. **Run the owner phase.** Expect the `declared a host-grant restore`
   warning with the number of restored scopes. If it is refused with
   `epoch_mismatch`, the configured epoch is below the one the database
   records: correct the configuration, do not touch the database. Remove the
   lineage action after a successful run, as the lineage runbook requires.
8. **Re-apply what the restore may have lost and SpiceDB cannot show**:
   revokes and bans acknowledged after the restore point whose projection was
   not yet written (see below), and any scope retirement made after the
   restore point. A revoke takes effect when it commits, also while the scope
   is closed.
9. **Start the servers** with the new configuration and credentials. Each
   logs, if scopes still wait for reconstruction, `Host-grant scopes closed by
   a restore transition admit nothing until their reconstruction is verified
   and activated` with their number. A refusal from the table above means the
   database is not the one the procedure declared: stop and investigate.
10. **Reconstruct and verify.** After reconciliation has run, every restored
    scope is active under the current incarnation:

    ```sql
    SELECT s.closed_reason, count(*)
      FROM host_grant_scopes s, host_grant_authority a
     WHERE s.authority_incarnation = a.incarnation
     GROUP BY s.closed_reason;
    ```

    A row with `closed_reason = 'restored'` is a scope still closed. A scope
    whose reconciliation reports a contradiction (`MarkerMissing`,
    `MalformedMarker`, `MalformedFamily`, `ContentMismatch`,
    `ReconstructionTooLarge`) stays closed until an operator resolves it.

## What a restore can still lose

- **Operations that were never projected.** A revoke or ban committed after
  the restore point whose projection SpiceDB had not yet written leaves no
  trace outside the lost database; an undeclared restore that lost only such
  operations is not detected either. Re-apply them from the audit trail
  (step 8). The window is the projection lag.
- **A retirement.** Retiring a scope writes nothing to SpiceDB, so a lost
  retirement is not detected, and the scope reopens after reconstruction.
  Retire it again (step 8).
- **An enrollment.** The servers refuse to start (`orphaned_projection`),
  declared or not: the scope's rooms would otherwise be served as rooms no
  scope manages. Restoring a later backup is the supported recovery. There is
  no tool in this release to retire a scope the database no longer knows;
  removing the scope's projection marker and its rooms' affiliation
  relationships from SpiceDB by hand (with `zed`) returns those rooms to the
  admission of unmanaged rooms and is an explicit operator decision.
- **SpiceDB lost or restored as well.** Without the projection the restored
  database has no witness: a scope whose projection was ever activated stays
  closed (`MarkerMissing`) and this release has no tool to reopen it. A
  SpiceDB restored to an older point than PostgreSQL is not a PostgreSQL
  rollback: the servers start, and reconciliation reports the older marker.
- **An asynchronous failover that lost commits** is a restore. If the servers
  refuse to start after a failover, follow this procedure.

## Rollout (G24)

- Migration 2002 adds the restore epoch and the lineage binding to the
  authority row, the `restored` state and the `restore` and `reconstruct`
  operation kinds, and an index on the operation journal. The owner phase
  applies it before any server of the release starts; a server of the
  previous release refuses a ledger that holds it. The first owner phase
  after it binds the authority to the database lineage and changes nothing
  else.
- `WADDLE_HOST_GRANT_RESTORE_EPOCH` is new. Unset, it is `0`, which is what
  the migration records, so a deployment that never declared a restore needs
  no configuration change. The owner phase and the servers must read the same
  value.
- Every server now reads, at startup, every projection marker in SpiceDB and,
  for each, the scope it names in PostgreSQL. A deployment without an
  enrolled scope holds no marker and only reads the authority row.
- A server's acceptance of a message to a room of an enrolled scope also
  compares the authority it reads with the one it attached to. Rooms that no
  scope manages, and SQLite deployments, are unchanged.
