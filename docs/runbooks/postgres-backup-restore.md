# Runbook: Postgres backup & restore (CNPG → Cloudflare R2)

Covers the two CloudNativePG clusters in namespace `waddle`:

| Cluster | Database | `serverName` | Objects live under |
| --- | --- | --- | --- |
| `postgresql` | `waddle` | `postgresql` | `s3://waddle-postgres-backups/postgresql/{base,wals}/` |
| `postgresql-spicedb` | `spicedb` | `postgresql-spicedb` | `s3://waddle-postgres-backups/postgresql-spicedb/{base,wals}/` |

Both clusters share `destinationPath: s3://waddle-postgres-backups/`; barman
appends `serverName` as the prefix, which is what keeps the two clusters from
clobbering each other.

Configuration lives in `infrastructure/waddle.cloud/gitops/waddle-server/`:

- `postgresql-cluster.yaml` / `spicedb-postgres-cluster.yaml` — `spec.backup.barmanObjectStore`
  with continuous WAL archiving (gzip), base-backup gzip compression, and a `30d`
  retention policy.
- `postgresql-scheduled-backup.yaml` — daily base backup at 02:00:00 UTC.
- `spicedb-scheduled-backup.yaml` — daily base backup at 02:30:00 UTC (staggered).
- `postgres-backup-credentials-external-secret.yaml` — R2 credentials synced from
  the dedicated 1Password item `postgres-backup-r2` (properties `access-key-id` /
  `secret-access-key`) into Secret `postgres-backup-r2-credentials`
  (keys `ACCESS_KEY_ID` / `SECRET_ACCESS_KEY`). Deliberately a separate item
  from the app's `server-runtime-production` R2 keys — see the HITL steps.

Object storage is Cloudflare R2 via its S3-compatible API, endpoint
`https://f90cc3950ab5b356ec869fe64c867ea7.r2.cloudflarestorage.com`, dedicated
bucket `waddle-postgres-backups` (do not reuse `waddle-social-files`). R2 does
not support S3 object tagging — keep the `barmanObjectStore` config minimal
(no `tags`/`historyTags`).

## Verifying backups are healthy

```bash
kubectl -n waddle get scheduledbackups,backups
kubectl -n waddle get cluster postgresql -o jsonpath='{.status.firstRecoverabilityPoint}{"\n"}{.status.lastSuccessfulBackup}{"\n"}'
kubectl -n waddle get cluster postgresql-spicedb -o jsonpath='{.status.firstRecoverabilityPoint}{"\n"}{.status.lastSuccessfulBackup}{"\n"}'
# WAL archiving status on the primary (discover it by label — the primary
# is not pinned to postgresql-1 and moves on failover/switchover):
PRIMARY=$(kubectl -n waddle get pod -l cnpg.io/cluster=postgresql,cnpg.io/instanceRole=primary -o jsonpath='{.items[0].metadata.name}')
kubectl -n waddle exec "$PRIMARY" -- psql -c "SELECT archived_count, failed_count, last_archived_time, last_failed_time FROM pg_stat_archiver;"
```

A healthy cluster shows `Backup` CRs in phase `completed`, a non-empty
`firstRecoverabilityPoint`, and `pg_stat_archiver.failed_count` not growing.

## WAL-buildup hazard and emergency rollback

Once `spec.backup.barmanObjectStore` is applied, Postgres retains every WAL
segment until it is archived to R2. If archiving fails persistently (bucket
missing, credential scope wrong, R2 unreachable), WAL accumulates without
bound and eventually fills the volume it lands on. Both clusters isolate WAL
on a dedicated 2Gi `walStorage`, so the buildup fills that volume (and PANICs
the primary) rather than the data PVC — contained, but still an outage until
archiving recovers or the backup config is rolled back. The Cluster CR stays
`Ready` while this happens — only the
`ContinuousArchiving` status condition flips — so it will not show up as a
Flux health failure. Watch `pg_stat_archiver.failed_count` after any change
to the backup config.

Emergency rollback: remove `spec.backup` from the affected Cluster manifest
(revert the commit) and let Flux reconcile — the `archive_command` returns to
a no-op and Postgres recycles the retained WAL. No restart is needed; this is
a config reload.

## Full restore procedure (scratch namespace)

