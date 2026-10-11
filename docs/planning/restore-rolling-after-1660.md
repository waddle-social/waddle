# Restore RollingUpdate after the verified effect-authority cutover

The final Recreate server revision is e4d1f48cd77607061c559248b4e43b7e987bf924.
Both replicas are updated, ready and available on its pinned image, with zero
restarts. V1025, rebuilt guests, stable UUID, observer generation 2 and the
unchanged valid coalescing index are verified. Authenticated live login/binding,
ping/discovery, isolated self-message, acknowledgement counts and cross-pod
stream resumption passed, followed by clean smoke-resource shutdown.

Change only the production HelmRelease update strategy to RollingUpdate with
maxSurge 1 and maxUnavailable 0. Keep the cutover guard enabled and chart floor
unchanged. Do not include server/build input edits: they would advance the floor
and require another Recreate rollout before this return to rolling updates.
This planning document is outside publisher server/build inputs.

Before merge, run cutover-history tests and verify the live Helm server dry-run
accepts the exact deployed fleet and renders the verified-cutover annotation.
Use sanitized output; do not print Secret manifests or runtime credentials.
After required CI, merge through the normal queue, monitor the main publisher,
reconcile GitOps if necessary, and verify RollingUpdate, surge/unavailability
limits, guard marker, all final pod source/image pins and readiness. Run a final
live XMPP smoke test and record deployment evidence. Keep workload availability
at two ready replicas during the rolling transition.
