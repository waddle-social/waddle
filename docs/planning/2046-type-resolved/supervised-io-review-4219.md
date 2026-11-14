# Supervised I/O test-target markers: partial source review

This note reviews the **105** marker rows whose `compiled_target_kind` is
`test` in the supplied supervised-I/O capture. It groups the scenarios and
traces consequential source edges; it is **not** an individual semantic review
of all 105 rows, a complete test or caller inventory, or proof of test
execution. All statements about tests below describe source setup and
assertions, not observed test results. The integrator identified the source as
`4219bdd`; this reviewer did not independently verify Git provenance.

## Scope, configuration, and scenario counts

The capture contains 3,356 rows in total; filtering `compiled_target_kind=test`
selects **105 distinct marker keys** on **66 file/line pairs** and 30 tags.
All 105 are in
[`tests/supervised_io/connection_actor.rs`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L50),
with target `test / supervised_io / test=true`, features `["test-support"]`,
and source hint `test`. The test crate is explicitly
[`cfg(feature = "test-support")`](../../../server/crates/waddle-server/tests/supervised_io.rs#L3)
and includes that module at `:5-6`; the server feature enables
[`waddle-xmpp/test-utils`](../../../server/crates/waddle-server/Cargo.toml#L176).
These are capture/compiled-marker facts, not a fresh compilation or run.

| Enclosing source scenario | Marker rows | Distinct source lines | Source scenario |
| --- | ---: | ---: | --- |
| Imports at `:50-54` | 9 | 4 | Type imports; no separate runtime operation at those spans |
| [`real_socket_actor_owner_keeps_exact_envelope_until_joined_terminal_settlement`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L1341) | 42 | 28 | Refused provisional candidate, active dispatch, write and terminal inventory |
| [`actor_owner_panic_keeps_unwritten_envelope_in_app_held_terminal_inventory`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L1687) | 17 | 10 | Actor-owner panic with accepted but unwritten envelope |
| [`real_socket_stop_at_physical_attempt`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L1791) | 19 | 12 | Shared helper for before-attempt and after-attempt stop tests |
| [`real_socket_publication_loss_compensates_exact_candidate`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L1944) | 18 | 12 | Shared helper for mirror timeout, caller cancellation and lease revocation |

The helpers are invoked by separate test functions at
[`1928`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L1928),
[`1933`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L1933),
[`2083`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L2083),
[`2088`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L2088),
and [`2094`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L2094).
There are no additional marker rows in the neighboring tests merely because
they share this file. Multiple rows can name different captured targets at one
line, so these counts are not numbers of operations or tests.

## Fixture boundary and source assertions

The shared [`actor_owner_route`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L581)
uses `TestSocketAdmission`, a custom `/actor-owner` Axum route and the
[`start_actor_local_endpoint` test interface](../../../server/crates/waddle-server/src/websocket_transport.rs#L908).
The examples use a Tokio in-memory duplex stream with a WebSocket upgrade, as
shown at [`tests:1352-1367`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L1352).
This is a physical WebSocket fixture, not a deployed network `/ws` test. The
[actor-local endpoint module](../../../server/crates/waddle-server/src/server/routes/websocket/actor_local_endpoint.rs#L1)
says the owner is release compiled but not selected by `/ws`; its publication
fixture is separately `test-support` gated.

- **Conflict, provisional refusal, and active path (42 rows).** At
  [`tests:1370`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L1370)
  an unrelated channel is used to assert `WrongOwner`. The initial publication
  passes a nonmatching expected predecessor and asserts a registry conflict,
  sealed custody and absence of an entry at
  [`:1378-1404`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L1378).
  It retries creation of a fresh channel while sealed prior custody is not yet
  ready for replacement (`:1406-1416`); that loop is not a retry of an
  uncertain wire write.
  A second publication is paused after mirror acceptance. The ask at
  [`:1457`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L1457)
  asserts a provisional refusal preserving owner, stanza and pending-row ID,
  and checks that a write-acceptance handle is present. It also asserts no
  queued ID and that the original acceptance receiver remains empty.
  After releasing publication, the ask at
  [`:1519`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L1519)
  asserts `AcceptedOrSettled`; the source then calls `write_next_direct`,
  checks `Flushed`, and reads a matching peer frame at `:1529-1551`. Crucially,
  its write-acceptance receiver is still empty at `:1538-1541`, and the joined
  terminal inventory retains the flushed envelope with an acceptance handle at
  [`:1583-1597`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L1583).
  A peer-visible frame, flush stage, acceptance callback and terminal
  settlement are different observations in this fixture.
- **Owner panic (17 rows).** The test constructs and publishes a candidate,
  asks for custody dispatch at
  [`tests:1719-1748`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L1719),
  removes the queued ID from the receiver and drops it, then panics the task
  holding the endpoint owner at `:1750-1760`. After the test waits for pump
  join, it drains the independently held app index and asserts one retained
  item with a matching stanza, `NotAttempted` and a present acceptance
  handle; the original acceptance receiver remains empty at
  [`:1763-1786`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L1763).
  Losing the ID from the actor stack is not represented as loss of the retained
  envelope in this source scenario.
- **Stop around physical attempt (19 rows).** The shared helper toggles its
  probe before the attempt versus after the attempt at
  [`tests:1791-1799`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L1791),
  publishes a test binding and asks custody dispatch at `:1825-1856`, then
  stops and releases the chosen probe at [`:1860-1885`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L1860).
  The source expects `NotAttempted` before the gate and `AttemptedUncertain`
  after the attempt at [`:1886-1894`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L1886),
  followed by a joined terminal drain preserving the envelope and unresolved
  acceptance at `:1899-1923`. `AttemptedUncertain` is not evidence of either
  peer receipt or safe replay; this helper does not automatically retry it.
- **Publication loss (18 rows).** The helper parks the actor with
  [`HoldUserActorForPublication` at `tests:1987`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L1987),
  waits until a provisional registry entry appears, and distinguishes
  `MirrorTimeout`, caller cancellation, and lease revocation at
  [`:2003-2053`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L2003).
  Its source waits for both registry absence and empty actor resources, then
  asserts sealed custody, no retained envelope and joined pumps at
  [`:2055-2077`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L2055).
  A timeout result alone is not that later compensation or join evidence.

## Consequential indirect edges and classifications

The following are source-level proposals for the cited operations, not blanket
labels for every row in a scenario:

1. **Test route to owner:** [`tests:601`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L601)
   -> [`TestSocketAdmission::start_actor_local_endpoint`](../../../server/crates/waddle-server/src/websocket_transport.rs#L919)
   -> [`ActorLocalEndpointOwner::start`](../../../server/crates/waddle-server/src/server/routes/websocket/actor_local_endpoint.rs#L816).
   This `test-support` entry constructs a supervised owner with independent
   cleanup indexing. Proposed class: `transport;migration-glue`; setup and
   type spans are distinct from physical I/O.
2. **Publication and compensation:** [`tests:1430`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L1430)
   and [`:2010`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L2010)
   -> [`publish_binding`](../../../server/crates/waddle-server/src/server/routes/websocket/actor_local_endpoint.rs#L1021)
   -> [`run_publication`](../../../server/crates/waddle-server/src/server/routes/websocket/actor_local_endpoint.rs#L1190).
   It publishes the provisional registry candidate, asks the child to mirror
   and confirm at [`:1217`](../../../server/crates/waddle-server/src/server/routes/websocket/actor_local_endpoint.rs#L1217)
   and [`:1243`](../../../server/crates/waddle-server/src/server/routes/websocket/actor_local_endpoint.rs#L1243),
   and then conditionally activates the confirmed candidate at
   [`:1262`](../../../server/crates/waddle-server/src/server/routes/websocket/actor_local_endpoint.rs#L1262).
   Failure or an undeliverable result seals the candidate and calls retirement
   at [`:1057-1065`](../../../server/crates/waddle-server/src/server/routes/websocket/actor_local_endpoint.rs#L1057).
   [`PublicationTicket::retire`](../../../server/crates/waddle-server/src/server/routes/websocket/actor_local_endpoint.rs#L216)
   retries exact registry retirement and owner-qualified actor unregister;
   its source also treats a stopped or not-running actor as a terminal ask
   outcome. Proposed classes: `authority;derived-index;migration-glue` for
   the publication and retirement chain; bounded send admission is not
   completed cleanup.
3. **Custody refusal and enqueue:** Test asks at
   [`tests:1457`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L1457),
   [`:1519`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L1519),
   [`:1742`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L1742),
   and [`:1850`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L1850)
   -> [`TrySendDirectWithCustody` handler](../../../server/crates/waddle-xmpp/src/registry/user_actor/delivery.rs#L269)
   -> [`try_deliver_with_custody`](../../../server/crates/waddle-xmpp/src/registry/user_actor/delivery.rs#L98).
   This dark local path preserves the exact envelope on proven refusal and
   returns `AcceptedOrSettled` for local acceptance or existing settlement;
   [the outcome contract](../../../server/crates/waddle-xmpp/src/registry/user_actor/delivery.rs#L132)
   explicitly does not prove a socket write. Proposed class: `transport`.
4. **Attempt, flush and terminal retrieval:** Test calls to
   [`write_next_direct`](../../../server/crates/waddle-server/src/server/routes/websocket/actor_local_endpoint.rs#L1151)
   at [`tests:1530`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L1530)
   and [`:1862`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L1862)
   derive a direct frame while the original envelope stays in custody. The
   transport's [`attempt`](../../../server/crates/waddle-server/src/websocket_transport.rs#L1316)
   invokes [`ActorOutboundCustody::attempt_with`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/actor_outbound.rs#L295)
   under the source/node/stop/phase gates, and the completed callback calls
   [`mark_flushed`](../../../server/crates/waddle-server/src/websocket_transport.rs#L1417).
   [`seal`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/actor_outbound.rs#L338)
   classifies queued work as not attempted; `take_terminal` refuses an
   in-progress attempt. The app's
   [`drain_joined_terminal`](../../../server/crates/waddle-server/src/server/routes/websocket/actor_local_endpoint.rs#L718)
   requires the record to be joined before extracting terminal IDs and
   envelopes. Proposed classes: `transport;authority`; a stage or join is not
   a recipient acknowledgement or a completed application-level settlement.

## Reproduction and limits

From the repository root, this reads the recorded capture and prints its
filtered counts and `enclosing_fn` groups without changing any file:

```python
import csv
from collections import Counter
from pathlib import Path
p = Path("docs/planning/2046-type-resolved/capture-4219bdd/"
         "test-support-supervised/w2046-4219-support-integration-supervised-uses.tsv")
with p.open(newline="") as stream:
    all_rows = list(csv.DictReader(stream, delimiter="\t"))
rows = [row for row in all_rows if row["compiled_target_kind"] == "test"]
fields = ("tag", "file", "line", "column", "compiled_target_kind",
          "compiled_target_name", "compiled_target_test_mode")
print("all", len(all_rows), "test", len(rows),
      "unique keys", len({tuple(row[k] for k in fields) for row in rows}),
      "source lines", len({(row["file"], row["line"]) for row in rows}))
print(Counter(row["enclosing_fn"] for row in rows))
print(Counter((row["compiled_target_kind"], row["compiled_target_name"],
               row["compiled_target_test_mode"], row["compiled_target_features"])
              for row in rows))
```

The recorded TSV's SHA-256 at inspection was
`c110dbfebc4bb5c7e8cc3fadec324c21b6bd57ce4ecb66c3edcb28b3409428a4`.
The capture and source assertions do not establish the outcome of a fresh
build, test, live `/ws` activation, remote delivery or end-to-end custody.
No Cargo, Git mutation, test, runtime or inventory-validator command was run;
no TSV or other repository file was edited for this review.