CNPG restores by bootstrapping a **new** cluster from the object store; it never
restores in place. Run drills into a scratch namespace so production is untouched.

1. Create the scratch namespace and copy the R2 credentials into it. Prefer a
   second ExternalSecret (copy
   `postgres-backup-credentials-external-secret.yaml` with
   `metadata.namespace: waddle-restore-drill`, applied by hand — do not add it
   to gitops). For a quick manual copy instead:

   ```bash
   kubectl create namespace waddle-restore-drill
   kubectl -n waddle get secret postgres-backup-r2-credentials -o json \
     | jq 'del(.metadata.namespace, .metadata.uid, .metadata.resourceVersion,
               .metadata.creationTimestamp, .metadata.ownerReferences,
               .metadata.managedFields)' \
     | kubectl -n waddle-restore-drill apply -f -
   ```

   (The plain `-o yaml | grep -v ...` trick does not work here: the secret is
   owned by an ExternalSecret and carries a multi-line `ownerReferences`
   block that line-filtering corrupts.)

2. Apply a recovery cluster. `externalClusters[].name` and the
   `barmanObjectStore.serverName` must match the **source** cluster's
   `serverName` (`postgresql` or `postgresql-spicedb`):

   ```yaml
   apiVersion: postgresql.cnpg.io/v1
   kind: Cluster
   metadata:
     name: postgresql-restore
     namespace: waddle-restore-drill
   spec:
     instances: 1
     storage:
       size: 10Gi
       storageClass: openebs-mayastor
     bootstrap:
       recovery:
         source: postgresql
     externalClusters:
       - name: postgresql
         barmanObjectStore:
           destinationPath: s3://waddle-postgres-backups/
           endpointURL: https://f90cc3950ab5b356ec869fe64c867ea7.r2.cloudflarestorage.com
           serverName: postgresql
           s3Credentials:
             accessKeyId:
               name: postgres-backup-r2-credentials
               key: ACCESS_KEY_ID
             secretAccessKey:
               name: postgres-backup-r2-credentials
               key: SECRET_ACCESS_KEY
           wal:
             compression: gzip
   ```

   For spicedb, name the restore cluster `postgresql-spicedb-restore` and
   substitute `postgresql-spicedb` for the `externalClusters[].name`,
   `bootstrap.recovery.source`, and `serverName` (the `destinationPath` is
   the same shared bucket root). Restore clusters carry no `spec.backup`, so
   they never write back to the archive.

3. Watch the restore:

   ```bash
   kubectl -n waddle-restore-drill get cluster postgresql-restore -w
   kubectl -n waddle-restore-drill logs -l cnpg.io/cluster=postgresql-restore -f
   ```

### Point-in-time recovery (PITR)

Add a `recoveryTarget` to the `recovery` stanza. Without one, CNPG recovers to
the end of the archived WAL (latest possible point).

```yaml
   bootstrap:
     recovery:
       source: postgresql
       recoveryTarget:
         targetTime: "2026-07-05 01:30:00+00"
```

Other selectors: `targetLSN`, `targetXID`, `targetName` (requires a prior
`pg_create_restore_point`), plus `exclusive` to control boundary inclusion.
The target must lie between `firstRecoverabilityPoint` and the last archived WAL.

### Verification queries

```bash
kubectl -n waddle-restore-drill exec postgresql-restore-1 -- psql -d waddle -c "\dt"
kubectl -n waddle-restore-drill exec postgresql-restore-1 -- psql -d waddle -c "SELECT now(), pg_is_in_recovery();"
# Spot-check row counts against expectations, e.g.:
kubectl -n waddle-restore-drill exec postgresql-restore-1 -- psql -d waddle -c "SELECT count(*) FROM mam_messages;"
# For spicedb:
kubectl -n waddle-restore-drill exec postgresql-spicedb-restore-1 -- psql -d spicedb -c "SELECT count(*) FROM relation_tuples;"
```

`pg_is_in_recovery()` must return `f` once the restored cluster has been
promoted (CNPG promotes automatically when the recovery target is reached).

Tear down when done: `kubectl delete namespace waddle-restore-drill`.

## HITL checklist (issue #1159)

**Blocking — complete BEFORE merging/applying the backup config.** Applying
`spec.backup` without a working bucket + credentials starts the WAL-buildup
countdown described above.

