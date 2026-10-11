# Relay cutovers and the RollingUpdate guard

## Extension effect authority cutover (#1660)

V1025 changes collection authority and the guest ABI becomes
`waddle:extension@3.0.0`. The committed HelmRelease uses `Recreate`: stop every
older binary before the new binary writes descendant custody or advances the
retention frontier. An already-running V1024 collector still uses the old
terminal-time policy; additive schema alone does not make mixed collectors safe.

Publish the reviewed server image and rebuilt, digest-pinned guest components
together. Preserve the deployment UUID. Verify the migration ledger, observer
and notification recovery, and that unresolved descendants/SM references retain
aliases, delivery bindings and receipts. Verify the rebuilt guests load and
replay their approved payloads. Keep deployment evidence separate from merge CI.

Restore `RollingUpdate` only in a separate change after fleet verification,
using the existing cutover revision guard. An older image is not a valid binary
rollback after this roll-forward ledger migration. This document records the
required procedure; it does not assert or authorize a live cutover.

Before stopping the old collectors, record the terminal ingress backlog and its
`terminal_at` ages using the old schema. After V1025 adds `retention_eligible_at`,
measure the legacy NULL-frontier backlog as startup/maintenance begins; the new
column cannot be queried on V1024. These rows are maintenance
candidates for adoption, not immediate deletion: the collector processes bounded
batches and starts a fresh eight-day tail. During adoption the CNPG eligible
count includes these rows, and the oldest-age gauge falls back to `terminal_at`.
Old ages and little reclamation can therefore be expected while adoption drains;
legacy rows will not be deleted for at least eight days after adoption.

For this planned window, arrange a scoped, time-limited silence for
`IngressGcBacklog` and `IngressGcAge` if the baseline would trigger them. Keep
maintenance failures, missing queries and heartbeat alerts active. Verify that
the legacy NULL-frontier backlog decreases and unresolved custody stays retained;
remove the silence when adoption has drained. Renew a silence only after
investigating lack of progress. This procedure does not apply a live silence.

Startup quarantine preflight and ancestry adoption each revisit retained kind-7/21
intents on every boot.
Individual pages and transactions are bounded; the whole startup scan has no
fixed wall-clock budget. Measure startup duration against the retained backlog
before scheduling a Recreate window, and account for it in readiness deadlines.
On canonically matchable rows with compatible structural class/reason constraints,
malformed sender JIDs, durable delivery identities and suppression audits are
isolated without blocking healthy candidates or boot. Damaged canonical authority,
fences and incompatible database constraints still fail closed.
After notification schema initialization, inspect the count of candidates with
`quarantined_at_ms IS NOT NULL`. Quarantine preserves the row, existing lineage
and a pending Foundation reference. Invalid suppression audit text is moved into
`quarantined_suppressed_reason` before clearing its active field, so new CHECKs can
validate the row without erasing the audit. Ordinary worker dispatch and pruning
must not consume it. Repeated startup, repaired fields or elapsed retention time
do not release quarantine custody automatically. Investigate these rows and
require a verified explicit repair/disposition before releasing their evidence.

Keep publisher-triggering server/build changes out of the gap between verified
fleet rollout and the RollingUpdate flip. Such a change advances the guarded
cutover revision and must finish its own Recreate rollout first. Unpublished
`server/docs/**` changes are excluded from that floor, as described below.

## RollingUpdate guard

Remote-resource relay endpoints have
[version compatibility handling](remote-resource-relay-upgrades.md). A future
change without a compatible delivery path still requires a `Recreate` deployment. Do not combine that cutover with the return to
`RollingUpdate` in one commit.

The server publisher and Helm chart enforce the return to rolling updates:

1. `server/scripts/cutover_revisions.py` reads complete first-parent Git history.
   It locates the most recent Recreate window and its last publisher-triggering
   server/build input change, then emits that revision and its first-parent
   descendants. A canceled build,
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
RollingUpdate history without a Recreate window. Its server/build inputs match
the server and flake paths that trigger `waddle-server-default.yml`, with a
regression test checking that the two policies agree. Unpublished changes such
as `server/docs/**` and server agent guidance do not advance the cutover floor
and may accompany a flip. Published source, configuration, charts, scripts,
schema, extensions, and WIT changes still require a Recreate rollout first.
The HelmRelease remains a separate cutover-history input even for manifest-only
changes. The accepted revision list grows within the current cutover window and
resets at the next Recreate window.

Run the guard regression tests with:

```sh
python3 -m unittest discover -s server/scripts -p 'test_cutover_*.py'
```

These tests cover Git histories with canceled builds, multiple cutovers, server
changes during Recreate, unpublished docs and guidance, publisher-input drift,
unrelated commits, and shallow checkouts; Helm fixtures
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


## Ingress send and observer cutover (#1776, PR #1899)

This release commits a one-shot `Recreate` in the production HelmRelease.
The remote-user side-effect endpoint changes from
`waddle.clustering.relay.remote_user_side_effect.v3` to v4 to carry the
recorded carbon obligation. The previous endpoint has no compatible receiver.
Roster pushes, blocklist pushes, and carbons therefore cannot cross an old/new
replica boundary. Existing resource-route/frame compatibility does not cover
this endpoint. Stop every old replica before starting the new fleet; expect a
brief connection interruption and client reconnects.

The same boundary protects observer work. V1022 moves the ownership-column
upgrade and legacy-attempt handling into the append-only migration ledger.
Startup initialization no longer rewrites existing work. Previously attempted
ownerless work becomes `started`, preserving its body, attempt counter, token,
and existing lease expiry. A missing expiry gets the database time plus three
minutes. Pristine pending work remains eligible. The upgrade runs once, after
old callback workers have stopped.

A started observer attempt can be claimed with a new token after its lease
expires. This prioritizes eventual processing over at-most-once guest effects:
a callback may repeat, but a displaced token cannot publish results or settle
receipts. After twenty attempts the work settles as `retry_exhausted` rather
than remaining indefinitely ambiguous. Live-delivery recovery and its possible
duplicate notification tradeoff are documented in the
[ingress authority runbook](../../server/docs/operations/ingress-authority.md).

1. Publish the image and Flux artifact containing V1021/V1022, the v4 endpoint,
   and the committed `Recreate` strategy together. Keep the deployment UUID.
2. Wait for every old replica to stop and every new replica to become ready on
   the digest-pinned cutover image. Check `WADDLE_GIT_SHA` and ledger versions
   1021 and 1022 explicitly.
3. Verify cross-node roster/blocklist pushes and carbons, client reconnects,
   and observer processing. Expired legacy attempts must become retryable;
   repeated startup must not quarantine work again. Verify a completed observer
   result is published once even when an older token returns late.
4. Restore `RollingUpdate` only in a separate follow-up after fleet verification,
   with no server/build changes in that flip-back commit. The existing publisher
   and live Helm guard must verify the completed cutover.

Roll forward on failure. Pre-cutover binaries cannot restart against the
advanced migration ledger; do not delete ledger entries to permit rollback.
The SQLite migration targets released databases without the new ownership
columns. An unreleased PR database that already has those columns but lacks
V1022 fails closed and requires deliberate development-database repair; the
migration does not silently discard its ownership evidence.
