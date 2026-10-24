# Legacy first-party push targets: issue #2016

This record separates available aggregate evidence from historical attribution.
It contains no owner, endpoint, node, job or message identifiers. No production
state was changed for this implementation.

## Available observations

The [issue #2016 investigation](https://github.com/waddle-social/waddle/issues/2016)
records a repeatable-read, read-only observation at **2026-10-23 10:34 UTC**:
three retained exact-FK failures shared a target whose local registration was
enabled and whose provider node was absent. There was exactly one enabled local
registration missing a provider node. Its creation/update dates and opaque node
shape were consistent with registration before local node provisioning was
required by [#621](https://github.com/waddle-social/waddle/commit/3c9bc9f90).
That is a plausible legacy origin, not proof of deletion or of the incident's
event-time registration state.

The [later production verification](https://github.com/waddle-social/waddle/issues/2012#issuecomment-6374774878)
at **2026-10-23 21:05 UTC** explicitly identifies deployed revision
`fab157563887cdb7fe606fd22d93912bbe4da452` (#2014). A subsequent admission for
the historical target failed as `registration_gone` with zero attempts and no
acceptance uncertainty, and its registration became disabled. Enabled local
missing-node registrations fell from one to zero. The 13 prior uncertain outbox
jobs remained unchanged. This verifies handling on that deployed revision; it
does not identify the revision serving the earlier October 21 event.

## Historical limits

None of the retained exact-FK jobs in the earlier observation had its final
transition within the reported October 21 11:25–11:32 UTC incident window.
Repository deployment configuration and commit dates alone cannot establish the
image serving that event. The available evidence does not establish that event's
deployed SHA. The current schema has no lifecycle audit proving whether the
historical node was deleted, pruned, or never provisioned. Historical use of the
older retirement implementation remains unproven.

Keep any future deployment/lifecycle evidence in restricted operator storage;
publish only revision, observation time and aggregate conclusions. A fresh
zero-orphan snapshot proves current state only. It cannot reconstruct deletion
history or establish what was enabled during a historical send.

## Regression and operating policy

Native SQLite/PostgreSQL regressions exercise actual retained-node pruning with
legacy enabled registrations, explicit configured retirement and provider
custody. Automatic pruning protects enabled configured-local registrations and
retains delivery evidence; it has no explicit retirement authority. Pre-existing
missing or inactive registrations are surfaced as aggregate diagnostics.

Use the [recovery and retention policy](../push-retry-policy.md#legacy-registrations-and-disabled-node-retention)
to distinguish ordinary gone-target handling from explicit custody retirement.
