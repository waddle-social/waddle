# Restricted PostgreSQL runtime cutover

This moves the production `waddle-server` from serving as the login that
owns its database (`waddle`) to serving as a runtime login that holds no DDL
privilege. It is the coordinated cutover of decision D1 in
[the completion contracts](../planning/2046-completion-contracts.md): the
server is stopped for the whole window.

Background: [restricted runtime startup](../planning/p2-1-runtime-startup.md),
in particular its owner entrypoint and provisioning sections.

Status: the manifests and this procedure have been rendered locally
(`helm template`, `kustomize build`). They have not been applied to a
cluster, and the procedure has not been rehearsed end to end. Rehearse it on
a restored copy of production before the real window.

Markers used below:

- **[reversible]**: undone by the stated command.
- **[DESTRUCTIVE]**: discards state.
- **[IRREVERSIBLE]**: past this step, going back means restoring the backup.

## What is deployed, and what turns it on

| Piece | Location | Off until |
|---|---|---|
| Chart support (`restrictedRuntime.*`, owner CronJob) | `server/charts/waddle-server` ≥ 0.4.11 | `restrictedRuntime.ownerJob.enabled` / `restrictedRuntime.enabled` |
| Roles, PostgreSQL image pin, owner CronJob values | `infrastructure/waddle.cloud/gitops/waddle-server/restricted-runtime/prepare` | listed under `components:` in that directory's `kustomization.yaml` |
| Serving switch | `…/restricted-runtime/serve` | listed under `components:` |
| `restrictedRuntime.releaseGitSha` | written by the release automation next to `WADDLE_GIT_SHA` | always written; inert while both switches are off |

Names used below: namespace `waddle`, CloudNativePG cluster `postgresql`,
database `waddle`, schema `public`, HelmRelease and Deployment
`waddle-server`, owner CronJob `waddle-server-owner`. `$PRIMARY` is the
primary instance's pod:

```sh
PRIMARY=$(kubectl -n waddle get pod -l cnpg.io/cluster=postgresql,cnpg.io/instanceRole=primary -o name)
psql_primary() { kubectl -n waddle exec -i "$PRIMARY" -c postgres -- psql -v ON_ERROR_STOP=1 -d waddle "$@"; }
```

`psql` inside the instance pod connects as the `postgres` superuser over
the local socket; superuser access does not need to be enabled on the
cluster.

## Phase A: stage (no outage)

Everything in this phase is **[reversible]** by reverting the commit, except
that roles the operator created stay until dropped.

1. Confirm PostgreSQL 17. The keypair-slot retirement check fails closed on
   18.

   ```sh
   psql_primary -Atc 'SHOW server_version'
   ```

   Put the exact running version into `imageName` in
   `restricted-runtime/prepare/postgresql-cluster-roles.yaml`. A different
   image restarts the instances; the same one does not.

2. Create the three password Secrets in `waddle`, each of type
   `kubernetes.io/basic-auth` with `username` and `password`:
   `postgresql-waddle-migrator` (`waddle_migrator`),
   `postgresql-waddle-attestor-installer` (`waddle_attestor_installer`),
   `postgresql-waddle-runtime` (`waddle_runtime`). The passwords are placed
   in connection URLs unescaped: use only letters, digits, `-`, `.`, `_` and
   `~` (for example `openssl rand -hex 24`). No Secret value is stored in
   the repository; supply them the way the other runtime secrets are
   supplied.

3. Record the old login's OID and fill in the cutover record.

   ```sh
   psql_primary -Atc "SELECT oid FROM pg_roles WHERE rolname = 'waddle'"
   ```

   In `restricted-runtime/prepare/helmrelease-owner-job.yaml` replace every
   all-zero UUID with a fresh one (`uuidgen`, each different) and `role_oid`
   with that OID. Keep `keypoolGeneration` and `installationOperationId`:
   they are the same on every later run. If the old servers ran with a
   `WADDLE_CLUSTERING_LEASE_TTL_MS` above 30000, set `--legacy-ttl` to it.

4. Add the component to
   `infrastructure/waddle.cloud/gitops/waddle-server/kustomization.yaml` and
   merge. The release publishes chart 0.4.11 or later and the GitOps
   artifact.

   ```yaml
   components:
     - restricted-runtime/prepare
   ```

5. Verify after reconciliation. The server still runs as `waddle`.

   ```sh
   kubectl -n waddle get cronjob waddle-server-owner   # SUSPEND is True
   kubectl -n waddle get cluster postgresql -o jsonpath='{.status.managedRolesStatus}'
   psql_primary -c "SELECT rolname, rolcanlogin, rolinherit, rolsuper, rolcreatedb, rolcreaterole
                      FROM pg_roles WHERE rolname LIKE 'waddle\_%' ORDER BY 1"
   ```

   Every `waddle_*` role must show `rolinherit = f` and no elevated
   attribute; the three logins `rolcanlogin = t`, the three others `f`. The
   provisioning procedure refuses anything else.

