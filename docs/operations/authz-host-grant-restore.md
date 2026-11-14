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

The authority incarnation is bound to what a restore of the database does
not bring back:

- **The declared restore epoch**, `WADDLE_HOST_GRANT_RESTORE_EPOCH`: a decimal
  integer in the deployment configuration, `0` (or unset) until the first
  restore, raised for each restore. The owner phase and every server must run
  with the same value. The database records the epoch its current incarnation
  was established under.
- **The database lineage** (`_lineage`, [lineage runbook](../../server/docs/operations/db-lineage.md)).
  The owner phase binds the incarnation to it. A logical restore into another
  database is adopted, which rotates the lineage. A physical copy (a base
  backup, a volume snapshot, a promoted replica), in place or in another
  cluster, keeps the lineage: the lineage does not detect it.
- **Witnesses in SpiceDB**, which a restore of PostgreSQL does not touch:
  - the **projection marker** of each enrolled scope: the operation whose
    projection was last written;
  - the **journal witness** of each scope: its latest recorded operation,
    written shortly after every commit, whether or not the projection
    followed;
  - the **room witnesses** of each projected scope: which scope each room
    belongs to;
  - the **position witness**: the PostgreSQL system identifier, timeline and
    flushed WAL position, and the epoch, written after commits and whenever
    a server passes its fence.

Witnesses are written after the commit they describe; a revoke never waits
for SpiceDB. One that was not written yet at the restore point records
nothing.

### Configuring the epoch

With the Helm chart, set the epoch in `config.extraEnv`:

```yaml
config:
  extraEnv:
    WADDLE_HOST_GRANT_RESTORE_EPOCH: "1"
```

`config.extraEnv` is rendered into the chart's ConfigMap, which both the
owner-phase Job and the server Deployment read, so one value reaches both.
No template change is needed. Outside the chart, set the variable in the
environment of the owner-phase job and of every server.

## The owner phase

`waddle-server owner-phase`:

- with an epoch above the recorded one, **declares a restore**: it
  establishes a new authority incarnation, records the epoch and the lineage,
  then closes every enrolled scope that is not retired or exhausted as
  `restored`, at its next generation, in batches of 256. It logs a warning,
  `declared a host-grant restore`, with the epoch and the number of restored
  scopes. A run that stopped after the first step is finished by the next run
  with the same epoch (`finished a host-grant restore transition ...`);
- with the recorded epoch, changes nothing, except that it binds the lineage
  the first time it runs after migration 2002;
- with an epoch below the recorded one, or against a database that holds a
  scope and was adopted without a raised epoch, is refused (exit status 4,
  `restore_fence`) and changes nothing.

## The fence every server passes

Before a server admits a message to a room of an enrolled scope, it verifies
the **restore fence**. The server starts whatever the fence finds; what it
affects is managed rooms only.

When the fence refuses **the whole authority**, the server admits no message
to any managed room, logs an error `... are not verified; rooms of enrolled
scopes are unavailable on this replica` with `kind = "restore_fence"` and the
refusal below, and tries again with a backoff of up to a minute:

| Refusal | Meaning | What to do |
| --- | --- | --- |
| `epoch_mismatch` | The configured epoch is not the one the database records: the configuration predates or postdates the latest declared restore, or the database was restored to before it. | Correct the configuration, or follow the restore procedure. |
| `lineage_changed` | The database was adopted after a restore or a clone and the restore was not declared. | Follow the restore procedure. |
| `position_regressed` | PostgreSQL is behind the position SpiceDB witnessed on the same timeline: it was rolled back in place or replaced by an older copy, or a position witness of a later declared epoch exists. | Follow the restore procedure. An acknowledgement does not clear it. |
| `position_unverified` | The PostgreSQL system or timeline changed since the witness: a promotion, a point-in-time recovery, or another cluster. | See [After a promotion](#after-a-promotion). |
| `position_unreadable` | The runtime role cannot run `pg_control_system()`, `pg_current_wal_flush_lsn()` or `pg_walfile_name()`. | Grant `EXECUTE` on them to the runtime role (as the lineage runbook does for `pg_control_system()`). |
| `authority_moved` | The authority changed while the fence ran, three times in a row. | Retried by itself; investigate if it persists. |

When the fence finds evidence about **one scope**, it closes that scope and
nothing else, logging `A host-grant scope was closed: ...` with the evidence:

| Evidence | Meaning |
| --- | --- |
| `projection_ahead` | SpiceDB's projection records an operation the database does not: the database lost operations. |
| `journal_ahead` | The journal witness records an operation the database does not, even one whose projection was never written. |
| `unknown_incarnation` | The projection was written under an incarnation the database never knew: the database is older than a declared restore. |
| `marker_missing` | The scope admitted and SpiceDB holds no projection marker for it: SpiceDB lost or was restored. |

The closed scope is `fenced`: it admits nothing, honours no initiator, is not
projected again and keeps the evidence in SpiceDB as it is. Only a declared
restore reopens it. Reconciliation closes a scope the same way when it finds
the same evidence.

When SpiceDB names a scope **the database does not hold** (the database lost
an enrollment), messages to that scope's rooms are refused with
`service-unavailable`, rather than served as rooms no scope manages. Other
rooms are unaffected.

### While a server runs

A server binds itself to the authority the fence verified: the incarnation,
the epoch, the start of the PostgreSQL server, the database and the
authority relation. When a message to a managed room finds any of them
changed (a PostgreSQL restart or failover, a declared restore, a database
moved, relations recreated by a logical restore), that message is refused,
an error of kind `authority_changed` is logged at most once a minute, and
the server runs the fence again by itself. If it passes, the server admits
again with the new binding; no restart is needed. While it fails, the
server's readiness reports `"host_grants": "authority_changed"` with HTTP
503, so it leaves the endpoint set, and it keeps trying. A server configured
with an epoch below a newly declared one stays not ready until it is
restarted with the new configuration.

A running server also runs the fence every five minutes. A restore that
rewrites rows in place without a restart is found by that pass: it closes the
scopes that lost operations, or, when it refuses the whole authority, stops
admitting messages to managed rooms and reports itself not ready until a
later pass succeeds.

## Scope states

| State | Admits | How it leaves the state |
| --- | --- | --- |
| active | yes | — |
| `restored` | no | Reconciliation rebuilds and verifies its projection: `reconstructed`. |
| `reconstructed` | no | An operator reopens it (`waddle-server host-grant reopen`): active. |
| `fenced` | no | A declared restore: `restored`. |
| `retired`, `exhausted` | no | Never. |

In this release no server process starts reconciliation on its own; it is
wired into the server together with the grant authority's XMPP handlers.
Until then a restored scope stays `restored`.

Query the states with:

```sql
SELECT s.closed_reason, count(*)
  FROM host_grant_scopes s, host_grant_authority a
 WHERE s.authority_incarnation = a.incarnation
 GROUP BY s.closed_reason;
```

A `NULL` reason is an active scope. Scopes bound to an earlier incarnation
are those a stopped transition has not reached yet; the next owner phase
finishes them.

## Operator commands

`waddle-server host-grant <action>` runs as the migrator login, with the
owner phase's connection environment (`WADDLE_DATABASE_URL`, and the schema
and role variables or flags), and can run while the servers run:

- `reopen [--scope <lifecycle>]` reopens reconstructed scopes whose latest
  operation is verified (all of them without `--scope`), and prints how many
  it reopened and how many it left closed.
- `acknowledge-position` records the PostgreSQL system and timeline the server
  now writes on as acknowledged. It never clears `position_regressed`.
- `revoke --scope <lifecycle> --room <room JID> --subject <account JID>
  [--ban] --operation-id <UUID> --account-domain <domain>` revokes (or bans)
  one account in one managed room, in any scope that is not retired, closed
  ones included. It never lifts a ban, refuses to remove the room's last
  owner of the served domain, and takes effect when it commits. Generate the
  operation ID once and repeat the command with it after an unknown outcome
  (exit status 1). `--account-domain` defaults to `WADDLE_XMPP_DOMAIN`.

A scope's lifecycle is `host_grant_rooms.lifecycle_id` for the room.

## Restore procedure

1. **Stop every server.** Scale the deployment to zero and wait until no pod
   remains. Old processes, their queued work and their open transactions end
   with them. Do not restore underneath a running server.
2. **Leave SpiceDB as it is.** Its markers and witnesses are what tells the
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
5. **If the database now lives in another database** after a logical
   restore, prepare the lineage adoption
   (`WADDLE_DB_LINEAGE_ACTION=adopt=<old-lineage-uuid>`) for the owner phase
   run below, as the lineage runbook describes. A physical restore keeps the
   lineage and needs no adoption.
