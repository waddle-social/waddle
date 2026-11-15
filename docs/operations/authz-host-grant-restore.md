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
- **Witnesses in SpiceDB**, which a restore of PostgreSQL does not touch, all
  under the `waddle_grant_projection` definition:
  - the **projection marker** of each enrolled scope (resource
    `<lifecycle>`): the operation whose projection was last written;
  - the **journal witness** of each scope (`head_<lifecycle>`): its latest
    recorded operation, written shortly after every commit whether or not the
    projection followed, and after every fence pass that passes for any
    scope whose witness is behind, retired scopes included;
  - the **room witnesses** of each projected scope: which scope each room
    belongs to;
  - the **position witness** (`position_<authority_id>`): the epoch, the
    PostgreSQL system identifier, timeline and flushed WAL position, as
    `<epoch>_<system>_<timeline>_<lsn>`;
  - the **regression latch** (`latch_<authority_id>`): a regression of the
    position a server saw, kept until a restore is declared or an operator
    acknowledges it, so that it stays refused after the server writes past
    the witnessed position again;
  - the **witnessed epoch** (`epoch_<authority_id>`): the epoch the position
    witness was last written under, which tells whether a restore was
    declared since a position witness or latch that cannot be read.

  `authority_id` is `host_grant_authority.authority_id`.

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
  clears any position acknowledgement, then closes every enrolled scope that
  is not retired or exhausted as `restored`, at its next generation, in
  batches of 256. It logs a warning, `declared a host-grant restore`, with the
  epoch and the number of restored scopes. A run that stopped after the first
  step is finished by the next run with the same epoch (`finished a
  host-grant restore transition ...`);
- with the recorded epoch, changes nothing, except that it binds the lineage
  the first time it runs after migration 2002;
- with an epoch below the recorded one, or against a database that holds a
  scope and was adopted without a raised epoch, is refused (exit status 4,
  `restore_fence`) and changes nothing.

## The fence every server passes

Before a server admits a message to a room of an enrolled scope, it verifies
the **restore fence**: when it starts, when a message finds the authority
changed (below), and about every five minutes while it runs. The server
starts whatever the fence finds; what it affects is managed rooms only.