- [ ] Create the R2 bucket `waddle-postgres-backups` in the Cloudflare account
      (the operator does not create buckets).
- [ ] Mint a dedicated R2 API token scoped to `waddle-postgres-backups`
      (read **and** write) and store it in the 1Password item
      `postgres-backup-r2` as properties `access-key-id` / `secret-access-key`
      (the item the ExternalSecret already references — no manifest edit
      needed). Do not reuse the app-runtime token: even if its scope
      happens to cover the new bucket, a compromised app key must not be able
      to delete backups (R2 has no object lock).

**Post-merge follow-ups.**

- [ ] After Flux applies the change, confirm WAL archiving starts
      (`pg_stat_archiver`, `.status.firstRecoverabilityPoint`) and the first
      `Backup` CR (created by `immediate: true`) reaches phase `completed` for
      both clusters.
- [ ] Load the backup-failure alert rules into Grafana Cloud (see
      [Backup-failure alerting](#backup-failure-alerting) below).

## Backup-failure alerting

A silent backup failure is only discovered when a restore is needed, so
backup health is alerted on as code:

- **Metrics pipeline (reconciled by Flux):**
  `gitops/grafana-alloy/helmrelease.yaml` scrapes each CNPG instance pod's
  built-in exporter (`:9187/metrics`, `job="cnpg"`, `cnpg_cluster` label
  preserved) and forwards it through the existing OTLP path to Grafana
  Cloud. Without this the backup-health series never leave the cluster.
  (This repo has no prometheus-operator / `PrometheusRule` CRDs, so an
  in-cluster rule would not reconcile — the rules evaluate in Grafana
  Cloud, where the metrics land.)
- **Alert rules (as code):**
  `infrastructure/waddle.cloud/grafana-cloud/alerts/postgres-backups.yaml`
  is the source of truth — three rules: `CNPGBackupStale` (no base backup
  in >26h), `CNPGWALArchiveFailing` (WAL not reaching R2), and
  `CNPGBackupMetricsMissing` (the metrics pipeline itself broke, so the
  other two have no data to fire on).

Apply the rules to Grafana Cloud (HITL — needs a Grafana Cloud ruler token,
not held in 1Password yet):

- [ ] Confirm the CNPG metric names against the deployed operator
      (`kubectl -n waddle exec "$(kubectl -n waddle get pod -l cnpg.io/cluster=postgresql,cnpg.io/instanceRole=primary -o jsonpath='{.items[0].metadata.name}')" -- curl -s localhost:9187/metrics | grep -E 'backup|archiver'`)
      and adjust the rule file if they differ from CNPG 1.25.
- [ ] Load the rule group into the Grafana Cloud Mimir ruler
      (`mimirtool rules load --address=<prom-url> --id=<tenant> …/postgres-backups.yaml`)
      or provision the equivalent Grafana-managed rules from the same file.
- [ ] Point the rules' notification policy at an operator-visible contact
      point (the routing lives in Grafana Cloud, not this repo).

## Restore validation (recoverability gate)

**A backup is not proven until a restore has succeeded.** A full
scratch-namespace restore drill (per [Full restore procedure](#full-restore-procedure-scratch-namespace)
above) MUST be completed for **both** clusters before this backup
configuration is trusted as the production recovery path, and re-run at
least quarterly. Record each drill here:

| Date (UTC) | Cluster | Backup restored (recovery target) | Row/table spot-check result | Operator |
| --- | --- | --- | --- | --- |
| _pending_ | `postgresql` | | | |
| _pending_ | `postgresql-spicedb` | | | |

> ⚠️ Not yet validated. These drills require a live CNPG operator and the
> R2 bucket (which is created as part of the blocking pre-merge steps
> above), so they cannot be run until rollout. Do not treat backups as a
> recovery guarantee until this table is filled in with a successful drill
> for each cluster.

## NetworkPolicy caveat

Namespace `waddle` currently has **no** NetworkPolicy, so egress to R2 is not
blocked. If a default-deny policy is ever introduced there, backups will break
unless the Postgres pods are allowed egress to the world on TCP 443 plus DNS —
use the Cilium idiom already in the repo
(`infrastructure/waddle.cloud/gitops/livekit-sfu/networkpolicy.yaml`,
`toEntities: [world]`).
