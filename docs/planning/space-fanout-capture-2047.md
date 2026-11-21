# Space fanout subscriber capture: the fence test

Fan-out from a Space node lists the node's deliverable subscribers, builds the
event and enumerates each subscriber's resources before it hands anything
over. This suite protects the distinction between a stored membership change,
a captured subscriber list and what is handed to a subscriber's queue.

Until the revocation slice of #2047 the suite was an unsafe negative control
(#2056): a subscriber banned after the capture still received the event. The
same schedule is now a passing fence test. Fan-out from a node of the Spaces
service reads each captured subscription again
(`PubSubStorage::subscription_deliverable`) immediately before the first
hand-over to its subscriber, and hands nothing to a subscription that no
longer exists, is no longer subscribed or whose entity is an outcast. A read
that fails withholds the event from that subscriber.

`crates/waddle-server/tests/xep0060_space_fanout_fence.rs` runs the production
fan-out with the real `DatabasePubSubStorage` on a PostgreSQL database of its
own. A delegating adapter pauses exactly one `list_deliverable_subscribers`
call after its SQL query has returned and returns those unchanged rows after
release; every other storage operation delegates to the same real
implementation.

The schedule commits a Space item with two subscribers, starts its fan-out and
observes the subscriber-query barrier. An independent storage handle then
commits Outcast for one captured subscriber and reads it back, and commits a
late subscription. While the original fan-out is still paused, another Space
node completes a fan-out and its notification is accepted by a queue.
Resuming the original call hands the event to the subscriber that is still
deliverable, hands nothing to the banned one and does not add the late
subscriber. A later fan-out reaches the two subscribers that remain. A second
case removes a captured subscription instead of banning its entity.

What the test does not establish:

- The acknowledged operation is the PubSub storage write, observed in
  PostgreSQL. A PubSub affiliation or subscription change on a Space that no
  host-grant scope enrolls has no replica acknowledgement: its fence is its
  storage commit, decided against at the hand-over, so a hand-over whose read
  preceded the commit can still follow it. The acknowledged revocation fence
  covers enrolled scopes
  ([host-grant design](../../server/docs/planning/host-grant-authority-2047.md#revocation-fence-decisions)),
  and the Space node of an enrolled scope delivers nothing at all.
- Queue acceptance is not durable stream-management custody, a socket write
  or recipient observation. The detached stream-management append that
  follows a closed or absent live queue is behind the same read and is not
  driven by the test.
- The other fixture services use the test-state defaults; no SpiceDB
  authority, S2S, actor restart, clustered fan-out, datastore outage or
  end-to-end XMPP claim is made. Independent progress assumes healthy storage
  and means another node under the same Spaces service.

Run `bash server/experiments/space-fanout/run-native.sh` from the repository
root. The runner pins the repository Rust/Nix inputs and PostgreSQL 17.11,
requires the exact two-test inventory and two executed passes with no ignored
test, and sets `WADDLE_TEST_REQUIRE_POSTGRES=1` so that a missing PostgreSQL
fails. It uses a private mode-0700 Unix-socket cluster, local trust
authentication without password material, no TCP listener and bounded
coordinated PostgreSQL shutdown; an unconfirmed shutdown retains the private
directory and fails the run. The suite also runs with the other PostgreSQL
integration suites, where each case creates and drops its own database.
