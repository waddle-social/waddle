# Federation grant authority: recovery experiment

This test-only increment builds on the grant-fence experiment in PR #2050.
Both candidate authorities can close the PostgreSQL admission guard during a
SpiceDB outage; that does not by itself establish where a grant mutation was
committed or whether its projection is recoverable.

Record independent typed evidence facts keyed by candidate, scope, incarnation,
generation, operation and digest: admission closed by a committed PostgreSQL
fence; the selected authority durably recorded the exact canonical mutation;
and the corresponding projection was verified. Verification is not reopening
admission. These facts are experiment evidence, not a production API or a
promise about external bytes.

For the PostgreSQL-canonical candidate, persist the complete bounded leaf
intent, including subject, relation and polarity; recovery may rebuild the
current projection only from that exact current authoritative SQL record, not
from a digest or a worker argument. For the SpiceDB-canonical candidate, write
the exact operation marker and canonical grant or positive revoke tombstone in
one SpiceDB transaction. SQL carries guard and mirror metadata, not
reconstructible grant authority; the recovery process receives only identity
and digest and reads its authority from SpiceDB. Verify the marker and grant or
tombstone at one consistent revision or through a single schema conjunction.
Tuple absence alone is not evidence of a committed revoke. Missing canonical
state stays closed until trusted restore or an explicit fresh regrant. Never
infer a successful mutation from a timeout.

Use real PostgreSQL and SpiceDB with finite contenders and deterministic
barriers to examine outage at grant/revoke, death after an acknowledged external
write before activation, current tuple/marker loss, stale recovery after a newer
revoke, and isolation of an independent scope. Retain the previous experiment's
race and cleanup cases. Report the exact stage reached and expected outcome for
each authority; do not treat an acknowledged write as a simulated lost reply.
Loss tests begin after the scope is durably closed. Include an explicit
negative control or documented limitation for canonical state disappearing
while active after an allowed check: the current guard alone cannot detect
that corruption. A newer revoke must supersede a pending closed operation by
exact identity, keep the scope closed and advance its generation; test stale
activation both before and after the revoke's commit.
The recovery contender must be a fresh process and must not receive the
SpiceDB-canonical payload. Assert the closed fence before injecting corruption
or terminating a worker, and remove the tuple and marker independently. Test
independent-scope progress while the per-scope mutation is held and SpiceDB is
healthy; a globally unavailable SpiceDB cannot serve protected reads.

This experiment does not select a production authority or prove writer closure,
multi-edge graph recovery, schema immutability, physical restore safety,
actor/claim integration, final byte draining or federation readiness. Existing
room-manifest acceptance requires separate characterization through the actual
ingress and replay code; a standalone admission ledger cannot prove that
contract. No production routing, schemas, permissions or activation change.
