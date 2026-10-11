# Cutover startup repair

The first V1025 rollout reached the intended image but failed before serving.
The rebuilt Jev observer changes its identity while published generation remains
1. Advance the configured generation to 2 through the existing fenced transition;
do not rewrite persisted identity, drop custody, or roll back the binary.

Recover serving promptly by republishing the already successful Flux artifact
with only that generation change. Keep server and guest digests, source SHA,
deployment UUID and Recreate strategy unchanged. Record this operational
configuration repair in PR evidence and reconcile using the normal controllers.
Then ship the matching CUE change so subsequent publication preserves it.

The initial startup also queried newly introduced frozen outbox columns before
Outbox schema initialization. Gate only the outbox-dependent legacy binding until
its required columns exist; the established HTTP startup reruns adoption after
Outbox initialization. Preserve already recorded custody and exact-payload checks.
Add a pre-upgrade schema regression, review independently, and run required CI.
