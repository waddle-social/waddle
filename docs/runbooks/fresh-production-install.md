# Fresh production install

This replaces the production `waddle-server` deployment and its databases
with empty ones and starts the current release under the restricted
PostgreSQL runtime at ingress protocol epoch 2. Every message, account,
room and permission in production is discarded. Use it only when that is
the decision; to keep the data, use the
[restricted runtime cutover](restricted-runtime-cutover.md) instead.

Why epoch 2 before serving: with two replicas, a stanza for a full JID bound
on the other replica is delivered live only at protocol epoch 2 ([direct
lanes, "Below protocol epoch
2"](../planning/2173-direct-lanes.md#below-protocol-epoch-2)). Only the
owner phase activates it, so production serves only after the owner Job has
run.

Markers: **[DESTRUCTIVE]** discards state; **[reversible]** is undone by
the stated command.

## What the GitOps directory deploys

`infrastructure/waddle.cloud/gitops/waddle-server/kustomization.yaml` lists
both restricted-runtime components:

- `restricted-runtime/prepare` pins the PostgreSQL image
  (`ghcr.io/cloudnative-pg/postgresql:17.11`), declares the six
  `waddle_*` roles as CloudNativePG managed roles, and renders the suspended
  owner CronJob `waddle-server-owner` with this database's cutover record.
- `restricted-runtime/serve` makes the server connect as `waddle_runtime`
  from the handoff, and sets `restrictedRuntime.handoff.awaitOwnerJob`.

With `awaitOwnerJob`, a release that finds no published handoff renders the
Deployment at zero replicas instead of failing (chart 0.4.13 or later), so
the first install creates the owner CronJob while no server runs. After the
owner Job publishes the handoff, the next upgrade starts the configured
two replicas. A Deployment that is already serving is never scaled down for
a missing handoff: that render fails instead.

How the empty database becomes the server's:

1. CloudNativePG creates database `waddle` owned by login `waddle`
   (`bootstrap.initdb`), the `postgresql-app` and `postgresql-ca` Secrets,
   and the managed roles with the passwords from the three
   `postgresql-waddle-*` Secrets.
2. `server/crates/waddle-server/sql/restricted_runtime_provisioning.sql`,
   run as the superuser, gives the database to `waddle_database_owner` and
   the schema to `waddle_owner`, grants CONNECT to the three logins only,
   creates `waddle_attestation` and restricts `pg_control_system()`. It
   accepts the logins CloudNativePG created.
3. The owner Job: `owner-phase` (as `waddle_migrator`) enrolls the lineage,
   builds the schema from the baseline (migration 2000; the default
   `WADDLE_DB_BASELINE_ACTION`, `stamp`, builds an empty database from the
   baseline and stamps only a database that holds the retired history),
   grants the runtime its privileges, enrolls the keypair pool and the peer
   allowlist and activates protocol epoch 2; `install-attestor` (as
   `waddle_attestor_installer`); `emit-handoff`, which also runs the
   server's pinned preflight as `waddle_runtime`; then the handoff is
   published as ConfigMap `waddle-server-runtime-handoff-pins`.
4. The server pods start from that handoff.

A fresh database has no stream-management rows to drain, no retired ledger
to stamp and no keypair-slot table of an older server, so the owner phase
runs without `--legacy-keypair-slots`.

## Before you start

- A kubeconfig for the production cluster. From
  `infrastructure/waddle.cloud`, signed in to the `waddle-social` 1Password
  account (`op signin --account waddle-social.1password.eu`; the command
  reads the Talos configuration and the Scaleway credentials from the
  `waddle-infra` vault):

  ```sh
  go run . cluster access --cluster production \
    --kubeconfig-out ~/.kube/waddle-production --talosconfig-out ~/.talos/waddle-production
  export KUBECONFIG=~/.kube/waddle-production
  ```

- `kubectl`, `flux`, `yq`, `gh` and `openssl` (the repository's `nix
  develop` shell has the first four).
- A checkout of the branch that carries this runbook, at the commit that
  will be merged.
- Nothing else merged to `main` that changes the server between the owner
  Job and the serving rollout: the owner Job must run the migrations of the
  image that serves.

Helpers used below:

```sh
PRIMARY() { kubectl -n waddle get pod -l cnpg.io/cluster=postgresql,cnpg.io/instanceRole=primary -o name; }
psql_primary() { kubectl -n waddle exec -i "$(PRIMARY)" -c postgres -- psql -v ON_ERROR_STOP=1 -d waddle "$@"; }
```

`psql` in the instance pod connects as the `postgres` superuser over the
local socket.

## 1. Stop reconciliation and the server

1. Suspend the Flux Kustomization that applies the directory, so nothing is
   recreated while it is wiped. **[reversible]**: `flux resume`.

   ```sh
   flux suspend kustomization infra-waddle-server -n flux-system
   ```

2. Uninstall the server. **[DESTRUCTIVE]** for its running sessions. The
   HelmRelease must not be suspended: helm-controller uninstalls only an
   unsuspended release when it is deleted.

   ```sh
   flux resume helmrelease waddle-server -n waddle 2>/dev/null || true
   kubectl -n waddle delete helmrelease waddle-server --wait --timeout=5m
   kubectl -n waddle wait --for=delete pod -l app.kubernetes.io/name=waddle-server --timeout=180s
   helm -n waddle list -a                     # waddle-server is gone
   ```

   If the deletion hangs, run `helm -n waddle uninstall waddle-server` and
   then remove the HelmRelease finalizer.

3. Remove what a release does not own: a handoff published for the old
   database, and owner Jobs.

   ```sh
   kubectl -n waddle delete configmap waddle-server-runtime-handoff-pins --ignore-not-found
   kubectl -n waddle delete job -l app.kubernetes.io/component=owner-job --ignore-not-found
   ```

## 2. Wipe the databases

Deleting the CloudNativePG `Cluster` is what resets roles as well as data:
roles are per PostgreSQL cluster, not per database. The PVCs and the
operator-generated Secrets (`postgresql-app`, `postgresql-ca`,
`postgresql-server`, `postgresql-replication`, `postgresql-superuser`) carry
owner references to the Cluster and are deleted with it.

1. **[DESTRUCTIVE]** The server database:

   ```sh
   kubectl -n waddle delete cluster postgresql --wait --timeout=10m
   kubectl -n waddle get pvc -l cnpg.io/cluster=postgresql        # none
   kubectl -n waddle get secret postgresql-app postgresql-ca      # NotFound
   ```

   Delete any PVC that is left with
   `kubectl -n waddle delete pvc -l cnpg.io/cluster=postgresql`.

2. **[DESTRUCTIVE]** SpiceDB's relationships describe the discarded rooms
   and accounts; wipe them too. Deleting the `SpiceDBCluster` makes the
   operator run the datastore migrations again on the new database.

   ```sh
   kubectl -n waddle delete spicedbcluster spicedb --wait --timeout=5m
   kubectl -n waddle delete cluster postgresql-spicedb --wait --timeout=10m
   kubectl -n waddle get pvc -l cnpg.io/cluster=postgresql-spicedb  # none
   ```

Uploaded files in the R2 bucket are not touched by this runbook.

## 3. Credentials

The managed roles take their passwords from three `kubernetes.io/basic-auth`
Secrets. They survive the wipe; create any that is missing. Passwords are
placed in connection URLs unescaped, so use hex.

```sh
for pair in migrator:waddle_migrator attestor-installer:waddle_attestor_installer runtime:waddle_runtime; do
  name="postgresql-waddle-${pair%%:*}" role="${pair#*:}"
  kubectl -n waddle get secret "$name" >/dev/null 2>&1 ||
    kubectl -n waddle create secret generic "$name" --type=kubernetes.io/basic-auth \
      --from-literal=username="$role" --from-literal=password="$(openssl rand -hex 24)"
done
kubectl -n waddle get secret postgresql-waddle-migrator postgresql-waddle-attestor-installer \
  postgresql-waddle-runtime waddle-clustering-keypool waddle-runtime-secrets waddle-xmpp-tls
```

Each existing Secret's `username` must be its role's name. All six Secrets
must exist.

## 4. Create the empty database

The Cluster is created from this branch's manifests now, before the merge,
because the cutover record needs the OID of its `waddle` login. Flux adopts
the object when the Kustomization resumes: the manifest and the field
manager are the ones it applies with.

1. Create both PostgreSQL clusters.

   ```sh
   kubectl kustomize infrastructure/waddle.cloud/gitops/waddle-server \
     | yq 'select(.kind == "Cluster")' \
     | kubectl apply --server-side --field-manager=kustomize-controller -f -
   kubectl -n waddle wait cluster/postgresql --for=condition=Ready --timeout=15m
   kubectl -n waddle wait cluster/postgresql-spicedb --for=condition=Ready --timeout=15m
   ```

2. Check the new cluster.

   ```sh
   psql_primary -Atc 'SHOW server_version'                  # 17.11
   kubectl -n waddle get cluster postgresql -o jsonpath='{.status.managedRolesStatus}'; echo
   psql_primary -c "SELECT rolname, rolcanlogin, rolinherit, rolsuper, rolcreatedb, rolcreaterole
                      FROM pg_roles WHERE rolname LIKE 'waddle\_%' ORDER BY 1"
   psql_primary -Atc "SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
                       WHERE n.nspname = 'public'"          # 0
   ```

   The managed roles status lists the six roles as reconciled. Every
   `waddle_*` role shows `rolinherit = f` and no elevated attribute; the
   three logins `rolcanlogin = t`, the three others `f`.

3. Record the OID of the bootstrap owner `waddle` in the cutover record.

   ```sh
   psql_primary -Atc "SELECT oid FROM pg_roles WHERE rolname = 'waddle'"
   ```

   Put it into `role_oid` in
   `infrastructure/waddle.cloud/gitops/waddle-server/restricted-runtime/prepare/helmrelease-owner-job.yaml`
   (it is 0 until then, which `emit-handoff` refuses), commit and push to
   the pull request.

4. Provision roles and ownership. The procedure checks everything before
   its first change and reports `nothing was changed` when it refuses; a
   second run changes nothing.

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
   psql_primary -Atc "SELECT pg_get_userbyid(datdba) FROM pg_database WHERE datname = 'waddle'"   # waddle_database_owner
   psql_primary -Atc "SELECT pg_get_userbyid(nspowner) FROM pg_namespace WHERE nspname = 'public'" # waddle_owner
   ```

## 5. Publish the release

1. Merge the pull request. With GitHub Actions enabled, the merge queue runs
   the checks and the push to `main` starts `waddle-server-default`. If the
   pull request was merged while Actions was disabled, start the workflow on
   `main` once Actions is enabled:

   ```sh
   gh workflow run waddle-server-default.yml --ref main
   ```

2. Wait for the run on the merge commit. `publishContainerImage` must
   succeed: it pushes `ghcr.io/waddle-social/waddle:sha-<short>`, chart
   0.4.13 and the GitOps artifact.

   ```sh
   gh run list --workflow waddle-server-default.yml --branch main --limit 3
   gh run watch <run-id> --exit-status
   flux reconcile source oci waddle-server -n flux-system
   flux get sources oci waddle-server -n flux-system        # revision ends in the merge commit's short SHA
   ```

## 6. Install

1. Resume the Kustomization. Flux adopts both Clusters, recreates the
   `SpiceDBCluster`, and installs the HelmRelease: the Deployment at zero
   replicas and the owner CronJob.

   ```sh
   flux resume kustomization infra-waddle-server -n flux-system
   flux get kustomizations infra-waddle-server -n flux-system
   flux get helmreleases waddle-server -n waddle
   kubectl -n waddle get helmrelease waddle-server -o jsonpath='{.status.history[0].chartVersion}'; echo   # 0.4.13 or later
   kubectl -n waddle get deployment waddle-server       # READY 0/0
   kubectl -n waddle get cronjob waddle-server-owner    # SUSPEND True
   kubectl -n waddle get spicedbcluster spicedb
   ```

   If the HelmRelease reports an older chart, run
   `flux reconcile helmrelease waddle-server -n waddle --with-source`.

2. Confirm no session uses the database. The owner phase refuses
   (`DatabaseNotExclusive`, exit 4) while any other client session is
   connected to `waddle`, superuser sessions included.

   ```sh
   psql_primary -c "SELECT pid, usename, application_name, backend_type FROM pg_stat_activity
                     WHERE datname = 'waddle' AND pid <> pg_backend_pid()"
   ```

   Expected: no rows. Do not use `--exclusive-database not-checked`.

## 7. Run the owner Job

```sh
JOB=owner-$(date +%Y%m%d%H%M%S)
kubectl -n waddle create job "$JOB" --from=cronjob/waddle-server-owner
kubectl -n waddle get pods -l job-name="$JOB" -w
kubectl -n waddle logs -l job-name="$JOB" --all-containers --prefix --tail=-1
kubectl -n waddle wait --for=condition=complete job/"$JOB" --timeout=15m
```

| Exit code | Meaning | Job behaviour |
|---|---|---|
| 3 from `owner-phase` | Prepared; keypair-pool generation pending until the logged activation time | A new pod after the backoff; expected once |
| 1 | Database or connection error, contended lock, or unknown outcome | A new pod after the backoff |
| 2 | Invalid configuration | The Job fails at once; fix the values, publish, create a new Job |
| 4 | A verifier refused the database, the plan or the cutover record (for example `role_oid` still 0); the log names the cause | The Job fails at once; fix the cause (a values change needs a merge and a publish), create a new Job |

Then check the result:

```sh
psql_primary -Atc "SELECT version, description FROM _migrations"                       # 2000|schema baseline
psql_primary -Atc "SELECT epoch, activated_at IS NOT NULL FROM public.ingress_protocol_epoch WHERE id = 1"  # 2|t
psql_primary -Atc "SELECT state FROM public.clustering_keypool WHERE singleton"          # active
kubectl -n waddle get configmap waddle-server-runtime-handoff-pins -o jsonpath='{.data.handoff\.json}' > handoff-$(date +%F).json
```

Keep the handoff copy with the change record.

## 8. Start the server

The HelmRelease is unchanged, so helm-controller does not upgrade it by
itself. Force the upgrade that renders the published handoff:

```sh
flux reconcile helmrelease waddle-server -n waddle --force
kubectl -n waddle rollout status deployment/waddle-server --timeout=10m
kubectl -n waddle get pods -l app.kubernetes.io/name=waddle-server -o wide   # 2/2 Running, Ready
```

## 9. Verify

```sh
kubectl -n waddle logs -l app.kubernetes.io/name=waddle-server --prefix --tail=-1 \
  | grep -E 'Restricted runtime (pinned read-only preflight passed|attached its attested serving pool)'
psql_primary -c "SELECT node_id, peer_id, pod_template_hash, expired, draining, heartbeat
                  FROM public.clustering_nodes WHERE NOT expired ORDER BY first_seen"
psql_primary -c "SELECT usename, count(*) FROM pg_stat_activity
                  WHERE datname = 'waddle' AND backend_type = 'client backend' GROUP BY 1"
psql_primary -Atc "SELECT count(*) FROM pg_class c JOIN pg_roles r ON r.oid = c.relowner
                    WHERE r.rolname = 'waddle_runtime'"                                  # 0
```

Expected: both log lines once per pod; two live `clustering_nodes` rows,
one per pod, with recent heartbeats; only `waddle_runtime` sessions besides
this `psql`; the runtime owns no relation.

Smoke checks from outside the cluster:

```sh
curl -fsS https://xmpp.waddle.social/health; echo
curl -fsS https://xmpp.waddle.social/ready; echo
# RFC 7395 upgrade: expect 101
curl -sS -o /dev/null -w '%{http_code}\n' --http1.1 --max-time 5 \
  -H 'Connection: Upgrade' -H 'Upgrade: websocket' -H 'Sec-WebSocket-Version: 13' \
  -H 'Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==' -H 'Sec-WebSocket-Protocol: xmpp' \
  https://xmpp.waddle.social/ws
# Stream features (needs websocat): expect <stream:features> with SASL mechanisms
printf '%s\n' '<open xmlns="urn:ietf:params:xml:ns:xmpp-framing" to="waddle.social" version="1.0"/>' \
  | timeout 5 websocat -n --protocol xmpp wss://xmpp.waddle.social/ws
```

Then sign in at https://waddle.chat with two Colony accounts (production
starts with no accounts; each is created at its first sign-in), send a
direct message between them, create a room, post to it and reload its
history. Repeat the direct message with each client reconnected until the
two are served by different pods (`kubectl -n waddle logs` shows which pod
accepted each stream): the message must arrive live, not only after
reconnecting.

## Retry and rollback

The data is disposable, so there is no restore. To start again from any
step, run sections 1 to 4 again with new UUIDs in
`restricted-runtime/prepare/helmrelease-owner-job.yaml` (`uuidgen` for each
of the nine) and the new `role_oid`, then sections 5 to 9.

Returning to the owner-as-runtime deployment (the server connecting as
`waddle` from `postgresql-app`) is not a rollback of this procedure:
provisioning took the database from `waddle`. It needs the components
removed from `kustomization.yaml` and the wipe in sections 1 and 2 again,
and leaves the server below epoch 2, where it may run only one replica.

## After the install

- The owner CronJob stays suspended. A later release whose migrations or
  owner preparation changed needs the owner Job again before its pods can
  attach, as described in [the cutover runbook's "After the
  cutover"](restricted-runtime-cutover.md#after-the-cutover).
- A release that changes nothing in the database needs no Job: the chart
  composes the handoff for the new release's Git SHA.
- The `waddle` login keeps its CloudNativePG-generated password in
  `postgresql-app` but cannot connect to `waddle`.
