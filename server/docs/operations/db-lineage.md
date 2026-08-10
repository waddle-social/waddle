# Database Lineage Attestation Runbook

This runbook covers database lineage attestation at readiness, first-time
enrollment, and operator-driven adoption after a clone or restore.

## What lineage attestation checks

At readiness time, each durable database pool validates one row in `_lineage`:

- Deployment UUID (`deployment_uuid`) from `WADDLE_DEPLOYMENT_UUID`
- Lineage UUID (`lineage_uuid`)
- PostgreSQL identity:
  - `pg_system_identifier`
  - `pg_database_oid`
  - `pg_database_name`
- Schema identity:
  - `pg_schema_oid`
  - `pg_schema_name`
- `format` (lineage table schema version)

Readiness fails if any attested value is missing, malformed, stale, or
mismatched. A clustered deployment also fails readiness when any durable store
uses SQLite instead of PostgreSQL.

The lineage table is created with bootstrap DDL outside the migration ledger and
versions itself by its `format` column. Existing installations that lack the table
start not-ready until the table is properly enrolled.

## First-time enrollment

Use this when a deployment first points at a durable database:

1. Close traffic / pause writes to the DB path as expected for your operation.
2. Set the stable operator-provided deployment UUID. In the Helm chart, set
   `deployment.uuid`; it renders as `WADDLE_DEPLOYMENT_UUID` and must never
   change across upgrades or rollbacks.
3. `kubectl scale` to a single replica (or otherwise isolate one pod) so only one
   process performs the action.
4. Set `WADDLE_DB_LINEAGE_ACTION=enroll` on one replica for one render.
5. Wait until readiness becomes `200` and confirms attestation.
6. Remove `WADDLE_DB_LINEAGE_ACTION` immediately (do not leave it set), then
   restore the normal replica count before reopening traffic.

When rolling out the lineage-aware binary to an existing installation, do
steps 2 and 4 in the SAME `helm upgrade` that carries the image bump: a
lineage-aware pod against an un-enrolled database stays alive but
permanently unready (it never promotes to serving and starts no background
janitors) until enrollment happens and the pod restarts. Enrollment is
idempotent, so applying `enroll` to every replica of that one rollout is
safe.

This binds the configured `WADDLE_DEPLOYMENT_UUID` into `_lineage` and records the
live PostgreSQL and schema identity.

## Physical clone / logical restore recovery (adoption)

After a physical clone or logical restore, use adoption to re-bind the deployment:

- Close traffic or scale to one replica.
- Set the stable `WADDLE_DEPLOYMENT_UUID` for this deployment.
- Set `WADDLE_DB_LINEAGE_ACTION=adopt=<expected-old-lineage-uuid>`. A
  deployment whose stores span several distinct databases/schemas (each with
  its own lineage UUID) lists every expected old UUID comma-separated:
  `adopt=<uuid-a>,<uuid-b>`.
- The action is one-shot and must be run manually.
- Adoption rotates exactly the boundaries whose current lineage UUID appears
  in the list (minting a fresh lineage UUID and re-capturing live identity);
  every other boundary is left untouched.
- Replaying the action is harmless: after a successful adoption (or a pod
  restart while the action is still rendered) the listed UUIDs match nothing
  and the server logs a warning per unmatched entry while starting normally.
  A restore that still NEEDS adoption keeps failing readiness on its own
  `identity_mismatch`, so an unmatched entry with a ready fleet means either
  "already adopted — remove the action" or a typo against a healthy
  database. Check the warning logs, then remove `WADDLE_DB_LINEAGE_ACTION`.
- Once attestation succeeds, remove `WADDLE_DB_LINEAGE_ACTION`, restore the
  normal replica count, and reopen traffic.

Important: this is operator-only and never automatic.

This does not use automatic drift repair; no automation should "auto-adopt" on
probe failure.

## PostgreSQL permission prerequisite

If the role cannot execute `pg_control_system()`, readiness reports
`system_identifier_unavailable`. Grant:

```sql
GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO <app role>;
```

Note: a `permission denied` on other objects (for example missing `SELECT`
on `_lineage`) is reported as a plain verification failure, not
`system_identifier_unavailable` — the grant above only fixes the
`pg_control_system()` case.

## Recovery after a failed startup attestation

A pod whose STARTUP attestation failed latches itself: it stays alive and
observable (liveness `200`, readiness `503` with per-store lineage detail),
but it can never promote to serving in that process — not even if the
underlying cause is fixed while it runs. After fixing the cause (enrollment,
grants, configuration), restart or roll the pod. Changing the configmap
(deployment UUID or lineage action) rolls pods automatically via the
checksum annotation.

## Physical-clone limitation (honest operator rule)

A same-name `pg_basebackup` clone is intentionally indistinguishable from inside
the database: it retains the PostgreSQL system identifier, OIDs, lineage row,
and deployment UUID. That means lineage cannot always detect a clone by itself,
so adoption discipline is the operator-level control for these cases.

## Startup refusal before migrations

The GLOBAL database's lineage is verified BEFORE schema migrations run. A
pod pointed at a database whose lineage row exists but does not verify for
this deployment (another deployment's database, an unadopted restore, or a
missing/incorrect `WADDLE_DEPLOYMENT_UUID`) exits at startup without
writing anything — including the append-only migration ledger — and the
log names the refusal. This surfaces as `CrashLoopBackOff` rather than a
not-ready pod; fix the DSN/UUID (or perform adoption) and roll. A database
with NO lineage row proceeds un-enrolled and is instead held not-ready by
the readiness gate.

## Clustering requirement

Clustered Waddle deployments require PostgreSQL. SQLite is unsuitable and fails
readiness in clustered mode.