When the fence refuses **the whole authority**, the server admits no message
to any managed room, logs an error `... are not verified; rooms of enrolled
scopes are unavailable on this replica` with `kind = "restore_fence"` and the
refusal below at most once a minute, sets the gauge
`waddle.host_grant.authority.unverified` to 1, and tries again with a backoff
of up to a minute. It stays ready (its readiness body says
`"host_grants": "unverified"`), because every server sees the same authority
and rooms no scope manages are still served. `epoch_mismatch` is the
exception: it is the server's own configuration, and the server is not
ready (see [Readiness](#readiness)). Alert on the gauge.

| Refusal | Meaning | What to do |
| --- | --- | --- |
| `epoch_mismatch` | The configured epoch is not the one the database records: the configuration predates or postdates the latest declared restore, or the database was restored to before it. | Correct the configuration, or follow the restore procedure. |
| `lineage_changed` | The database was adopted after a restore or a clone and the restore was not declared. | Follow the restore procedure. |
| `projection_absent` | Scopes admit, and SpiceDB holds no projection marker or room witness of a scope the database holds, and no position witness, latch or witnessed epoch of this authority: the servers are connected to a SpiceDB that is empty or another deployment's (another deployment's markers do not count, nor do journal witnesses). Nothing is closed and no position witness is written into it. | Point the servers at the deployment's SpiceDB; the refusal clears by itself. If that SpiceDB is lost, follow [What a restore can still lose](#what-a-restore-can-still-lose). |
| `position_regressed` | PostgreSQL is behind the position SpiceDB witnessed: on the same timeline it was rolled back in place or replaced by an older copy; on another timeline it was promoted or recovered below the witnessed position; or a position witness of a later declared epoch exists. The regression is latched (the `latch_<authority_id>` witness) and stays refused after the server writes past the witnessed position again. | Follow the restore procedure. A regression on the witnessed timeline may instead be acknowledged, if an operator has established that nothing was lost (see [After a promotion](#after-a-promotion)); one across timelines takes a declared restore. |
| `position_unverified` | The PostgreSQL timeline changed since the witness (a promotion, a point-in-time recovery) or the system did (another cluster); or SpiceDB holds a latch without its position witness; or the position witness, the latch or the witnessed epoch cannot be read. | See [After a promotion](#after-a-promotion). Another system, a latch without its witness and a witness that cannot be read take a declared restore: the first fence pass under the raised epoch removes what cannot be read (and the latch), and the pass after it writes a new position witness. Do not delete these witnesses by hand. |
| `position_unreadable` | The runtime role cannot run `pg_current_wal_flush_lsn()` or `pg_walfile_name()` (outside the attested pool, `pg_control_system()` as well). | Grant `EXECUTE` on the two WAL functions to the runtime role. Never grant `pg_control_system()` to a restricted runtime role: the physical attestor requires that it cannot run it, and on the attested pool the server takes the system identifier from the attested identity. |
| `authority_moved` | The authority changed while the fence ran, three times in a row. | Retried by itself; investigate if it persists. |

When the fence finds evidence about **one scope**, it closes that scope and
nothing else, logging `A host-grant scope was closed: ...` with the evidence:

| Evidence | Meaning |
| --- | --- |
| `projection_ahead` | SpiceDB's projection records an operation the database does not: the database lost operations. |
| `journal_ahead` | The journal witness records an operation the database does not, even one whose projection was never written, or one of an incarnation this database never held. |
| `unknown_incarnation` | The projection was written under an incarnation the database never knew: the database is older than a declared restore. |
| `marker_missing` | The scope admitted and SpiceDB, which holds this deployment's witnesses, holds no projection marker for it: SpiceDB lost it. |

The closed scope is `fenced`: it admits nothing, honours no initiator, is not
projected again and keeps the evidence in SpiceDB as it is. Only a declared
restore reopens it. For the first three kinds the scope is also marked as one
the restore **lost operations** in (`host_grant_scopes.lost_operations`),
which a reopen requires the operator to confirm (see
[Operator commands](#operator-commands)). Reconciliation closes a scope the
same way when it finds the same evidence.

When SpiceDB names a scope **the database does not hold** (the database lost
an enrollment), messages to that scope's rooms are refused with
`service-unavailable`, rather than served as rooms no scope manages, whether
or not the server's fence passed: the fence reads those rooms before it
decides anything else. Until a server's first fence pass has read them it
asks SpiceDB about the room of each such message (within half a second; an
answer that the room has no witness is kept for a minute); if SpiceDB does
not answer, the message is served as an unmanaged room, and the server stops
asking for a minute. Other rooms are unaffected.

A server that was already running when the database lost the enrollment
refuses its rooms from its next fence pass that reads SpiceDB: at most one
periodic pass (about five and a half minutes) after the restore, sooner when
a message to a managed room finds the authority changed and the fence runs
again. While SpiceDB does not answer no pass reads it, and the server keeps
serving those rooms as unmanaged. Stopping every server before a restore
(the [restore procedure](#restore-procedure)) avoids the window.

### While a server runs

A server binds itself to the authority the fence verified: the incarnation,
the epoch, the start of the PostgreSQL server, the database and the
authority relation. When a message to a managed room finds any of them
changed (a PostgreSQL restart or failover, a declared restore, a database
moved, relations recreated by a logical restore), that message is refused,
an error of kind `authority_changed` is logged at most once a minute, and
the server runs the fence again by itself. If it passes, the server admits
again with the new binding; no restart is needed. While it fails, managed
rooms stay refused and the server keeps trying; for a refusal of the whole
authority it stays ready.

A running server also runs the fence about every five minutes (with up to
half a minute of jitter). A restore that rewrites rows in place without a
restart is found by that pass: it closes the scopes that lost operations, or,
when it refuses the whole authority, drops the server's attachment until a
later pass succeeds. A pass that cannot complete (PostgreSQL or SpiceDB not
answering) keeps the attachment, up to three in a row.

### Readiness

`/readyz` follows the host-grant gate only while an unretired scope is
enrolled. A failure of **this server alone** answers HTTP 503 with
`"status": "not_ready"` and the cause in `host_grants`, and the server
leaves the endpoint set until it clears:

| `host_grants` | Cause | Clears when |
| --- | --- | --- |
| `configuration_predates_restore` | The configured epoch is below the recorded one: a restore was declared and this server was not restarted with the new configuration. | It is restarted with the current configuration. |
| `configuration_ahead_of_restore` | The configured epoch is above the recorded one: the owner phase has not declared the restore yet, or the configuration is wrong. | The owner phase runs with the same epoch, or the configuration is corrected. |
| `runtime_profile_mismatch` | The runtime role does not match the profile, or the store cannot be read as it requires. | The role is corrected and the server restarted. |
| `spicedb_unreachable` | Three verifications in a row, over at least 30 seconds, could not complete because SpiceDB did not answer. | A verification completes. |
| `fence_incomplete` | The same, failing on PostgreSQL (busy or not answering). | A verification completes. |

A refusal of the **whole authority** (the fence refusals above other than
`epoch_mismatch`) keeps the server ready with `"host_grants": "unverified"`,
since every server shares it. A deployment with no unretired scope reports
`"host_grants": "unmanaged"`; a verified server adds no `host_grants`
field. A SpiceDB outage that every server shares, and that outlasts the
grace, takes every server out of the endpoint set while a scope is
enrolled.

## Scope states

| State | Admits | How it leaves the state |
| --- | --- | --- |
| active | yes | — |
| `restored` | no | Reconciliation rebuilds and verifies its projection: `reconstructed`. |
| `reconstructed` | no | An operator reopens it (`waddle-server host-grant reopen`): active. One the restore lost operations in reopens only by name, with `--lost-operations-reapplied`. |
| `fenced` | no | A declared restore: `restored`. |
| `retired`, `exhausted` | no | Never. |

In this release no server process starts reconciliation on its own; it is
wired into the server together with the grant authority's XMPP handlers.
Until then a restored scope stays `restored`.

Query the states with:

```sql
SELECT s.closed_reason, s.lost_operations, count(*)
  FROM host_grant_scopes s, host_grant_authority a
 WHERE s.authority_incarnation = a.incarnation
 GROUP BY s.closed_reason, s.lost_operations;
```

A `NULL` reason is an active scope. Scopes bound to an earlier incarnation
are those a stopped transition has not reached yet; the next owner phase
finishes them.

## Operator commands

`waddle-server host-grant <action>` runs as the migrator login, with the
owner phase's connection environment (`WADDLE_DATABASE_URL`, and the schema
and role variables or flags), and can run while the servers run. Give it the
servers' SpiceDB as well (`WADDLE_SPICEDB_ENDPOINT`,
`WADDLE_SPICEDB_PRESHARED_KEY`, and `WADDLE_SPICEDB_INSECURE` as the servers
set it) so that the act is witnessed at once; without it, the next fence pass
of a server witnesses it, about five minutes later at most while a server is
attached. A restore of PostgreSQL before then loses the act without trace.

- `reopen [--scope <lifecycle> [--lost-operations-reapplied]]` reopens
  reconstructed scopes whose latest operation is verified (all of them
  without `--scope`) and prints how many it reopened, how many are not
  reconstructed, and how many it left closed because the restore lost
  operations in them. Such a scope reopens only by name, with
  `--lost-operations-reapplied`: the operator's confirmation that the
  revokes and bans the restore lost have been applied again with `revoke`.
- `acknowledge-position [--witnessed <stamp>]` acknowledges the position
  witness for the PostgreSQL system and timeline the server now writes on.
  `--witnessed` is the witness as SpiceDB holds it,
  `<epoch>_<system>_<timeline>_<lsn>`; with the servers' SpiceDB in the
  environment it can be left out and is read from SpiceDB. Without SpiceDB,
  read it with `zed relationship read waddle_grant_projection:position_<authority_id>`
  (the subject ID is the stamp). The acknowledgement applies only to that
  witness: for a later timeline of the same system whose position is not
  below the witness (a promotion), or to a regression on the witnessed
  timeline. It never applies to a regression across timelines or to another
  system, and one of a witness on a later timeline than the server's, of
  another system, or of an earlier timeline whose position the server's
  timeline is below (a regression across timelines), is refused. The
  command never moves the witness past a latched or regressed position; it
  says why when it does not. With SpiceDB the command applies it at once and
  removes it; otherwise the next fence pass applies it, and it is spent once
  the witness has moved.
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
   stays in force, and it marks the scope as one the restore lost operations
   in; it never grants anything SpiceDB holds and the database does not. A
   scope whose reconstruction reports a contradiction (`MarkerMissing`,
   `MalformedMarker`, `MalformedFamily`, `ContentMismatch`,
   `ReconstructionTooLarge`) stays `restored`. A scope that was never
   projected has nothing to lower its grants to; when its journal witness
   shows an operation the restored database does not record, it is marked
   as one the restore lost operations in all the same.
10. **Apply what SpiceDB cannot show.** A revoke or ban acknowledged after the
    restore point whose projection was never written is lost with the
    database; reconstruction cannot lower it. Scopes marked with lost
    operations (the query under [Scope states](#scope-states)) lost such
    operations or ones SpiceDB saw. Apply the revokes and bans you know of
    with `waddle-server host-grant revoke`, and retire again any scope
    retired after the restore point. The audit trail of the lost operations
    is in the lost database; use the application's other records and the
    people who made them.
11. **Reopen.** `waddle-server host-grant reopen` reopens every reconstructed
    scope the restore lost no operations in. Reopen each scope it lost
    operations in by name, once step 10 is done for it:
    `waddle-server host-grant reopen --scope <lifecycle>
    --lost-operations-reapplied`. A scope reopens only when its latest
    operation is verified; rerun after the next reconciliation pass if the
    command reports scopes not reconstructed.

## After an undeclared restore is detected

A server that closes scopes as `fenced`, or refuses the authority with
`lineage_changed` or `position_regressed`, is serving a database that was
restored without this procedure. Follow the [restore procedure](#restore-procedure)
from step 1, without restoring PostgreSQL again unless it must be: the
declaration (steps 6 and 7) is what turns `fenced` scopes into `restored`
ones, and clears the regression latch.

## After a promotion

A promotion of a replica (a planned switchover or a failover) and a
point-in-time recovery start a new PostgreSQL timeline. It also restarts the
PostgreSQL server: running servers notice it with the next message to a
managed room and run the fence again by themselves.

- If the promoted replica's position is below the witnessed one, servers
  refuse with `position_regressed`: commits the old primary had flushed are
  missing. That is a restore; follow the restore procedure from step 4.
- Otherwise servers refuse with `position_unverified` and refuse managed
  rooms; they stay ready. If the promoted replica had received every commit
  (synchronous replication, or a planned switchover after it caught up), run
  `waddle-server host-grant acknowledge-position` (with `--witnessed`, or
  with the servers' SpiceDB in the environment). The servers' next fence
  attempt passes and moves the witness to the new timeline.
- If you cannot establish that the promotion lost nothing, it is a restore:
  follow the restore procedure from step 4. Scopes whose lost operations
  SpiceDB saw are closed by the fence either way; the acknowledgement is your
  assertion that there are no others.

A server latches a regression on the witnessed timeline as well. If an
investigation shows that nothing acknowledged was lost (for example, WAL the
old primary had flushed but no commit depended on), the same
`acknowledge-position` command, bound to the latched witness, clears it.
Otherwise declare a restore.

## What a restore can still lose

- **Operations whose witnesses were not written yet.** A revoke or ban
  committed just before the restore point whose journal witness, position
  witness and projection had not been written leaves no trace outside the
  lost database; an undeclared restore that lost only such operations is not
  detected. The window is the lag of the witness writers: shortly after the
  commit for a change made by a server; for an operator's act without the
  servers' SpiceDB, until the next fence pass of a server (about five
  minutes while one is attached). Apply such operations again (step 10).
- **A regression nobody saw.** The latch is written when a server's fence
  pass sees the position below the witness. A restored server that writes
  past the witnessed position before any server compares them (a busy
  cluster restored from a crash-consistent snapshot, say) shows no
  regression; the scope evidence still applies. A latch SpiceDB did not take
  (the failure is counted in `waddle.host_grant.witness.write_failures`) is
  held by that server alone, which writes it again on every later fence
  pass. Until one succeeds it is lost when that server restarts, and it
  stops counting once another writer (another server's fence pass, a
  committed change, `host-grant` with the servers' SpiceDB) moves the
  position witness, which it does as soon as PostgreSQL has written past the
  witnessed position again.
- **What a lost operation changed.** The witnesses show that operations were
  lost, and the projection what it held; neither records what a lost,
  unprojected operation changed. The operator applies it again.
- **An enrollment whose projection was never written.** Nothing outside the
  database knows its rooms; they are served as rooms no scope manages. An
  enrollment whose projection was written is detected, and its rooms are
  refused; no tool in this release retires a scope the database no longer
  knows. Removing its marker and witnesses from SpiceDB by hand (with `zed`)
  returns its rooms to the admission of unmanaged rooms and is an explicit
  operator decision. While SpiceDB does not answer and a server has not yet
  read those rooms, they are served as unmanaged, and a server that was
  running when the enrollment was lost serves them until its next fence pass
  that reads SpiceDB (see [The fence every server passes](#the-fence-every-server-passes)).
- **SpiceDB lost or restored as well.** Servers pointed at an empty SpiceDB
  refuse with `projection_absent` and close nothing; with the right SpiceDB
  back, they serve again. If it is lost for good, declare a restore: scopes
  whose activated projection has no marker cannot be reconstructed
  (`MarkerMissing`), and this release has no tool to reopen them. A SpiceDB
  that lost some markers beside this deployment's witnesses closes the
  scopes without one (`marker_missing`). When PostgreSQL and SpiceDB were
  restored together to points that agree, nothing detects it: declare the
  restore, and review each scope before reopening it. A SpiceDB restored to
  an older point than PostgreSQL is not a PostgreSQL rollback: the servers
  serve, and reconciliation reports the older marker.
- **An asynchronous failover that lost commits** is a restore. See
  [After a promotion](#after-a-promotion).

## Rollout (G24)

- Migration 2002 adds to the authority row the restore epoch, the lineage
  binding, a fixed authority identity and the position acknowledgement; to
  the scope rows the lost-operations mark; the `restored`, `reconstructed`
  and `fenced` states; the `restore`, `reconstruct`, `fence` and `reopen`
  operation kinds; and an index on the operation journal. The owner phase
  applies it before any server of the release starts; a server of the
  previous release refuses a ledger that holds it. The first owner phase
  after it binds the authority to the database lineage and changes nothing
  else.
- `WADDLE_HOST_GRANT_RESTORE_EPOCH` is new. Unset, it is `0`, which is what
  the migration records, so a deployment that never declared a restore needs
  no configuration change. The owner phase and the servers must read the same
  value.
- `waddle-server host-grant` is a new operator subcommand; it reads
  `WADDLE_SPICEDB_*` when set.
- A server starts whatever the restore fence finds; a refusal leaves managed
  rooms unavailable on that server. Readiness fails, while an unretired scope
  is enrolled, only for a failure of that server alone (see
  [Readiness](#readiness)).
- Every server reads, when it attaches to the grant store and on every fence
  pass, every projection marker and witness in SpiceDB and the scope table.
  A deployment without an enrolled scope holds none and writes none. The
  built-in SpiceDB schema is unchanged.
- A server's acceptance of a message to a room of an enrolled scope also
  compares the authority it reads with the one it attached to. Rooms that no
  scope manages, and SQLite deployments, are unchanged, except that until a
  server's first fence pass has read the rooms of lost enrollments, a message
  to an unmanaged room may ask SpiceDB about that room.
