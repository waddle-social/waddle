# Production Space parent-move characterization

This test-only characterization drives a typed PubSub publish IQ through the
active bound-session frame handler. It uses production PostgreSQL PubSub/link
stores, the production PermissionActor and the production SpiceDB schema. Both
AppState and protocol PubSub references share one fully delegating real storage
wrapper. No production behavior, schema, authority, or activation changes.

The wrapper pauses exactly one completed stale-bookmark `get_items` call with
the old node, one room ID and `Some(1)`. It returns the real rows unchanged.
While paused, separate fresh reads observe both old and new graph parents and
bookmarks, with the SQL discovery link already pointing to the new Space.
Two schedules remove the old-only member before the move or during that pause.
The deletion ACK, absence of the member relation and fresh fully consistent
permission denial are checked. The new-only member keeps inherited access;
a separate Space mutation and fresh check progress while the mover is paused.
The exact publish result, final parent/bookmark/link state, frozen payload and
absence of direct channel grants are asserted. The random non-default channel
avoids deployment fallback channels. Probe users have no alternative grants.

These are scoped sequential reads, **not one common graph or SQL snapshot**.
A bound authenticated test session is supplied; no SASL exchange, WebSocket
socket, MUC join, MAM read, final-admission fence or federation is tested. The
intermediate overlap is current behavior, not a promised future access policy.
No canonical authority choice is established by either outcome.

Run `bash server/experiments/parent-move/run-native.sh`. Its pinned pure Nix
shell requires Linux, `/proc`, native PostgreSQL 17.11 and SpiceDB 1.56.1. It
executes exactly two ignored native cases and the shared fixture unit suite;
missing prerequisites fail. Each case writes secret-scanned private evidence
under `/tmp/waddle-fence-evidence-*`, including exact HEAD/status digest,
binary versions, outcome and verified cleanup. The runner requires a clean
committed tree and compares a digest of all tracked source bytes before/after.
`PARENT_MOVE_DEVELOPMENT=1` permits explicitly nonformal dirty-tree development.

The shared #2059 fixture owns all service subprocesses. No fixture controller
is copied here. Its sticky failure/closed spawn gate, normal PostgreSQL shutdown,
owned descendants, bounded deadlines and retention on uncertainty remain in
force. Joined bounded futures are cancelled together on a case failure; test
actors are stopped before verified fixture cleanup. Credentials remain in
memory or private child input/environment and never enter controller argv,
logs or evidence. Shared fixture limitations, including trusted workers and
no host-crash guarantees, still apply.

The Linux-only dev dependency is excluded from the server workspace. All three
Nix source closures include the fixture plus its owning workspace manifest so
Cargo can resolve it. Release targets have no normal dependency edge to it.
Dedicated PR and main/manual pipelines run this nonpublishing proof; normal
release/check jobs retain their existing behavior and resolve the dev edge.
