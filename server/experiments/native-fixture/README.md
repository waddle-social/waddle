# Native PostgreSQL and SpiceDB test fixture

This unpublished Linux-only support crate belongs to the standalone grant-fence
workspace. It is not a production dependency. The consumer supplies its own SQL
and SpiceDB schemas; this crate contains no authorization, room or grant schema.
The only test schema is an empty object definition used by native cleanup tests.

Run the complete workspace, including this crate's tests, from a clean checkout:

```sh
bash server/experiments/grant-fence/run-native.sh
```

The existing pinned Nix shell supplies PostgreSQL 17.11 and SpiceDB 1.56.1.
Missing binaries or version mismatches fail. The workspace uses one authoritative
Cargo.lock in grant-fence. Its report fingerprints both source trees, manifests,
that lock, consumer schemas and runner, and repository Nix/toolchain inputs.
Both nonpublishing grant-fence CI lanes include this source path.

Fixture owns its generated private root, service handles and one closed/count/
first-failure mutex. Every process that may access that root reserves a count
before OS spawn. A pre-spawn permit may roll back if spawn never happened. On
successful spawn it transfers infallibly into Process before any fallible setup.
A live permit never decrements in Drop; only verified full process-group cleanup
or normal coordinated PostgreSQL completion releases it, idempotently. Poison,
count overflow/underflow, lost live permits and cleanup failures fail closed.

Shutdown closes reservation under the same mutex, releases the lock, stops owned
services and checks for outstanding children before removing data. A reservation
acquired before close may spawn afterward: its outstanding count retains the
root. A new reservation after close fails. No lock is held while spawning,
waiting or stopping. Consumers use Fixture::spawn_worker; Process construction,
raw Child and the gate are not public. Private source-provenance probes execute
fixed Git commands without fixture credentials or private-root access.

Automatic root deletion is disabled at creation, so an early return or unwind
cannot delete data through TempDir's automatic cleanup. Verified explicit shutdown
can still remove a safely stopped fixture after an unrelated startup error or
cancellation. Cleanup uncertainty, sticky failures, or outstanding reservations
detected during shutdown retain the root permanently, even when later cleanup
succeeds. An external worker may outlive Fixture; that is an error with retained
data, never permission to delete data under a running process. Tests deliberately
retaining directories remove only their own roots after separately verifying all
test children stopped.

Workers are trusted native test programs. Their descendants must remain in the
owned process group; arbitrary daemonization, escaping descendants, hostile same-
user processes and parent SIGKILL are not supervised guarantees. PostgreSQL has
its separate coordinated shutdown because its backends create different groups.
The existing WNOWAIT identity reservation, bounded waits, content-free diagnostics
and sticky failure rules are preserved. A reaped abnormal PostgreSQL exit is not
verified cleanup and cannot release through a subsequent idempotent stop.

Credentials are generated per fixture, non-Debug and never persisted in files,
arguments or reports. Workers receive only their serialized application settings
through bounded anonymous stdin; PostgreSQL initdb receives its password through
anonymous stdin and SpiceDB receives generated credentials through its private
child environment. PostgreSQL stores only verifiers. Native output is disabled,
fixture files and public evidence are scanned for exact generated secret bytes.
