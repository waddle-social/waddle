# Federation release rollout (#2047)

This runbook takes the work of issue #2047 (open XMPP and Spaces
federation) into `main` and onto production in one planned window, with
server-to-server federation (S2S) left off. Main deploys automatically, so
the merge is the deployment
([ADR-0020](../../server/docs/adrs/0020-independent-community-federation.md)
D11). The release changes behaviour even with S2S off: it needs an empty
SpiceDB store, it applies thirteen forward-only migrations, and it changes
what clients see in parsing, routing, permissions and the web client. This
document is the rollout gate G24 of the ADR; each part it collects has its
own runbook, linked where it applies.

Turning S2S on is not part of this rollout. It is the separate activation
decision G22 (see [Activation is out of scope](#activation-is-out-of-scope)).
No host-grant scope is enrolled by this rollout either.

These instructions do not authorize or assert a merge, a deployment, a store
deletion or a rollback.

## Contents

- [What the release carries](#what-the-release-carries)
- [Prerequisites](#prerequisites)
- [Before the window](#before-the-window)
- [The window](#the-window)
- [S2S stays off](#s2s-stays-off)
- [Old binaries and mixed versions](#old-binaries-and-mixed-versions)
- [Stores, migrations and startup checks](#stores-migrations-and-startup-checks)
- [What users see with S2S off](#what-users-see-with-s2s-off)
- [Monitoring](#monitoring)
- [Rollback](#rollback)
- [After the window](#after-the-window)
- [Activation is out of scope](#activation-is-out-of-scope)
- [Gaps to settle before the window](#gaps-to-settle-before-the-window)

## What the release carries

Slice names are those of [the delivery plan](../planning/federation-2047.md#slices).

| Part | Slices | Changes with S2S off | Its own runbook or record |
| --- | --- | --- | --- |
| Authorization ID codec | W1 | Every SpiceDB ID is written in the `v2_` encoding; a populated store without the codec marker is refused, so the store must be emptied and every grant except the deployment's own is lost | [Codec cutover](authz-codec-cutover.md) |
| Authorization schema versions and outcast precedence | W5o, W5b | Schema version 2 is recorded; a room ban (`outcast`) removes every channel permission and the author's own message edit and delete; a binary with an older schema can no longer write the schema | [Schema versions](authz-schema-versions.md) |
| Identity profile `waddle-identity-v1` | W5i | Client stanzas, sign-in, provisioning and lookups take addresses exactly instead of folding them; configured domains and owner entries must be canonical | [Identity profile rollout](identity-profile-rollout.md) |
| Destination routing | W2, W5a | A client stanza to a domain the deployment does not serve is classified before any handler and refused with `remote-server-not-found` | [Routing design, "Behaviour with S2S off"](../../server/docs/planning/s2s-routing-2047.md#behaviour-with-s2s-off) |
| Client identity | W3, W8 | The web client keys room state by room address, and stops joining rooms on other domains | [Routing design, web client list](../../server/docs/planning/s2s-routing-2047.md#behaviour-with-s2s-off) |
| Host-grant authority | W5b, W5c, W5r, W5d, W5e | New tables, an in-memory enrolled set every replica loads before it serves, a restore fence, a revocation fence and the retired-account ledger; with no scope enrolled, rooms keep their existing checks | [Restore](authz-host-grant-restore.md), [reassignment](authz-host-grant-reassignment.md), [account retirement](account-retirement.md) |
| S2S transport, routing, rooms, account traffic and Spaces | W4a–W4d, W5a, W6a–W6c, W6d, W7a, W7b | Nothing reaches or leaves the deployment over S2S; empty tables, new room and Space configuration fields, and the changes listed under [What users see](#what-users-see-with-s2s-off) | The design documents in `server/docs/planning/` |
| Advertisement and default admission mode | W11, W12 | W11 advertises only what verification demonstrated, and at run time only where S2S is on; W12 changes the default admission mode, which is not read while S2S is off | Their own reviewed changes |

## Prerequisites

Each is a go or no-go item. Record the answer for each in the change record
of the window.

1. **Go-ahead.** The owner has given an explicit go-ahead naming the window
   (start, end, who operates, who decides a rollback). Without it, stop.
2. **Verification and review.** Verification steps 1 to 10 of
   [the plan](../planning/federation-2047.md#verification-phase) are recorded
   in the [evidence log](../evidence/2047/README.md), the close-out review of
   the integrated head returned CLEAN, and W11 and W12 are merged into the
   integration branch with steps 1, 2 and 5 run again.
3. **Hosted CI at the exact head.** The integration branch is rebased on the
   current `main`, pushed, and every hosted check passed at the commit that
   will be merged. Record that commit's SHA. If `main` moves afterwards, the
   rebase and the hosted run are repeated; never merge a head whose checks ran
   on another commit.
4. **Nothing else in flight.** No other change that rebuilds the server image
   merges between the go-ahead and the end of the window: the owner phase
   must run the migrations of the image that serves.
5. **Production posture.** Read from the cluster and from
   `infrastructure/waddle.cloud/gitops/waddle-server/`:
   - whether the server serves under the restricted PostgreSQL runtime (the
     directory's `kustomization.yaml` lists `restricted-runtime/prepare` and
     `restricted-runtime/serve`, and the pods connect as `waddle_runtime`) or
     as the login that owns the schema. The window has a branch for each;
   - under the restricted runtime: the chart is 0.4.13 or later and the
     values set `restrictedRuntime.handoff.awaitOwnerJob` (the `serve`
     component does on `main`). Window steps 7 and 8 rely on both; the
     integration branch has them only once rebased on `main`;
   - the name of the Flux Kustomization that applies the `waddle-server`
     directory, and that the HelmRelease's values come from it (the
     published artifact sets `image.digest` and `WADDLE_GIT_SHA`);
   - the update strategy is `Recreate`, and the deployment UUID
     (`deployment.uuid` in the HelmRelease) is the one production has used;
   - the replica count, and that no other process writes SpiceDB
     relationships with the server's credentials (operator tools, a second
     deployment, a test job).
6. **PostgreSQL 17.** The release's PostgreSQL code and its focused sets were
   exercised on PostgreSQL 17 (17.11). Confirm the running major version:

   ```sh
   PRIMARY=$(kubectl -n waddle get pod -l cnpg.io/cluster=postgresql,cnpg.io/instanceRole=primary -o name)
   psql_primary() { kubectl -n waddle exec -i "$PRIMARY" -c postgres -- psql -v ON_ERROR_STOP=1 -d waddle "$@"; }
   psql_primary -Atc "SELECT version()"
   ```

7. **SpiceDB 1.56.1.** The SpiceDB evidence ran against SpiceDB 1.56.1. The
   `SpiceDBCluster` follows the operator's `stable` channel, so the running
   version is not fixed by the manifests: read it from the `SpiceDBCluster`'s
   status or the SpiceDB pod's image, and stop if it is not a version the
   evidence covers. Use a `zed` build that matches it for every command
   below.
8. **Clocks.** Every node that runs a server and both PostgreSQL clusters'
   instances synchronise their clocks (NTP; on Talos, `talosctl time`
   against each node). The account-retirement step that ends sessions
   tolerates about 20 s between hosts and needs at least that
   ([Ending sessions](account-retirement.md#ending-sessions)). The
   host-grant revocation fence needs an offset well under 1 s between the
   servers and the PostgreSQL hosts, the standby a failover promotes
   included; that matters only once a scope is enrolled, which this rollout
   does not do, and the fence's skew gauge is not recorded until then.
9. **Canonical configuration.** `WADDLE_XMPP_DOMAIN`, `WADDLE_MUC_DOMAIN`
   and `WADDLE_SPACES_JID` are lowercase, A-label, without a trailing dot;
   each `WADDLE_SERVER_OWNER_LOCALPARTS` entry (`config.serverOwnerLocalparts`
   in the HelmRelease) is a bare canonical localpart, exactly as the account
   is named. A replica refuses to start otherwise
   ([identity profile rollout, Procedure](identity-profile-rollout.md#procedure)).
10. **S2S configuration absent.** Nothing in the HelmRelease values, the
    chart's ConfigMap or the Secrets the Deployment reads sets
    `WADDLE_S2S_ENABLED` or `WADDLE_S2S_CONFIG_FILE`. Any value of
    `WADDLE_S2S_ENABLED` other than unset, `false` or `true` fails startup.
11. **Runtime role grants.** Under the restricted runtime, the owner phase of
    this release grants the runtime role what the new relations need,
    `SELECT` on `retired_account_jids` included; a server whose role cannot
    read that ledger refuses to start. Under a schema-owning login the server
    applies the migrations itself and owns the new relations. The restore
    fence reads `pg_current_wal_flush_lsn()` and `pg_walfile_name()`, which
    PostgreSQL grants to every role and the provisioning leaves in place; if
    they were revoked the fence refuses (`position_unreadable`), which with
    no scope enrolled is logged as information and changes nothing else.
12. **Backups possible.** A PostgreSQL backup of the `waddle` database and a
    copy of SpiceDB's datastore (`postgresql-spicedb`) can be taken inside
    the window, and restoring them has been rehearsed. These are the only way
    back to the previous release once the window's migrations have run (see
    [Rollback](#rollback)). The manifests in this repository declare no
    backup target; confirm how `kubectl cnpg backup` is served before relying
    on it.
13. **Rehearsal.** The window has been run end to end against a restored copy
    of production, including the owner phase and the SpiceDB emptying, with
    the same chart and `zed` versions.

## Before the window

With the previous release still serving:

1. **Identity audit.** Run the read-only `waddle-identity-audit` against the
   account database and decide, for each `non_canonical_spelling` OpenID
   Connect account, how its owner continues: such accounts can no longer
   sign in over XMPP after the rollout
   ([identity profile rollout](identity-profile-rollout.md#existing-accounts-outside-the-profile)).
   Record the counts per finding; the release's startup report is compared
   with them later. Keep the per-account report private.
2. **Authorization inventory.** Inventory what the empty store will lose
   ([codec cutover, Procedure step 1](authz-codec-cutover.md#procedure)):
   per-definition relationship counts for `server`, `space`, `channel`,
   `message`, `direct_message` and `role`, every `channel outcast`
   relationship, and, with clustering, `clustering_muc_room_affiliations`.
   Decide which rooms are banned again after the window, which owners are
   granted again, and which rooms stay closed until then. The inventory holds
   account JIDs in the old, ambiguous encoding: use it only to decide by
   hand, never replay it, and delete it after the window.
3. **Tell users.** Announce the outage and the visible changes (see
   [What users see](#what-users-see-with-s2s-off)): room ownerships and
   moderators other than the deployment's are granted again by hand, some
   sign-ins under a non-canonical spelling stop working, and messages to
   addresses on other servers are now refused at once.
4. **Prepare the operator environment.** A kubeconfig for the production
   cluster, `kubectl`, `flux`, `zed`, `psql` access through the primary pod,
   and a checkout of the commit that will be merged.

## The window

The constraint that orders the steps: **no binary from before the codec may
start, or write SpiceDB, between the moment the store is emptied and the
moment this release has marked it**. Such a binary would write IDs in the old
encoding into a store this release reads as `v2_` IDs, and the marker cannot
detect rows written before it.

How a release reaches the cluster decides how that is enforced. Publication
pushes the rendered `infrastructure/waddle.cloud/gitops/waddle-server/`
directory, with the new image digest and `WADDLE_GIT_SHA` written into the
HelmRelease values, as an OCI artifact (`waddle-server-source.yaml`); a Flux
Kustomization applies it, and helm-controller rolls the HelmRelease. The
HelmRelease has drift detection on, so a manual scale-down is reverted unless
the HelmRelease is suspended. While the Kustomization is suspended the
in-cluster HelmRelease keeps the previous release's digest, and resuming the
HelmRelease alone starts the **previous** image. Steps 7 to 9 therefore check
the digest before anything starts.

The merge happens before the server stops, so the image is built and
published while the previous release still serves, and the outage lasts from
step 4 to step 9. The web client (`chat`) deploys from the same merge through
its own pipeline, independently of the server; its changes work against
either server release (see [Clients](#clients)).

| Step | What | Check before going on |
| --- | --- | --- |
| 1 | Hold Flux | Kustomization and HelmRelease suspended |
| 2 | Merge | The merge commit is the verified head |
| 3 | Wait for publication | Published image digest and Git SHA recorded |
| 4 | Stop the server | No server pod; no other SpiceDB writer |
| 5 | Final inventory and backups | Counts recorded; both backups complete |
| 6 | Empty SpiceDB | Every definition reads empty |
| 7 | Load the new release's configuration | HelmRelease still suspended and carrying the step 3 digest; SpiceDB empty |
| 8 | Owner phase (restricted runtime) | Owner Job ran the step 3 image and completed; handoff published |
| 9 | Start this release | Pods run the step 3 digest; both ready; startup checks pass |
| 10 | Restore grants and bans | Bans and ownerships applied |
| 11 | Verify | [After the window](#after-the-window) checklist done |

[Rollback](#rollback) gives the way back from each step.

1. **Hold Flux.** Suspend the Flux Kustomization that applies the
   `waddle-server` directory (find it with `flux get kustomizations -A`) and
   the HelmRelease:

   ```sh
   flux suspend kustomization <name> -n flux-system
   flux suspend helmrelease waddle-server -n waddle
   ```

2. **Merge** the verified head into `main`, as one merge, with the hosted
   checks passed at that exact commit (prerequisite 3).
3. **Wait for publication.** The server's default pipeline builds the image
   and pushes the artifact. Record the image digest and the Git SHA it
   carries. Nothing changes in the cluster while the Kustomization is
   suspended.
4. **Stop the server.**

   ```sh
   kubectl -n waddle scale deployment/waddle-server --replicas=0
   kubectl -n waddle wait --for=delete pod -l app.kubernetes.io/name=waddle-server --timeout=180s
   ```

   Confirm that no pod of the previous image remains and that no other
   process holds the SpiceDB preshared key and writes relationships. Under
   the restricted runtime, confirm that no owner Job is running.
5. **Final inventory and backups.** With the server stopped, SpiceDB is
   still readable and holds the last state the previous release wrote:
   - repeat the SpiceDB counts and the ban list of
     [Before the window](#before-the-window) step 2;
   - take a PostgreSQL backup of `waddle` and wait for it to complete, and
     keep a copy of the migration ledger with the change record:

     ```sh
     psql_primary -c "\copy (SELECT version, description, checksum FROM _migrations ORDER BY version) TO STDOUT WITH CSV" > migrations-before-2047.csv
     ```

   - take a copy of SpiceDB's datastore database (`postgresql-spicedb`). It
     is the only record of the grants step 6 deletes, and the only way to
     run the previous release again with them. It holds account JIDs: keep it
     under the access controls of the account database;
   - under the restricted runtime, keep a copy of the published handoff
     ConfigMap (`waddle-server-runtime-handoff-pins`).
6. **Empty SpiceDB** ([codec cutover, Procedure step 3](authz-codec-cutover.md#procedure)).
   Either:
   - **delete every relationship** of each definition from the inventory, for
     example `zed relationship bulk-delete <definition>`, repeated until
     `zed relationship read <definition>` returns nothing for every one of
     them. The bulk-delete syntax and its default limit must be confirmed
     against the `zed` build in use before the window; or
   - **recreate the datastore**: delete the `SpiceDBCluster` and its
     PostgreSQL cluster `postgresql-spicedb` and their volumes. Step 7's
     reconciliation creates both again from the manifests, and the SpiceDB
     operator migrates the new, empty datastore.

   The existing schema may stay; this release rewrites it.
7. **Load the new release's configuration.**
   1. Under the restricted runtime, delete the handoff ConfigMap
      `waddle-server-runtime-handoff-pins` (its copy is kept from step 5).
      With no handoff and the Deployment at zero replicas, a release renders
      the Deployment at zero replicas, and still renders the owner CronJob,
      when `restrictedRuntime.handoff.awaitOwnerJob` is set (prerequisite 5).
   2. Resume the Flux Kustomization, and only it:
      `flux resume kustomization <name> -n flux-system`.
   3. Confirm the HelmRelease is still suspended
      (`flux get helmreleases -n waddle`) and now carries the step 3 image:

      ```sh
      kubectl -n waddle get helmrelease waddle-server -o jsonpath='{.spec.values.image.digest}{"\n"}'
      kubectl -n waddle get helmrelease waddle-server -o yaml | grep -A1 'name: WADDLE_GIT_SHA'
      ```

      If the digest is not the step 3 digest, stop: resuming the HelmRelease
      would start the previous image on the empty store.
   4. Restart SpiceDB (or wait for the recreated one to be ready): `ReadSchema`
      can serve a cached schema after its datastore was reset, and the
      schema-version check reads it ([schema versions](authz-schema-versions.md)).
      Verify again that every definition reads empty.
8. **Owner phase, restricted runtime only.** The owner phase of this
   release's image must apply migrations 2002 to 2014 and grant the runtime
   role before any server of this release starts; servers of this release
   refuse a database whose ledger lacks them. Nothing in the chart sequences
   it for an upgrade; this step uses the chart's fresh-install path set up in
   step 7.
   1. Resume the HelmRelease: `flux resume helmrelease waddle-server -n waddle`.
      It renders the owner CronJob with the new image and the Deployment at
      zero replicas.
   2. Confirm both before going on:

      ```sh
      kubectl -n waddle get cronjob waddle-server-owner -o jsonpath='{.spec.jobTemplate.spec.template.spec.containers[*].image}{"\n"}'
      kubectl -n waddle get deployment waddle-server -o jsonpath='{.spec.replicas}{"\n"}'   # 0
      ```

      The CronJob's image must be pinned to the step 3 digest; otherwise
      suspend the HelmRelease again and stop.
   3. Run the owner Job from that CronJob and wait for it:

      ```sh
      JOB=owner-$(date +%Y%m%d%H%M%S)
      kubectl -n waddle create job "$JOB" --from=cronjob/waddle-server-owner
      kubectl -n waddle logs -l job-name="$JOB" --all-containers --prefix --tail=-1 -f
      kubectl -n waddle wait --for=condition=complete job/"$JOB" --timeout=15m
      ```

      Its exit codes are those of
      [the restricted runtime cutover](../runbooks/restricted-runtime-cutover.md);
      a refusal names its cause and changes nothing.
   4. Confirm the handoff ConfigMap was published again.

   This sequence is derived from the chart and must be proven in the
   rehearsal (prerequisite 13).

   With a schema-owning login there is no owner phase: the first server to
   start applies the migrations under the migration lock.
9. **Start this release.** Under the restricted runtime,
   `flux reconcile helmrelease waddle-server -n waddle --force`; with a
   schema-owning login, resume the HelmRelease (its digest was checked in
   step 7). Then:

   ```sh
   kubectl -n waddle rollout status deployment/waddle-server --timeout=10m
   kubectl -n waddle get pods -l app.kubernetes.io/name=waddle-server \
     -o jsonpath='{range .items[*]}{.metadata.name} {.status.containerStatuses[*].imageID}{"\n"}{end}'
   ```

   Every pod must run the step 3 digest, both replicas must become ready,
   and every check under [Startup checks](#startup-checks) must pass. One
   replica logs `Marked empty authorization store for the current ID codec`
   and one `Recorded the authorization schema version`. A replica refused
   because the store `changed while its codec marker was being written` only
   needs a restart.
10. **Restore grants and bans.** Grant again the room and Space ownerships
    decided before the window, and ban again through the normal XEP-0045
    admin path or the `set-affiliation` ad-hoc command, so SpiceDB and the
    SQL room affiliations agree. Confirm that the rooms kept closed are
    closed ([codec cutover, Accepted loss](authz-codec-cutover.md#accepted-loss)).
11. **Verify.** Run [After the window](#after-the-window). Keep `Recreate`;
    a return to `RollingUpdate` is a separate change under the guard in
    [relay cutovers](relay-cutovers.md#rollingupdate-guard).

## S2S stays off

S2S is off unless `WADDLE_S2S_ENABLED` is `true`. With it unset or `false`
no S2S file is read, no listener is bound or taken from `LISTEN_FDS`, no peer
is dialled and no S2S credential is loaded
([connection manager design](../../server/docs/planning/s2s-connection-manager-2047.md#activation-and-the-configuration-snapshot)).
The production chart has no S2S port or setting. Confirm after step 9:

- **Not configured.** The rendered Deployment and every ConfigMap and Secret
  it reads set neither `WADDLE_S2S_ENABLED=true` nor
  `WADDLE_S2S_CONFIG_FILE`. Secrets are read through `envFrom`
  (`extraSecretRefs` in the values), so list their keys as well:

  ```sh
  kubectl -n waddle get deployment waddle-server -o yaml | grep -n WADDLE_S2S
  kubectl -n waddle get configmap -o yaml | grep -n WADDLE_S2S
  for secret in $(kubectl -n waddle get deployment waddle-server \
      -o jsonpath='{.spec.template.spec.containers[*].envFrom[*].secretRef.name}'); do
    kubectl -n waddle get secret "$secret" -o go-template='{{range $k, $v := .data}}{{$k}}{{"\n"}}{{end}}' \
      | grep -n WADDLE_S2S && echo "in $secret"
  done
  ```

  Every command prints nothing.

- **No listener.** No server log contains `Bound S2S listener`, and the
  Service exposes no port besides the chart's HTTP port
  (`kubectl -n waddle get svc`).
- **No dial.** No `waddle.s2s.*` series is exported by any replica. None is
  registered at zero, so any series at all means S2S code ran
  ([metrics](server-metrics.md#server-to-server-connection-metrics)).
- **No advertisement.** The server's `disco#info` lists no
  `urn:waddle:s2s:0`; the web client therefore lists bookmarks of rooms on
  other servers under "Rooms on other servers — unavailable" and joins none.
  `server/capabilities.toml` shows only what W11's reviewed change added.
- **Empty S2S tables.** The tables of migrations 2004, 2005, 2008 and 2010
  to 2013 hold no row (`s2s_iq_correlations`, `s2s_iq_correlation_owners`,
  `s2s_remote_room_occupancies`, `s2s_remote_room_verdicts`,
  `s2s_local_room_seats`, `s2s_pending_subscriptions`,
  `s2s_remote_pubsub_subscriptions`, `s2s_remote_offline_admissions`;
  `s2s_federated_spaces` and `s2s_space_subscriptions` stay empty until an
  owner turns a Space's `federated` setting on).

## Old binaries and mixed versions

The production strategy is `Recreate`, so in the window every replica of
the previous release stops before any replica of this release starts, and
no two releases serve together. The rules below say what happens if an old
binary runs anyway (a leftover pod, an operator tool, a rollback attempt) and
what a later rolling update has to respect.

| Part | An old binary after the window | Rule |
| --- | --- | --- |
| Authorization ID codec | Its schema write would remove `waddle_authz_codec` and `waddle_authz_schema`, which hold relationships, so SpiceDB refuses it and its startup fails. A process that writes relationships without writing the schema (a tool with the preshared key) is not fenced and would write the old encoding | No pre-codec binary or tool runs against the marked store. Never delete the marker to get past a refusal ([codec cutover, Rollback](authz-codec-cutover.md#rollback)) |
| Schema version 2 | The previous release has no version check of its own: its schema write would remove `waddle_authz_schema`, which holds the version marker, so SpiceDB refuses it and its startup fails. A versioned binary older than the store logs `authorization store schema is version N, newer than version M of this server` | A crash-looping old pod is a binary that was not retired; remove it ([schema versions](authz-schema-versions.md#rollout-within-2047)) |
| Migration ledger | The previous release refuses a ledger that holds a version it does not know: `found unknown migration v2002 ...; this usually means an older binary is starting against a newer ledger` | Migrations are forward-only. See [Rollback](#rollback) |
| Identity profile | A replica of the previous release folds inputs, accepts `ALICE` at sign-in and provisions Unicode OpenID Connect names | Every replica runs this release before any client relies on the refusals |
| Destination routing | A replica of the previous release answers IQs to other domains per namespace (`not-allowed` for ping, Carbons, session and discovery; other namespaces reach local handlers), archives a message to another domain and drops it silently. The decision is taken by the replica that holds the client's connection and leaves no stored state, so a client on another replica sees the other behaviour | No migration is needed; the behaviour converges when the last old replica is gone |
| Relay between replicas | The ordered relay's envelope and its id `waddle.clustering.relay.deliver_ordered.v16` are unchanged. One message id is new, `waddle.clustering.relay.remote_room_request.v1`, sent only for stanzas from S2S streams: nothing sends it with S2S off, and a replica that predates it answers `UnknownMessage`, which the sender turns into a retryable error | A rolling update is safe for the relay in either order |
| Host-grant writer and revocation fences | A binary from before the fences decides managed rooms from the legacy stores, holds no fence lease, and so is neither waited for by a revoke nor bound by it | No scope is enrolled until every serving replica runs this release, and no older replica joins while a scope is enrolled (G11, G24) |
| Session endings of a retired account | A replica without migration 2014's lease is not seen by `waddle-server account retire`, ends nothing, and keeps the account's streams open | Every serving replica runs this release before an account is retired ([account retirement, step 3](account-retirement.md#procedure)) |
| `[host_policy]` in the S2S snapshot | A binary that predates the table refuses a snapshot that holds it, `federate_new_rooms` included; the snapshot is not read while S2S is off | Before a snapshot uses the table, every replica runs this release; a rollback to an older binary removes the table first |
| Web and native clients | A client older than the server sees the new refusals (see [What users see](#what-users-see-with-s2s-off)); a web client newer than the server, between the `chat` deployment and step 9, behaves as with S2S off | No client-side migration is needed; see [Clients](#clients) |

### Clients

- The web client stores a room's last-seen anchor under the room's address
  (`waddle.chat.last-seen.v2.room.<room JID>`) and deletes the anchors it
  stored under a room's local id, which two rooms on different services can
  share. Each room's "new messages" divider therefore starts again once per
  browser. A route that names a room by its address resolves to exactly one
  room; a route with a legacy local id still resolves when it is not
  ambiguous.
- Tabs opened before the `chat` deployment run the old client until they
  reload. Against this server they see the server-side changes listed under
  [What users see](#what-users-see-with-s2s-off) and none of the web client's.
- Native clients keep their own release cycle and see only the server-side
  changes.

## Stores, migrations and startup checks

### SpiceDB: the empty-store cutover

The release refuses a populated SpiceDB store that carries no codec marker,
before it writes the schema. Each replica logs `Refusing the authorization
store: it is not bound to the current ID codec` and startup fails with
`Failed to ensure permission schema: Authorization store refused: authorization
store holds ...`. That is the expected outcome of skipping step 6, and it
writes nothing. The full procedure, what is lost, the refusals and the
verification are in the [codec cutover](authz-codec-cutover.md); this
runbook's step 6 is its step 3.

Verification, after step 9:

```sh
zed relationship read waddle_authz_codec
# waddle_authz_codec:incarnation#codec@waddle_authz_codec:v2
zed relationship read waddle_authz_schema
# waddle_authz_schema:installed#version_2@waddle_authz_schema:installed
zed relationship read waddle_grant_projection
# nothing: no scope is enrolled
```

and the deployment owners can administer the server, a fresh account
receives its deployment membership, and a re-applied ban denies the banned
account in its room, also when the account is a member of the parent Space.

### SpiceDB: the schema version

The start that binds the empty store to the `v2` codec installs schema
version 2 and records it directly; no store is recorded at version 1. There
is no mixed-version period for this transition, because the codec cutover
already stops every replica of the previous release
([schema versions, Rollout within #2047](authz-schema-versions.md#rollout-within-2047)).

### PostgreSQL: migrations 2002 to 2014

The migrations after the schema baseline (version 2000) are numbered without
a gap. Migration 2001 (offline call notification candidates) belongs to the
previous release; this release adds 2002 to 2014. The runner refuses a
database that records a later version but not an earlier one, a version it
does not know, and a version whose description or checksum differs from its
own. There is no down migration.

| Version | Creates | Runtime role (restricted runtime) | With S2S off |
| --- | --- | --- | --- |
| 2002 | Host-grant authority: authority row, scope lifecycles, scopes, rooms, grants, operation journal | `SELECT` on the authority row, lifecycles and rooms; `SELECT, UPDATE` on scopes; `SELECT, INSERT, UPDATE` on grants; `SELECT, INSERT` on the journal | No scope; the authority row only |
| 2003 | Restore incarnation: epoch, lineage binding, authority id, position acknowledgement, the `restored`, `reconstructed` and `fenced` states | Unchanged matrix | The first owner phase after it binds the authority to the database lineage and changes nothing else |
| 2004 | `s2s_iq_correlations`, `s2s_iq_correlation_owners` | `SELECT, INSERT, DELETE`; `SELECT, INSERT, UPDATE, DELETE` | Empty |
| 2005 | `s2s_remote_room_occupancies`, `s2s_remote_room_verdicts` | `SELECT, INSERT, UPDATE, DELETE` | Empty |
| 2006 | `host_grant_fence_replicas`, `host_grant_fence_acks` (PostgreSQL only) | `SELECT, INSERT, UPDATE` | Empty: no lease without an enrolled scope |
| 2007 | `retired_account_jids` | `SELECT` only | Read at every startup, provisioning and session creation |
| 2008 | `s2s_local_room_seats` | `SELECT, INSERT, UPDATE, DELETE` | Empty |
| 2009 | `channels.federated`, zero for every row | — | Every room is closed to other servers |
| 2010 | `s2s_pending_subscriptions`, `s2s_remote_pubsub_subscriptions` | `SELECT, INSERT, UPDATE, DELETE` | Empty |
| 2011 | `s2s_remote_offline_admissions` | `SELECT, INSERT, UPDATE, DELETE` | Empty |
| 2012 | `s2s_federated_spaces` | `SELECT, INSERT, UPDATE, DELETE` | Empty unless an owner turns a Space's `federated` setting on |
| 2013 | `s2s_space_subscriptions` | `SELECT, INSERT, UPDATE, DELETE` | Empty |
| 2014 | `account_session_ending_replicas`, `account_session_ending_requests`, `account_session_ending_acks` | `SELECT, INSERT, UPDATE` on leases and acknowledgements; `SELECT` on requests | One lease row per serving process, renewed every 5 s |

On SQLite, 2002, 2003 and 2006 create nothing. A development or test
database that a build from before the final numbering migrated records
versions that now name other migrations, with other checksums, and every
later startup refuses it: recreate it. No such database shipped.

Under the restricted runtime the owner phase applies them and grants the
runtime role from the matrix before any server of the release starts
(window step 8). With a schema-owning login, the first server to start
applies them. A schema-owning login also fails the host-grant store's
privilege check at every start; with no scope enrolled that is logged as
information and changes nothing, and it must move to the restricted runtime
before any scope is enrolled.

`WADDLE_HOST_GRANT_RESTORE_EPOCH` is new and defaults to `0`, the value
migration 2003 records. Leave it unset; the owner phase and every server
must read the same value ([restore runbook](authz-host-grant-restore.md#configuring-the-epoch)).

### Startup checks

In addition to the codec and schema checks above, each replica of this
release runs these at startup:

| Check | On failure | Expected after the window |
| --- | --- | --- |
| Configured domains and owner entries canonical | Startup fails, naming the variable | Passes |
| `WADDLE_S2S_ENABLED` is unset, `false` or `true` | Startup fails | Unset |
| Identity profile report: both account stores read and classified | Logged only, without identifiers: `every local account is inside the identity profile`, or `local accounts outside the identity profile` with a count per finding | The counts of the pre-window audit, the same on every replica |
| Bookmark item report: the PubSub store read | Logged only: `every stored bookmark item id is canonical`, or counts per carrier (`spaces`, `pep_bookmarks`, `dm_bookmarks`) | Recorded; a nonzero `spaces` count must be cleared or accepted before activation |
| Retired-account ledger readable as the runtime role, before the first provisioning | Startup fails: `the runtime database role cannot read retired_account_jids ...` | Passes |
| Host-grant enrolled set loaded (PostgreSQL) | Retried every second for up to 30 s, then the process exits: `the host-grant enrolled set could not be loaded; refusing to serve until it is` | Loaded; reloaded every 5 s |
| Host-grant runtime privileges and restore fence | Never stops the replica. With no scope enrolled a failure is logged as information and retried; readiness stays ready | Readiness ready: with a verified attachment the `/readyz` body has no `host_grants` field, without one it says `"host_grants": "unmanaged"` (always the case under a schema-owning login) |
| Session-ending lease kept (migration 2014) | Never stops the replica: warning `the session-ending lease could not be kept; retrying`, and the replica is missing from retirement reports until it succeeds | Lease renewed every 5 s |
| Revocation fence clock skew | Recorded only while the replica holds a fence lease, which needs an enrolled scope | Absent |

## What users see with S2S off

Every change below applies with S2S off. Source documents give the detail;
the slice is in brackets.

### Authorization and rooms

| Area | Before | After | Source |
| --- | --- | --- | --- |
| Grants in SpiceDB | Space and channel owners, admins, moderators, managers, members, writers and viewers; roles; direct-message participants; channel `outcast` bans | All lost. Startup restores only the deployment's `server` owner and member grants. Bans committed to the SQL room affiliations (clustering) survive and are enforced on join, but SpiceDB's readers stop seeing them until they are re-applied; managed channels' Space `parent` links are repaired lazily on join | [Codec cutover](authz-codec-cutover.md#accepted-loss) |
| Banned subject | Kept every channel permission another relation or the parent Space gave, and control of their own messages | Every channel permission is denied, Space-derived ones included; a Space admin banned from a room can no longer manage, moderate or delete it. Visible change is narrow: the server paths that read these permissions already checked bans first | [Schema versions, Version 1](authz-schema-versions.md#version-1-outcast-precedence) |
| `muc#admin` and `channels:set-affiliation` | Recorded an owner or admin affiliation for any JID | Refused (`not-allowed`) for an account at a domain the deployment does not serve; existing affiliations are kept | [Routing](../../server/docs/planning/s2s-routing-2047.md#behaviour-with-s2s-off) |
| Privileged Space standing | Written for any JID | `spaces:set-role` refuses owner and admin, PubSub `<affiliations/>` refuses owner, publisher and publish-only, and the permission actor refuses channel `manager` and `moderator`, Space `moderator` and role `manager`, for an account at a domain the deployment does not serve | Routing |
| Owner reseed and seeds | The reseed could lift a ban on a node; seeds were written for any domain | The reseed lifts no ban; owner seeds and deployment ownership are written only for accounts of the served domain; an admin Space creation without a seed policy is refused before anything is written | [Host-grant design, writer fence rollout](../../server/docs/planning/host-grant-authority-2047.md#rollout-part-of-the-writer-fence-g24) |
| Room address not in canonical form | Accepted as an unmanaged room | A room message to it is refused with `service-unavailable`; the identity profile already refuses such addresses at parsing | [Host-grant design, rollout part](../../server/docs/planning/host-grant-authority-2047.md#rollout-part-g24) |
| Spaces placement | A Spaces items read could move a channel and retract its bookmarks from other Spaces | A publish that names a new Space needs owner, admin or manager standing on the channel or one of its parent Spaces, or deployment ownership; an items read moves nothing; a join fills a missing parent only from the channel's link | Writer fence rollout |
| Room-bound extension nodes | Subscriptions and events kept after access ended | Follow the room's current right | Writer fence rollout |
| Room administration | Waited for the room's configuration lock without bound | A client's administration and owner configuration requests wait at most 15 s, then `resource-constraint` | [Host-grant design, revocation fence rollout](../../server/docs/planning/host-grant-authority-2047.md#rollout-part-of-the-revocation-fence-g24) |
| Corrections and retractions | A room compared occupant addresses only, so a nickname could retract another account's archived message | Accepted only from the account the archive records as the author | Routing |
| Room configuration | — | New boolean field `urn:waddle:roomconfig:federated`, off for every room; changing it is announced with status 104 and has no effect with S2S off | Routing |
| Space configuration | — | New field `urn:waddle:spaces:federated`, off by default | Routing |
| MUC service `disco#items` (dormant: needs an enrolled scope) | A public managed room was listed to nobody | Listed to a session whose grants allow `view`; the first-party client joins every listed room it has no bookmark for, so members join such rooms at connect | Routing |
| Session room listings | Live rooms asked one at a time, 5 s each; a slow managed room's grants left it out; an instant room naming a channel was kept when the catalog read filled its bound | Asked sixteen at a time within 1.2 s; a managed room not decided within 1 s answers `internal-server-error` (type `wait`); an instant room naming a channel that the catalog check cannot clear is left out when the catalog read fills its bound; a shortened listing tells no count; concurrent listings share one read | Routing |
| Space node `disco#items` | An empty list for any node | The rooms the Space holds (up to 1,024), or `item-not-found` for a node the session may not read | Routing |
| Rooms leaving a Space | Only a retract notified subscribers | A publish into another Space and the admin channel delete, channel update and Space delete commands notify local subscribers as a retract | Routing |
| `urn:waddle:joined-spaces:0` | Created like any PEP node | A private node: `whitelist` access, up to 1,024 items, only its owner reads or publishes, a publish without an item id is refused | Routing |
| Inbox and activity | Rows for room members at other domains | Written only for addresses at served domains; earlier rows stay | Routing |
| Re-seating after a room's claim moves (clustering) | A recorded occupant at a domain the deployment does not serve was left out of a managed room and seated again in any other | Left out of every room | Routing |
| Remote member-level grant in a managed room (dormant: needs an enrolled scope) | Admitted the remote account | Admits nothing, because no remote domain is admitted; a remote recipient planned in a groupchat is frozen no child | [Host-grant design, effect boundary rollout](../../server/docs/planning/host-grant-authority-2047.md#rollout-part-of-the-effect-boundary-g24) |
| Revocation sweep in a managed room (dormant: needs an enrolled scope) | An occupant at an unadmitted domain kept its seat while its recorded affiliation was a membership | Removed, as is an occupant whose membership is a recorded increase not yet effective; with S2S off only an occupancy recorded while S2S was on is affected | Effect boundary rollout |

### Addressing, routing and accounts

| Area | Before | After | Source |
| --- | --- | --- | --- |
| Stanza addresses and payload JIDs | Case, width and compatibility forms folded | Refused with `<jid-malformed/>`; a trailing dot is removed and a U-label domain becomes its A-label | [Identity profile rollout](identity-profile-rollout.md#what-changes-for-clients-and-accounts) |
| MUC nicknames | Folded (`ｎｉｃｋ` joined as `nick`) | The stanza is refused; a client must use the nickname exactly | Identity profile rollout |
| Sign-in | `ALICE` signed in as `alice`; OAUTHBEARER re-slugged | Only the exact canonical name; OpenID Connect accounts with a non-canonical stored name can no longer sign in over XMPP | Identity profile rollout |
| New account names | Any valid localpart | 1–64 bytes of `a-z`, `0-9`, `.`, `_`, `-`, `+`; OpenID Connect names transliterated | Identity profile rollout |
| Resource binding | `ｗｅｂ` bound as `web` | `<bad-request/>` | Identity profile rollout |
| RFC 7395 `<open from>` | Folded | A non-canonical hint closes the stream with `invalid-from` | Identity profile rollout |
| XEP-0198 handled count | A stanza refused at parsing was not counted, so later acknowledgements named the wrong stanza | Every well-formed stanza refused at parsing counts as handled | Identity profile rollout |
| Admin ad-hoc commands (`group-dm:create`, `channels:set-affiliation`, `channels:kick`, `spaces:set-role`, push operator commands) | JID fields folded | A non-canonical JID refused: `<jid-malformed/>` for a typed field, `<bad-request/>` for an untyped one | Identity profile rollout |
| Account lookups (roster, presence, group DMs, XEP-0055) | Input folded before the lookup | Taken exactly; a non-canonical spelling names no account; XEP-0055 leaves out rows whose stored name is not canonical | Identity profile rollout |
| Bookmark item ids (Spaces, XEP-0402, DM bookmarks) | Stored as written and folded when read | Publishing a non-canonical id is refused with `<bad-request/>`, including a U-label remote room; a retract by the exact stored id is accepted | [Identity profile rollout, stored bookmark items](identity-profile-rollout.md#stored-bookmark-items-with-non-canonical-ids) |
| IQ to a domain the deployment does not serve | Ping, Carbons, session and discovery refused with `not-allowed`; other namespaces reached local handlers | Every `get`/`set` refused with `remote-server-not-found` (type `cancel`) from the home server, before any handler; before resource binding, `not-authorized`; results and errors to such a domain dropped without an error; XEP-0410 self-ping reads the error as undecided | [`destination.rs`](../../server/crates/waddle-server/src/destination.rs), [remote ping note](../planning/remote-ping-2047.md) |
| IQ to a numeric IPv4 domain (`2130706433`, `0x7f.1`, `127.1`) | Classified as a remote or unqualified domain | Never routable: `remote-server-not-found`, as for IP literals and single-label names | Identity profile rollout |
| Message to an unserved domain | Archived, put in the sender's inbox and carbon-copied, then dropped silently | Message error `remote-server-not-found` (type `cancel`) from the addressed JID, without the body; nothing archived | Routing |
| Presence to an unserved domain | A subscription request answered `unsubscribed`; directed presence dropped | Presence error `remote-server-not-found` | Routing |
| IQ to a blocked remote address | `remote-server-not-found` | `not-acceptable` with `<blocked/>` | Routing |
| Direct-message pin marker for a remote peer | `forbidden` | `remote-server-not-found`; no pin recorded | Routing |
| Received carbons | Private messages with `muc#user` from a full JID outside the account domain were carbon-copied | Not carbon-copied | Routing |
| Roster contacts at other domains | Removed at every start | Kept, dormant; adding a new one is still refused (`item-not-found`) | [Account traffic, Behaviour with S2S off](../../server/docs/planning/s2s-account-traffic-2047.md#behaviour-with-s2s-off) |
| Message or presence to a remote JID, and `unsubscribed` | — | Answered `remote-server-not-found` before any roster or blocking check; an `unsubscribed` first removes a request recorded from that JID, which exists only if recorded while S2S was on | Account traffic |
| Retired accounts | No retirement existed | `waddle-server account retire` records a JID in a ledger no provisioning path reuses (an identity-provider login with a retired name gets `name1`, `name2`, …) and ends the account's sessions | [Account retirement](account-retirement.md) |
| Configured domains and owner entries | Any JID-valid spelling | Must be canonical, or startup fails | Identity profile rollout |

### Web client

The web client changes listed in
[the routing design](../../server/docs/planning/s2s-routing-2047.md#behaviour-with-s2s-off)
("The web client changes these whether or not S2S is on"), in short:

| Area | After |
| --- | --- |
| Rooms left with status 301, 321 or 322, or destroyed | Not joined again in the session or at a later sign-in |
| A late error answering a join | Applied to that join; a denial blocks automatic joins |
| Space items and bookmarks naming rooms on other domains | Not listed in the Space; bookmarks shown under "Rooms on other servers — unavailable" with Forget only; nothing joins them |
| Retained rooms at session-ready | Rejoined only when on the account domain or placed at home by discovery |
| A room on another domain created by the join (status 201) | Left at once and recorded as missing; its bookmark's `autojoin` turned off |
| Media from a room whose host is not known yet | Waits for the user to load it |
| Leaving a room on another domain | `autojoin` turned off on the stored bookmark, whatever this device last read of it; when the home server stops advertising `urn:waddle:s2s:0`, every room placed on another domain is left and a lost stream rejoins none |
| Unfollowing a Space on another domain | Dropped when another device followed it later |
| A Space's `federated` change | Sends `pubsub#max_items` back unchanged |
| Last-seen anchors | Keyed by room address; legacy anchors deleted once |

### Background cost on every replica

Not visible to users; expected in database load and logs:

- The host-grant enrolled set loads before the replica serves and reloads
  every 5 s (one read-only snapshot of two small relations); permission
  checks of channel and Space objects answer from it in memory.
- Every legacy channel or Space tuple write reads PostgreSQL once per object
  it names (500 ms lock and 1 s statement bound); a room message's
  acceptance adds one indexed lookup.
- The restore fence reads the authority row, lists SpiceDB's (empty)
  projection definition and reads the (empty) scope table at attachment and
  about every five minutes; until its first pass, a message to an unmanaged
  room may ask SpiceDB about that room's witness, within half a second.
- Each serving process renews its session-ending lease every 5 s and reads
  the retirement requests once a second in a read-only transaction.
- Session creation reads the retired ledger once; registration and
  identity-provider provisioning insert under its condition.
- A Spaces publication lists the node's deliverable subscriptions once more
  before it hands over, and its fan-out no longer looks up connections for a
  subscriber at a domain the deployment does not serve.
- A self-ping and the media grants of a call participant ask the host-grant
  gate, which answers an unmanaged room from the enrolled set in memory.
- At startup, the identity and bookmark reports read both account stores and
  the PubSub store.

## Monitoring

Metric names are those of the [metrics reference](server-metrics.md).

| Signal | Expected | Act on |
| --- | --- | --- |
| `/readyz` on each replica | 200, with no `host_grants` field or `"host_grants": "unmanaged"` | 503, or any other `host_grants` value |
| `waddle.host_grant.enrolled_set.load_started` and `.age`, per replica | Present; age below 30 s | Series absent, or age above 30 s: the replica's set is stale |
| `waddle.host_grant.authority.unverified` (declared in `permissions/host_grant/runtime_attach.rs`) | 0: it is 1 only while an unretired scope is enrolled | Any 1 |
| `waddle.s2s.*` | Absent | Any series |
| `waddle.host_grant.fence.clock_skew` | Absent: no fence lease without an enrolled scope | — |
| `waddle.auth.failures` by `stage` and `error_code` | A rise from non-canonical sign-ins, bounded by the audit's counts | A rise beyond them |
| `waddle.muc.admission.denied` | Re-applied bans and lost grants show here | A rise not explained by the re-bans and lost ownerships |
| `waddle.muc.room_listings.session_builds` | Near one per listing burst per replica | Growth with connect rate |
| `waddle.restricted_runtime.startup.refusals`, `.attach.failures` | Zero | Any |
| `waddle.account_retirement.*` | Only after an operator runs a retirement | — |
| Logs | The startup lines above; host-grant verification failures at information level | `Refusing the authorization store`, `the host-grant enrolled set could not be loaded`, `cannot read retired_account_jids`, `the session-ending lease could not be kept`, a `clock_skew` warning, an error-level host-grant verification log |

## Rollback

Prefer roll-forward. What a step changed decides how far back is possible:

| Failure at or before | Way back | Loses |
| --- | --- | --- |
| Steps 1 to 3 (the previous release serves) | Revert the merge on `main` and wait for the revert's publication. Then resume the Kustomization and the HelmRelease, and confirm the HelmRelease carries the revert's digest. Resuming Flux before the revert is published rolls this release onto the populated store, where it refuses to start | Nothing |
| Step 4 or 5 (stopped, stores untouched) | The same; the revert's release starts on the untouched stores | Nothing |
| Step 6 or 7 (SpiceDB emptied, PostgreSQL untouched) | Keep the HelmRelease suspended. Restore SpiceDB's datastore from step 5 and restart SpiceDB; under the restricted runtime, restore the handoff ConfigMap from step 5. Then as for steps 1 to 3 | Nothing, if the copy is complete and nothing wrote SpiceDB in between |
| Step 8 or 9 (migrations applied, store marked) | Suspend the HelmRelease and stop every replica. Restore PostgreSQL from step 5, adopting the lineage as the [lineage runbook](../../server/docs/operations/db-lineage.md) requires for the restore method used. Restore SpiceDB's datastore from step 5, or empty it again including both markers. Under the restricted runtime, restore the handoff ConfigMap from step 5; if the restore recreated pinned objects, the previous release's owner Job has to publish a new handoff. Then as for steps 1 to 3 | Everything written since step 4. With an emptied rather than restored SpiceDB, every grant again, as in the [reverse codec cutover](authz-codec-cutover.md#rollback) |
| After step 10 (serving) | The same as steps 8 and 9 | In addition, the messages, accounts and grants written while this release served |

The limits:

- **Migrations are forward-only.** The previous release refuses a ledger that
  holds 2002 or later, so it cannot start against the migrated database. No
  down migration exists. Going back means a PostgreSQL restore.
- **The SpiceDB markers are a fence.** The previous release cannot start
  against a marked store, and must not: never delete `waddle_authz_codec` or
  `waddle_authz_schema` by hand to get past the refusal
  ([codec](authz-codec-cutover.md#rollback),
  [schema](authz-schema-versions.md#rollback)).
- **The identity profile alone would need no data change**
  ([its rollback](identity-profile-rollout.md#rollback)), but in this
  release it ships with migrations and the codec cutover, which do.
- **A restore discards what users did since.** Weigh a rollback after the
  server has served against fixing forward.
- With no scope enrolled, a PostgreSQL restore needs none of the host-grant
  restore procedure: [the restore runbook](authz-host-grant-restore.md)
  applies to databases holding an enrolled scope.

## After the window

Record each result in the evidence log.

1. Both replicas run the published digest and `WADDLE_GIT_SHA` of the merge
   commit, and are ready.
2. The migration ledger holds every version from 2001 to 2014:

   ```sh
   psql_primary -Atc "SELECT string_agg(version::text, ',' ORDER BY version) FROM _migrations WHERE version > 2000"
   # 2001,2002,2003,2004,2005,2006,2007,2008,2009,2010,2011,2012,2013,2014
   psql_primary -Atc "SELECT count(*) FROM host_grant_scopes"
   # 0
   ```

3. The SpiceDB verification under
   [Stores, migrations and startup checks](#spicedb-the-empty-store-cutover)
   passes.
4. The identity report on every replica matches the pre-window audit; the
   bookmark report is recorded.
5. [S2S stays off](#s2s-stays-off) holds.
6. Functional checks with two accounts: sign in, exchange a direct message,
   create a room, post and reload its history, with the two accounts on
   different replicas; a deployment owner administers the server; a new
   account receives its deployment membership; a re-banned account is
   refused in its room, also as a member of the parent Space; a message to an
   address on another server is answered `remote-server-not-found` and does
   not appear in the sender's archive.
7. Grants and bans decided before the window are applied, and the closed
   rooms are closed.
8. The pre-window SpiceDB inventory is deleted.
9. Monitoring shows the expected values for at least a day; then the window
   is closed in the change record.

## Activation is out of scope

Turning S2S on in any deployment is gate G22: a separate owner decision with
its own named window and its own runbook, which does not exist yet. It is not
part of this rollout, and nothing in it should be read as preparing an
enablement beyond what is listed here. Before it, at least:

- W11's advertisement and W12's default change are merged and reviewed, and
  G20's evidence for the `open` default has passed.
- The public-WebPKI profile is checked against a real public peer, the peer
  scope is lifted to `public` through its reviewed change (G6), and discovery
  is checked against real SRV records.
- The activation runbook states that listing a trusted home delegates
  privileged authority and room moderation to that home's operator and to
  whoever later controls the domain (D8).
- Every account the identity report lists is resolved or explicitly
  accepted, and the bookmark report's `spaces` count is zero or each item
  accepted ([identity profile rollout, step 5](identity-profile-rollout.md#procedure)).
- Old binaries and credentials are retired (G11, G22); every replica runs a
  binary that knows `[host_policy]` before the snapshot uses it.
- Inbound dialback stays off unless G5's conditions are met.

Enrolling the first host-grant scope is a further decision, and has these
preconditions recorded in the
[host-grant design](../../server/docs/planning/host-grant-authority-2047.md)
and the [verification checklist](../evidence/2047/unrun-tests.md#items-recorded-by-the-known-issue-list):

- The server serves under the restricted runtime.
- Every serving replica runs this release or later, and none older joins
  while a scope is enrolled; the enrollment waits until every replica's
  `waddle.host_grant.enrolled_set.load_started` is later than the
  enrollment's commit by more than the allowed clock skew.
- PostgreSQL hosts are within well under 1 s by NTP, and every replica's
  `waddle.host_grant.fence.clock_skew` stays below 500 ms.
- **Open at this release:** each writer-fence surface outside room-manifest
  acceptance must compare its attachment's binding with the authority it
  reads, per decision, and must ask
  `HostGrantAttachmentGate::refuses_unmanaged` before serving a room no
  enrolled scope manages. In this release only room-manifest acceptance
  (`ingress/commit_room.rs`) asks it.
- **Open at this release:** a PubSub admission identity for an enrolled
  scope's Space (until then that Space is served to nobody over S2S), and a
  tool that seeds a replacement SpiceDB with the authority's witnesses (until
  then a permanently lost SpiceDB leaves projected scopes closed).

## Gaps to settle before the window

These steps are undefined, unproven or disagree between documents. Each needs
an answer, or the rehearsal's evidence, before the go-ahead is acted on.

1. **Owner phase on an upgrade.** The chart does not sequence the owner Job
   before serving pods for a release with migrations. Window steps 7 and 8
   derive a sequence from the chart's fresh-install path (`awaitOwnerJob`
   and a missing handoff, chart 0.4.13 or later) and from how publication
   reaches the HelmRelease (through the Flux Kustomization, not the
   HelmRelease alone); it has not been run. Whether resuming the
   Kustomization leaves a suspended HelmRelease suspended is checked in step
   7 but not established.
2. **`zed` bulk deletion.** The codec cutover leaves the exact
   `zed relationship bulk-delete` syntax and its default limit to be
   confirmed against the deployed `zed`.
3. **Backups.** The manifests declare no backup target for either
   PostgreSQL cluster; how `kubectl cnpg backup` is served in production, and
   how SpiceDB's datastore is copied and restored, must be settled and
   rehearsed.
4. **Versions.** Neither SpiceDB's version (operator channel `stable`) nor,
   outside the restricted-runtime component, PostgreSQL's is pinned by the
   manifests.
5. **Destination routing and client state.** The mixed-version behaviour of
   the destination classifier and the web client's storage change above are
   derived from the code; neither slice recorded a rollout part of its own.
6. **Metrics reference.** `waddle.host_grant.authority.unverified` and
   `waddle.host_grant.witness.write_failures` are recorded by the server and
   cited by the restore runbook, but are not in the
   [metrics reference](server-metrics.md).
7. **W11 and W12.** Their changes do not exist yet; this runbook is reviewed
   again once they are merged, in particular the advertisement check under
   [S2S stays off](#s2s-stays-off).
