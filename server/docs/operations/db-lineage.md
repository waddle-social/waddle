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

This binds the configured `WADDLE_DEPLOYMENT_UUID` into `_lineage` and records the
live PostgreSQL and schema identity.

## Physical clone / logical restore recovery (adoption)

After a physical clone or logical restore, use adoption to re-bind the deployment:

- Close traffic or scale to one replica.
- Set the stable `WADDLE_DEPLOYMENT_UUID` for this deployment.
- Set `WADDLE_DB_LINEAGE_ACTION=adopt=<expected-old-lineage-uuid>`.
- The action is one-shot and must be run manually.
- Adoption is atomic: it verifies the current lineage UUID and mints a new
  lineage UUID before rebinding the deployment and refreshing all live identity
  fields.
- Once attestation succeeds, remove `WADDLE_DB_LINEAGE_ACTION`, restore the
  normal replica count, and reopen traffic.

Important: this is operator-only and never automatic.

This does not use automatic drift repair; no automation should "auto-adopt" on
probe failure.

## PostgreSQL permission prerequisite

If the role cannot read catalog identity, readiness reports
`SystemIdentifierUnavailable`. Grant:

```sql
GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO <app role>;
```

Then rerun readiness/probe.

## Physical-clone limitation (honest operator rule)

A same-name `pg_basebackup` clone is intentionally indistinguishable from inside
the database: it retains the PostgreSQL system identifier, OIDs, lineage row,
and deployment UUID. That means lineage cannot always detect a clone by itself,
so adoption discipline is the operator-level control for these cases.

## Clustering requirement

Clustered Waddle deployments require PostgreSQL. SQLite is unsuitable and fails
readiness in clustered mode.
