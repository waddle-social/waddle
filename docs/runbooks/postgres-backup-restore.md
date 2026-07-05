# Runbook: Postgres backup & restore (CNPG → Cloudflare R2)

Covers the two CloudNativePG clusters in namespace `waddle`:

| Cluster | Database | Backup path (`serverName`) |
| --- | --- | --- |
| `postgresql` | `waddle` | `s3://waddle-postgres-backups/postgresql/` (`postgresql`) |
| `postgresql-spicedb` | `spicedb` | `s3://waddle-postgres-backups/postgresql-spicedb/` (`postgresql-spicedb`) |

Configuration lives in `infrastructure/waddle.cloud/gitops/waddle-server/`:

- `postgresql-cluster.yaml` / `spicedb-postgres-cluster.yaml` — `spec.backup.barmanObjectStore`
  with continuous WAL archiving (gzip), base-backup gzip compression, and a `30d`
  retention policy.
- `postgresql-scheduled-backup.yaml` — daily base backup at 02:00:00 UTC.
- `spicedb-scheduled-backup.yaml` — daily base backup at 02:30:00 UTC (staggered).
- `postgres-backup-credentials-external-secret.yaml` — R2 credentials synced from
  1Password item `server-runtime-production` (properties `r2-access-key-id` /
  `r2-secret-access-key`) into Secret `postgres-backup-r2-credentials`
  (keys `ACCESS_KEY_ID` / `SECRET_ACCESS_KEY`).

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
# WAL archiving status on the primary:
kubectl -n waddle exec postgresql-1 -- psql -c "SELECT archived_count, failed_count, last_archived_time, last_failed_time FROM pg_stat_archiver;"
```

A healthy cluster shows `Backup` CRs in phase `completed`, a non-empty
`firstRecoverabilityPoint`, and `pg_stat_archiver.failed_count` not growing.

## Full restore procedure (scratch namespace)

CNPG restores by bootstrapping a **new** cluster from the object store; it never
restores in place. Run drills into a scratch namespace so production is untouched.

1. Create the scratch namespace and copy the R2 credentials into it:

   ```bash
   kubectl create namespace waddle-restore-drill
   kubectl -n waddle get secret postgres-backup-r2-credentials -o yaml \
     | grep -v -E 'namespace:|resourceVersion:|uid:|creationTimestamp:|ownerReferences:' -A0 \
     | kubectl -n waddle-restore-drill apply -f -
   ```

   (Or create a second ExternalSecret targeting the scratch namespace.)

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
           destinationPath: s3://waddle-postgres-backups/postgresql/
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

   For spicedb, substitute `postgresql-spicedb` for the source name,
   `destinationPath`, and `serverName`.

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

## HITL checklist (follow-ups from issue #1159)

- [ ] Create the R2 bucket `waddle-postgres-backups` in the Cloudflare account
      if it does not exist yet (the operator does not create buckets).
- [ ] Verify the `server-runtime-production` R2 API token scope covers the new
      `waddle-postgres-backups` bucket (read **and** write). If it is scoped to
      `waddle-social-files` only, mint a dedicated backup token and add it to
      1Password (then point the ExternalSecret at the new properties).
- [ ] After Flux applies the change, confirm WAL archiving starts
      (`pg_stat_archiver`, `.status.firstRecoverabilityPoint`) and the first
      `Backup` CR (created by `immediate: true`) reaches phase `completed` for
      both clusters.
- [ ] Run the scratch-namespace restore drill above for both clusters and
      record the result.
- [ ] Wire a backup-failure alert in the alerts-as-code issue. There is no
      PrometheusRule/monitoring stack in this repo today (observability is
      Grafana Alloy OTLP push), so this is documentation only for now.
      Suggested signals from the CNPG metrics exporter:
      - `cnpg_collector_last_available_backup_timestamp` — alert when older
        than ~26h (a missed daily backup).
      - `cnpg_collector_pg_wal_archive_status` — alert on growing failed WAL
        archive count.

## NetworkPolicy caveat

Namespace `waddle` currently has **no** NetworkPolicy, so egress to R2 is not
blocked. If a default-deny policy is ever introduced there, backups will break
unless the Postgres pods are allowed egress to the world on TCP 443 plus DNS —
use the Cilium idiom already in the repo
(`infrastructure/waddle.cloud/gitops/livekit-sfu/networkpolicy.yaml`,
`toEntities: [world]`).
