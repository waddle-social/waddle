# Federation grant authority: recovery experiment

This test-only increment builds on the grant-fence experiment in PR #2050.
Both candidate authorities can close the PostgreSQL admission guard during a
SpiceDB outage; that does not by itself establish where a grant mutation was
committed or whether its projection is recoverable.

Model three distinct evidence stages: admission closed for the exact scope and
generation; the selected authority durably recorded the exact canonical
mutation; and the corresponding projection was verified. A stage is evidence
for this experiment, not a production API or a promise about external bytes.

For the PostgreSQL-canonical candidate, recovery may rebuild the current
projection only from the exact current authoritative SQL intent. For the
SpiceDB-canonical candidate, write the exact operation marker and tuple together
in SpiceDB; SQL carries guard and mirror metadata, not reconstructible grant
authority. Missing canonical state stays closed until trusted restore or an
explicit fresh regrant. Never infer a successful mutation from a timeout.

Use real PostgreSQL and SpiceDB with finite contenders and deterministic
barriers to examine outage at grant/revoke, death after an acknowledged external
write before activation, current tuple/marker loss, stale recovery after a newer
revoke, and isolation of an independent scope. Retain the previous experiment's
race and cleanup cases. Report the exact stage reached and expected outcome for
each authority; do not treat an acknowledged write as a simulated lost reply.

This experiment does not select a production authority or prove writer closure,
multi-edge graph recovery, schema immutability, physical restore safety,
actor/claim integration, final byte draining or federation readiness. Existing
room-manifest acceptance requires separate characterization through the actual
ingress and replay code; a standalone admission ledger cannot prove that
contract. No production routing, schemas, permissions or activation change.