6. **Raise `WADDLE_HOST_GRANT_RESTORE_EPOCH` by one** in `config.extraEnv`
   (see [Configuring the epoch](#configuring-the-epoch)). Never lower it. If a
   previous restore attempt already raised it and the database was restored
   again, raise it above every value used before.
7. **Run the owner phase.** Expect the `declared a host-grant restore`
   warning with the number of restored scopes. If it is refused with
   `epoch_mismatch`, the configured epoch is below the one the database
   records: correct the configuration, do not touch the database. Remove the
   lineage action after a successful run, as the lineage runbook requires.
8. **Start the servers** with the new configuration and credentials. A server
   logs, if scopes are closed for the restore, `Host-grant scopes are closed
   for a restore ...` with their counts. A fence refusal from the first table
   means the database is not the one the procedure declared: stop and
   investigate.
9. **Wait for reconstruction.** Reconciliation rebuilds each restored scope
   from the database, reads it back and activates it; the scope is then
   `reconstructed`. Where SpiceDB shows that the restored database lost
   operations, reconstruction lowers the database's grants to what the
   projection holds, so a revoke or ban the restore lost and SpiceDB saw
   stays in force; it never grants anything SpiceDB holds and the database
   does not. A scope whose reconstruction reports a contradiction
   (`MarkerMissing`, `MalformedMarker`, `MalformedFamily`, `ContentMismatch`,
   `ReconstructionTooLarge`) stays `restored`.
10. **Apply what SpiceDB cannot show.** A revoke or ban acknowledged after the
    restore point whose projection was never written is lost with the
    database; reconstruction cannot lower it. Scopes that were `fenced` with
    `journal_ahead` before the declaration lost such an operation. Apply the
    revokes and bans you know of with `waddle-server host-grant revoke`, and
    retire again any scope retired after the restore point. The audit trail of
    the lost operations is in the lost database; use the application's other
    records and the people who made them.
11. **Reopen.** `waddle-server host-grant reopen` reopens every
    reconstructed scope (or one, with `--scope`). A scope reopens only when
    its latest operation is verified; rerun after the next reconciliation pass
    if the command reports scopes left closed.

## After an undeclared restore is detected

A server that closes scopes as `fenced`, or refuses the authority with
`lineage_changed` or `position_regressed`, is serving a database that was
restored without this procedure. Follow the [restore procedure](#restore-procedure)
from step 1, without restoring PostgreSQL again unless it must be: the
declaration (steps 6 and 7) is what turns `fenced` scopes into `restored`
ones.

## After a promotion

A promotion of a replica (a planned switchover or a failover) and a
point-in-time recovery start a new PostgreSQL timeline, which the position
witness cannot verify: servers refuse the authority with
`position_unverified`.

- If the promoted replica had received every commit (synchronous replication,
  or a planned switchover after it caught up), run `waddle-server host-grant
  acknowledge-position`. The servers' next fence attempt passes and moves the
  witness to the new timeline.
- Otherwise the promotion may have lost commits: it is a restore. Follow the
  restore procedure from step 4. Scopes whose lost operations SpiceDB saw are
  closed by the fence either way.

A promotion also restarts the PostgreSQL server: running servers re-verify by
themselves (see [While a server runs](#while-a-server-runs)).

## What a restore can still lose

- **Operations whose witnesses were not written yet.** A revoke or ban
  committed just before the restore point whose journal witness, position
  witness and projection had not been written leaves no trace outside the
  lost database; an undeclared restore that lost only such operations is not
  detected. The window is the lag of the witness writers. Apply such
  operations again (step 10).
- **What a lost operation changed.** The witnesses show that operations were
  lost, and the projection what it held; neither records what a lost,
  unprojected operation changed. The operator applies it again.
- **An enrollment whose projection was never written.** Nothing outside the
  database knows its rooms; they are served as rooms no scope manages. An
  enrollment whose projection was written is detected, and its rooms are
  refused; no tool in this release retires a scope the database no longer
  knows. Removing its marker and witnesses from SpiceDB by hand (with `zed`)
  returns its rooms to the admission of unmanaged rooms and is an explicit
  operator decision.
- **SpiceDB lost or restored as well.** A scope that admitted and has no
  marker is closed (`marker_missing`) and this release has no tool to reopen
  it. When PostgreSQL and SpiceDB were restored together to points that agree,
  nothing detects it: declare the restore, and review each scope before
  reopening it. A SpiceDB restored to an older point than PostgreSQL is not a
  PostgreSQL rollback: the servers serve, and reconciliation reports the older
  marker.
- **An asynchronous failover that lost commits** is a restore. See
  [After a promotion](#after-a-promotion).

## Rollout (G24)

- Migration 2002 adds to the authority row the restore epoch, the lineage
  binding, a fixed authority identity and the position acknowledgement; the
  `restored`, `reconstructed` and `fenced` states; the `restore`,
  `reconstruct`, `fence` and `reopen` operation kinds; and an index on the
  operation journal. The owner phase applies it before any server of the
  release starts; a server of the previous release refuses a ledger that holds
  it. The first owner phase after it binds the authority to the database
  lineage and changes nothing else.
- `WADDLE_HOST_GRANT_RESTORE_EPOCH` is new. Unset, it is `0`, which is what
  the migration records, so a deployment that never declared a restore needs
  no configuration change. The owner phase and the servers must read the same
  value.
- `waddle-server host-grant` is a new operator subcommand.
- A server starts whatever the restore fence finds; a refusal leaves managed
  rooms unavailable on that server. Readiness fails only while a server has
  not re-verified an authority that changed under it.
- Every server reads, when it attaches to the grant store, every
  projection marker and witness in SpiceDB and, for each scope they name, the
  scope in PostgreSQL. A deployment without an enrolled scope holds none and
  writes none. The built-in SpiceDB schema is unchanged.
- A server's acceptance of a message to a room of an enrolled scope also
  compares the authority it reads with the one it attached to. Rooms that no
  scope manages, and SQLite deployments, are unchanged.
