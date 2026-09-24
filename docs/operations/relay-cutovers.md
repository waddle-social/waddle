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