From here until the serving switch is merged, merge nothing that changes
the server: the owner Job must run the migrations of the image that will
serve.

## Phase B: cutover window (outage)

1. Suspend reconciliation. **[reversible]**: `flux resume`.

   ```sh
   flux suspend helmrelease waddle-server -n waddle
   ```

   Also suspend the Flux Kustomization that applies this directory
   (`flux get kustomizations -A`), so the manifests do not change during
   the window.

2. Stop the server (D1 step 1). **[reversible]**: scale back up.

   ```sh
   kubectl -n waddle scale deployment/waddle-server --replicas=0
   kubectl -n waddle wait --for=delete pod -l app.kubernetes.io/name=waddle-server --timeout=180s
   ```

3. Take the rollback backup and wait until it has completed. This is the
   only way back after step 6.

   ```sh
   kubectl cnpg backup postgresql -n waddle
   kubectl -n waddle get backup
   ```

4. Disable the old login and end its backends (D1 steps 2 and 3).
   **[reversible]**: `ALTER ROLE waddle LOGIN`.

   ```sh
   psql_primary <<'SQL'
   ALTER ROLE waddle NOLOGIN;
   SELECT pg_terminate_backend(pid) FROM pg_stat_activity
    WHERE datname = 'waddle' AND usename = 'waddle';
   SQL
   ```

5. Assert that nothing of it remains (D1 step 4). Both counts must be 0.
   Repeat until they are; do not continue otherwise.

   ```sh
   psql_primary -Atc "SELECT
     (SELECT count(*) FROM pg_stat_activity WHERE datname = 'waddle' AND usename = 'waddle'),
     (SELECT count(*) FROM pg_prepared_xacts WHERE database = 'waddle')"
   ```

6. Provision roles and ownership. **[IRREVERSIBLE]**: this is the rollback
   boundary. The procedure checks everything before its first change and
   reports `nothing was changed` when it refuses; fix the cause and run it
   again.

   ```sh
   { cat <<'SQL'
   SET waddle.schema                   = 'public';
   SET waddle.old_login                = 'waddle';
   SET waddle.database_owner           = 'waddle_database_owner';
   SET waddle.schema_owner             = 'waddle_owner';
   SET waddle.migrator_login           = 'waddle_migrator';
   SET waddle.runtime_login            = 'waddle_runtime';
   SET waddle.attestor_installer_login = 'waddle_attestor_installer';
   SET waddle.attestor_definer_role    = 'waddle_attestor';
   SET waddle.attestor_schema          = 'waddle_attestation';
   SQL
     cat server/crates/waddle-server/sql/restricted_runtime_provisioning.sql
   } | psql_primary
   ```

   Use the file from the commit the serving image is built from.

7. Run the owner Job (D1 step 5). **[DESTRUCTIVE]**: with
   `--legacy-keypair-slots retire` it drops the old servers' keypair-slot
   table, including which node held which slot; an old image cannot lease a
   slot afterwards.

   ```sh
   JOB=owner-$(date +%Y%m%d%H%M%S)
   kubectl -n waddle create job "$JOB" --from=cronjob/waddle-server-owner
   kubectl -n waddle get pods -l job-name="$JOB" -w
   kubectl -n waddle logs -l job-name="$JOB" --all-containers --prefix --tail=-1
   ```

   The pod runs `owner-phase`, `install-attestor`, `emit-handoff` and then
   publishes the handoff. What to expect:

   | Exit code | Meaning | Job behaviour |
   |---|---|---|
   | 3 from `owner-phase` | Prepared; keypair-pool generation pending. The log prints the activation time | A new pod is started after the backoff; expected once on the first cutover |
   | 1 | Database or connection error, contended lock, or unknown outcome | A new pod is started after the backoff |
   | 2 | Invalid configuration | The Job fails at once; fix the values |
   | 4 | A verifier refused the database or the plan. The log names the relation, privilege, lease or sessions | The Job fails at once; fix the cause, then create a new Job |

   Wait for the Job to complete:

   ```sh
   kubectl -n waddle wait --for=condition=complete job/"$JOB" --timeout=15m
   ```

   If it fails with `DatabaseNotExclusive`, a session is still connected:
   the log lists it. Do not use `--exclusive-database not-checked`.

   `owner-phase` also drains every XEP-0198 session the old server left in
   `sm_sessions` and `sm_unacked` without a row in `sm_durable_sessions`.
   The new server resumes only sessions of the durable session authority,
   so each such session is moved once to offline storage
   (`pending_delivery`) and its rows are deleted, in one transaction per
   session. When it moved any, the log prints `moved stream-management
   sessions without a durable record to offline storage` with the counts.
   What clients see:

   - A client whose session was detached or live when the server stopped
     cannot resume it. Its `<resume/>` is answered with `<failed/>` and it
     binds afresh; this costs one resume per client at the cutover.
   - Every message the old session retained unacknowledged is delivered to
     the account through offline delivery on its next bind, once. A message
     that was already an offline row the session had claimed is released
     back to offline delivery and is not stored a second time; an offline
     row the client had acknowledged is deleted.
   - Retained `<iq/>` and `<presence/>` frames, and messages offline storage
     does not keep (for example chat states only, or from a sender the
     account blocks), are not delivered, as when an unresumed session
     expires with no other resource online.

   The drain is safe to repeat: a rerun of the Job finds no such session
   and changes nothing. To see what is left before or after it:

   ```sh
   psql_primary -Atc "SELECT count(*) FROM (SELECT stream_id FROM sm_sessions
                        UNION SELECT stream_id FROM sm_unacked) l
                       WHERE NOT EXISTS (SELECT 1 FROM sm_durable_sessions d
                                          WHERE d.session_id = l.stream_id)"
   ```

   Expected after the Job: `0`.

