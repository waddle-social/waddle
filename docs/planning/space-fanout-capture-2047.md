# Space fanout subscriber-capture negative control

This test-only characterization protects the distinction between a stored
membership change, a captured subscriber list and protected-effect admission.
It does not select a grant authority or change the existing fanout contract.

The dedicated native PostgreSQL test runs production `fan_out_publish` with the
real `DatabasePubSubStorage`. A delegating test adapter pauses exactly one
`list_deliverable_subscribers` call after its SQL query has returned; it returns
those unchanged rows after release. Every other storage operation delegates to
the same real database implementation. No production hook or routing logic changes.

The deterministic schedule commits a Space item, starts its fanout and observes
the subscriber-query barrier. An independent storage handle then commits Outcast
for the captured subscriber, reads it back and proves that a fresh deliverable
query excludes that account. A late subscriber is committed separately. While
the original fanout is still paused, another Space node completes an affiliation
mutation and actual notification queue acceptance. Resuming the original call
queues the exact captured item to the now-Outcast resource and excludes the late
subscriber. A later fresh fanout reaches the late subscriber and excludes Outcast.

The stale notification is an expected **unsafe negative control** for a future
community grant fence. Passing means that this current behavior was observed;
subscriber capture is not asserted to be authorized admission or guaranteed
future policy. The acknowledged operation is the production PubSub storage
Outcast write, independently observed in PostgreSQL; this is not an IQ success
or a demonstrated canonical community revoke across SpiceDB and all replicas.

Assertions compare the complete typed event, payload, item ID, node, sender,
resource destination and direct-frame queue metadata. They inspect real
connection-registry queues with production dual registration. Queue acceptance
is not durable SM custody, a socket write or recipient observation. Other fixture
services use the existing test-state defaults; no production SpiceDB authority,
S2S, actor restart, clustered fanout, datastore outage, cancellation recovery or
end-to-end XMPP claim is made. Independent progress assumes healthy storage and
means another node under the same Spaces service, not another deployment.

Run `bash server/experiments/space-fanout/run-native.sh` from the repository root.
The runner pins the repository Rust/Nix inputs and PostgreSQL 17.11. It requires
the exact one-test inventory and one executed pass with no ignored test; missing
PostgreSQL configuration fails rather than silently skipping. General test lanes
ignore this native prerequisite case; its separate nonpublishing CI lane runs it.
The runner uses a private mode-0700 Unix-socket cluster, local trust authentication
without password material, no TCP listener and bounded coordinated PostgreSQL
shutdown. Unconfirmed shutdown retains the private directory and fails the run.
Uncatchable host/process termination requires external cleanup. The fixture and
runner are adapted from the reviewed #2052 native room-child characterization,
without importing its production or accepted-child assertions.
