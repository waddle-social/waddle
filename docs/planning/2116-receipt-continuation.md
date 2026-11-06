# Live receipt continuation

The ordinary `IngressAuthority::execute` path can retain a complete set of
already-proven, generic effect receipts before awaiting their pooled inserts.
The executor applies the existing all-sibling proof and arm-owned exclusions
first, and transfers the resulting immutable keys together in the same poll.
Remote carbon fanout, observer batches, frame completions, nested execution,
recovery, and direct callers of `execute_effects` keep their existing paths.
Attested PostgreSQL pools and node-bound clustered authorities keep the existing
executor path. Standalone authorities without a bound node fatal-fence token
also use the existing path.
The server's `open_ingress_authority` binds the actual `AppState` lifecycle
token before publishing the handle. The continuation uses only ordinary owner
PostgreSQL and SQLite pools in standalone mode, including standalone operation
of a binary built with the `clustering` feature.

The authority owns eight bounded workers. Its 256-batch and 4096-receipt limits
include in-flight work, and rejection leaves the eligible set unresolved and
the execution outcome uncertain. Capacity is acquired only after the effect,
so this is not a guarantee that every completed effect obtains custody. Each
retained batch holds authority admission and the workers retry only the exact
idempotent pooled inserts, never the effect or message terminalization. The
existing epoch-first `READ COMMITTED` PostgreSQL writer and same-pool SQLite
writer are unchanged. A failed or timed-out attempt remains unknown. A later
acknowledged insert establishes that the exact receipt is present on the
selected database; it does not establish whether an earlier COMMIT succeeded.

Graceful drain closes admission to the worker and joins retained work within
the supplied budget after admitted executions have handed over their proofs.
A timed-out drain returns false and signals force-stop;
the in-flight insert keeps its admission guard until that attempt completes or
its five-second attempt budget expires. The owner must retain the authority and
call drain again to join the worker. If force-stop abandons any retained proof,
the later join still reports false. Dropping an authority after an incomplete
drain is not proof that custody or persistence completed. The queue is in
memory and does not survive process loss. A blocked key occupies one worker;
enough simultaneously blocked batches, including duplicates of one key, can
exhaust the finite worker pool. Workers check the bound fatal fence before
each insert attempt and observe it during retry waits. This check admits an
attempt but does not linearize PostgreSQL transaction start against the fence:
if fatal cancellation races just after the check, the attempt may acquire a
connection, begin, or commit afterward. The worker awaits it for at most five
seconds, which does not prove the backend has terminated. A timed-out or lost
result remains unknown; process replacement requires its own fencing.

The focused tests cover real SQLite and ordinary PostgreSQL authority execution
after caller cancellation, an injected lost acknowledgement following a
successful insert, capacity rejection, and timeout followed by force-stop and
rejoin. They also exercise fatal fencing before the first insert and before a
retry, plus standalone and node-bound authorities in a clustering-enabled
build. A separate PostgreSQL test boots the shared owner pool, confirms the
authority and worker retain its reserved ingress lane, and cancels the caller
after proof. A synthetic decision with real stored intent keys tests multi-key
admission, shared-sibling eligibility, and the arm-owned exclusion. The
injected acknowledgement loss exercises retry behavior; it is not a
network-level PostgreSQL COMMIT fault or failover test.
