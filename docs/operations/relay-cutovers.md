# Relay cutovers and the RollingUpdate guard

Remote-resource relay endpoints have
[version compatibility handling](remote-resource-relay-upgrades.md). A future
change without a compatible delivery path still requires a `Recreate` deployment. Do not combine that cutover with the return to
`RollingUpdate` in one commit.

The server publisher and Helm chart enforce the return to rolling updates:

1. `server/scripts/cutover_revisions.py` reads complete first-parent Git history.
   It locates the most recent Recreate window and its last server source change,
   then emits that revision and its first-parent descendants. A canceled build,
   a PR description, and a YAML comment do not establish that an image rolled.
   A flip commit that also changes server/build inputs is rejected: those
   changes must first ship in a separate Recreate commit.
2. Publication writes these revisions to `cutoverGuard.allowedRevisions` in the
   generated Flux artifact. It also pins the image digest and sets
   `WADDLE_GIT_SHA`, using the same checkout as the image build. Checked-in values
   enable the guard, and the HelmRelease requires chart 0.4.6 or newer.
3. Before the first rolling upgrade in this cutover window, Helm reads the live
   Deployment and pods. The Deployment must have observed its latest generation,
   with all replicas updated, ready, and available. Exactly that many matching
   pods must exist, all running, ready, and nonterminating, with digest-pinned
   server images and an allowed `WADDLE_GIT_SHA`.
4. A successful check records `waddle.social/verified-cutover` on the Deployment.
   Later ordinary rolling upgrades can repair unhealthy pods without rechecking
   the completed cutover. Recreate removes this marker; a new cutover also
   changes the required revision and invalidates any older marker.

The check runs before Helm changes any resources. It also catches the case where
Flux never received the Recreate artifact: the still-running pre-cutover image
has an older SHA even though the live Deployment still says RollingUpdate.
Failure leaves the existing Deployment untouched. Restore Recreate and publish
an image containing the cutover, wait for its rollout to complete, then return
to RollingUpdate. If no server image was published for the final server change
in the Recreate window, publish one before flipping.

The guard uses Helm's existing Kubernetes credentials, which need `get` on the
Deployment and `list` on pods in the release namespace. No pod-exec permission,
new credential, image, Kubernetes hook, or external attestation service is used.
A lookup error or missing object fails closed. A fresh install has no previous
fleet to protect and skips the live check. Plain `helm template` renders an
install; `helm template --is-upgrade` fails without live objects. Use
`--dry-run=server` for a real cluster upgrade preview, as described in the
[Helm lookup documentation](https://docs.helm.sh/docs/v3/chart_template_guide/functions_and_pipelines/#using-the-lookup-function).

The publisher rejects shallow history (and fetches complete history in CI) or a
RollingUpdate history without a Recreate window. It deliberately includes all
server-tree changes in the cutover floor: requiring a slightly newer image is
safer than overlooking a wire change. The accepted revision list grows within
the current cutover window and resets at the next Recreate window.

Run the guard regression tests with:

```sh
python3 -m unittest discover -s server/scripts -p 'test_cutover_*.py'
```

These tests cover Git histories with canceled builds, multiple cutovers, server
changes during Recreate, unrelated commits, and shallow checkouts; Helm fixtures
cover mixed images, incomplete rollouts, absent pods, terminating pods, mutable
images, missing source SHAs, and reuse/reset of the completed-cutover marker.
They run in the existing `renderDeployment` CI task.

## Occupancy authority cutover (#1733, PR #1869)

This release requires a one-shot `Recreate`, committed in the production
HelmRelease. Global migration **V0013** creates `xmpp_occupancy_authority`, which
records the current bind generation for each full JID. Fresh binds publish this
authority; resumes and relayed registrations only verify it. The registration
endpoint changes from `waddle.clustering.relay.remote_resource_register.v2` to
`waddle.clustering.relay.remote_resource_register.v3`, without a v2 compatibility
handler. `waddle.clustering.relay.remote_resource_force_detach.v3` likewise
replaces v2 because the serialized force-detach origin gains a fresh-bind
replacement variant. Stop every old replica before starting new replicas: an
old writer neither publishes nor checks the new authority, and mixed peers
cannot register or force-detach remote resources through the same endpoints.
The rolling compatibility introduced by [#1841](https://github.com/waddle-social/waddle/pull/1841)
covers live resource routing and frame delivery. It does not provide compatible
receivers for these registration changes or generation fencing in old writers.

The migration adds an empty authority table and does not backfill old sessions.
A detached session created before the cutover has no matching authority row and
cannot resume. The server returns XEP-0198 `<failed/>` with `item-not-found` and
the server's handled count, leaving the authenticated connection open for a
fresh bind. Clients can retain and retry their unhandled outbound tail, then
rejoin their rooms.
Expect the connection interruption inherent in `Recreate`. The additive DDL
does not make a rolling upgrade safe.

A fresh bind discovers and retires locally held predecessor snapshots by their
exact generation, including snapshots without an authority row. SM maintenance
also promotes queues from locally owned detached snapshots whose generation is
missing or superseded, without waiting for their resume timeout. Both paths
verify the snapshot's SM owner and claim epoch before taking custody. Snapshots
owned by another node follow the existing ownership recovery path; a bind does
not synchronously discover or steal every foreign snapshot. Recovery timing
therefore depends on claim recovery and maintenance as well as reconnects.

Deployment and verification are a separate operator action; implementation and
tests do not deploy this change:

1. Publish the image and Flux artifact containing V0013, both v3 endpoints, and
   the committed `Recreate` strategy. Keep the deployment UUID unchanged.
2. Wait until every server replica runs the digest-pinned cutover image, reports
   its expected `WADDLE_GIT_SHA`, and is ready. Confirm migration version 13 is
   recorded in the global `_migrations` ledger and fresh binds populate
   `xmpp_occupancy_authority`. A ledger-wide `MAX(version)` is insufficient:
   the channel/message namespace already includes V1020.
3. Verify fresh binds, cross-node remote-resource registration, MUC joins, and
   same-full-JID replacement cleanup. Verify resumption of a session created by
   the new fleet. A pre-cutover resume must receive SM `<failed/>`, retain its
   unhandled tail, and allow a fresh bind on the same authenticated connection.
   Check pending-message promotion and reconnect-time database pool health.
4. Only after verification, use a separate follow-up change to restore
   `RollingUpdate` with `maxSurge: 1` and `maxUnavailable: 0`. Keep server and
   build changes out of that flip-back commit. The publisher and live Helm
   guard above must confirm the cutover actually reached every replica.

Roll forward if this release fails. A binary whose global migration catalog
ends at V0012 refuses startup against a ledger containing V0013; returning the
strategy to `RollingUpdate` does not authorize a binary rollback. Do not remove
the ledger row or authority table to bypass this fence. Never roll back before
the append-only migration-ledger guard introduced in `43860571` (#1671): those
older binaries can destructively recreate database tables, as documented in
the [ingress authority runbook](../../server/docs/operations/ingress-authority.md).

The authority table currently retains one row per full JID. Garbage collection
is a separate follow-up: deleting a row on disconnect would invalidate valid
resumes. Safe reclamation must prove terminal cleanup and absence of resumable
state or in-flight work, then delete only the unchanged generation.
