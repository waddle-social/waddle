# Reuse immutable charts before publishing GitOps

## Problem and evidence

The publisher for merged PR #1939 pushed the server image, then Helm failed to
tag chart `0.4.7`: GHCR reported its newly uploaded manifest as not found. The
same digest can now be pulled. Its unpacked files are identical to the existing
`0.4.7` chart, so publishing that chart was unnecessary. The current script only
recovers explicit duplicate/conflict errors and stopped before guest publication
and the Flux artifact update. Production still runs revision `645db266`.

## Implementation plan

- Isolate chart publication in a Python standard-library helper called by the
  existing publisher. Keep its authentication and image/guest/Flux steps intact.
- Pull the exact existing chart version first. Compare the complete unpacked
  file contents without treating tar timestamps as chart changes. Reuse identical
  content; refuse changed content under the same version.
- Fail closed on unknown/authentication pull errors and malformed archives.
  Publish a genuinely missing version, with bounded retries for transient registry
  failures and a content check if another publisher wins the race.
- Re-read the exact version before retrying an ambiguous write, and verify remote
  contents after a successful write. These checks reject observed version drift;
  fully atomic tag immutability requires registry policy or serialized writers.
- Exercise the actual CUE publisher chart step with a fake Helm CLI and real tar
  archives. Reproduce the captured tag-not-found failure before modifying it.
- Run those regressions in the existing deployment-render gate and include the
  helper in publisher task inputs. Validate generated workflow/lock consistency.

## Delivery and acceptance

User authorization is to repair publishing so GitOps picks up the merged change.
After verification and CI, merge this fix and monitor the main publisher. Verify
the pushed guest digests and Flux artifact, then the production HelmRelease and
all server pods at the new digest-pinned revision. Confirm that revision contains
PR #1939 and that the `Recreate` cutover, migration and guest loading succeed.
Keep the deployment UUID and existing cutover revision guard. Do not manually
patch pod images or restore `RollingUpdate` as part of this repair.

The missing-manifest condition is consistent with transient registry visibility,
but the original CI log has no HTTP trace proving its underlying cause. The fix
removes the unnecessary upload in this incident and bounds retries for new charts.