8. Confirm the handoff was published and keep a copy with the change
   record.

   ```sh
   kubectl -n waddle get configmap waddle-server-runtime-handoff-pins -o jsonpath='{.data.handoff\.json}'
   ```

9. Switch the server to the runtime login (D1 step 6). Merge:

   ```yaml
   components:
     - restricted-runtime/prepare
     - restricted-runtime/serve
   ```

   and remove `ownerPhaseExtraArgs` from
   `restricted-runtime/prepare/helmrelease-owner-job.yaml`. Once the release
   has published, resume:

   ```sh
   flux resume helmrelease waddle-server -n waddle
   ```

   and resume the Flux Kustomization suspended in step 1. The chart composes
   the handoff for the new release's Git SHA from the published ConfigMap
   and restores the replica count.

## Verification

```sh
kubectl -n waddle rollout status deployment/waddle-server
kubectl -n waddle logs deployment/waddle-server | grep -E 'Restricted runtime (pinned read-only preflight passed|attached its attested serving pool)'
psql_primary -c "SELECT usename, count(*) FROM pg_stat_activity
                  WHERE datname = 'waddle' AND backend_type = 'client backend' GROUP BY 1"
psql_primary -Atc "SELECT count(*) FROM pg_class c JOIN pg_roles r ON r.oid = c.relowner
                    WHERE r.rolname = 'waddle_runtime'"
psql_primary -Atc "SELECT state FROM public.clustering_keypool WHERE singleton"
```

Expected: both log lines on every pod; only `waddle_runtime` sessions
(besides this `psql`); the runtime owns 0 relations; the keypool state is
`active`. Then sign in with a client, send a direct message, open a room
and read its history.

A pod that refuses to start prints the reason: a handoff for another release
(`release identity differs from WADDLE_GIT_SHA`), an endpoint that differs
from the cutover record, or a store that failed its attachment. It does not
fall back to the old login.

## Rollback

| Reached | Rollback |
|---|---|
| Phase A only | Revert the commit. Drop the `waddle_*` roles if they should not stay |
| Phase B steps 1–5 | `ALTER ROLE waddle LOGIN`, scale the Deployment back up, resume Flux. Nothing else changed |
| Phase B step 6 or later | **Restore.** There is no reverse script |

Restore procedure after step 6: recover the CloudNativePG cluster from the
backup taken in step 3 (a physical backup; it carries roles, memberships,
ownership and ACLs, which a logical dump of the database does not), revert
the `components:` entries, make sure `waddle` can log in, and let Flux
deploy the previous release. Every write accepted after step 2 is lost;
with the server stopped for the window that is only what the new version
accepted after step 9.

## After the cutover

- The old login `waddle` stays `NOLOGIN`. Rotate or remove its credential
  once the rollback window has closed.
- `restricted-runtime/prepare` and `serve` stay enabled. The owner CronJob
  stays suspended.
- A later release whose migrations or owner preparation changed needs the
  owner Job again, with the server at zero replicas, before its pods can
  attach: suspend, scale to zero, create a Job from the CronJob of the new
  release, resume. This is manual; nothing in the chart sequences it.
- A release that changes nothing in the database needs no Job: the chart
  recomposes the handoff with the new `release_git_sha`.
- If a pinned object is ever recreated, pods refuse to start. Run the owner
  Job again to publish a fresh handoff.
