# Support-only endpoint and connection-state sites: partial review

This source review covers the **179** support-only capture keys outside
`waddle-xmpp/src/registry/user_actor.rs` and
`waddle-xmpp/src/registry/user_actor/implementation.rs`. It groups source
regions and checks selected caller edges; it does **not** claim an individual
semantic classification for every row, a complete indirect-edge inventory, or
validator coverage. The integrator identified the server source as `4219bdd`;
this review did not independently verify Git provenance. Recheck anchors and
feature conditions against the eventual source target before incorporation.

## Scope and capture meaning

Subtract production keys from support keys using exactly `(tag, file, line,
column, compiled_target_kind, compiled_target_name, compiled_target_test_mode)`.
The default difference is **352 distinct keys**. Excluding the 173 keys reviewed
in [support-actor-review-4219.md](support-actor-review-4219.md) leaves **179**
distinct keys, **157 distinct file/line pairs**, **82 distinct tags**, in
**20 files**. The clustering subtraction produced the same set of 352 keys
under this same key definition. Because the key omits profile and compiled
features, equal keys do not establish equal behavior or feature contexts.

Of these 179 support keys, 115 are `lib / waddle_server / test=false` with
`test-support`, and 64 are `lib / waddle_xmpp / test=false` with `test-utils`.
Their capture hints are 109 `prod`, 59 `test-support`, and 11 `test`; hints
are not cfg proof. For example, all 41 `connection_actor.rs` sites have a
`prod` hint, but the module is included as a
[`test-support` fixture](../../../server/crates/waddle-server/src/server/routes/websocket/connection_actor_core.rs#L29).
The server's [`test-support` feature enables `waddle-xmpp/test-utils`](../../../server/crates/waddle-server/Cargo.toml#L176).
The capture's `test=false` is the compiled library target mode, not evidence of
ordinary `/ws` reachability.

| Source group | Keys | Files and counts |
| --- | ---: | --- |
| Connection actor fixture and driver | 50 | [`connection_actor.rs`](../../../server/crates/waddle-server/src/server/routes/websocket/connection_actor.rs#L1) 41; [`prebind.rs`](../../../server/crates/waddle-server/src/server/routes/websocket/connection_actor/prebind.rs#L1) 4; [`connection_actor_core.rs`](../../../server/crates/waddle-server/src/server/routes/websocket/connection_actor_core.rs#L103) 2; [`connection_actor_driver_task.rs`](../../../server/crates/waddle-server/src/server/routes/websocket/connection_actor_driver_task.rs#L453) 3 |
| Endpoint publication and physical fixture | 24 | [`actor_local_endpoint.rs`](../../../server/crates/waddle-server/src/server/routes/websocket/actor_local_endpoint.rs#L1) 24 |
| Characterization seams | 22 | [`characterization_seams.rs`](../../../server/crates/waddle-server/src/server/routes/websocket/characterization_seams.rs#L229) 22 |
| Registry endpoint, entry, and publication | 46 | [`endpoint.rs`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/endpoint.rs#L203) 20; [`outbound.rs`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L577) 16; [`connections.rs`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L14) 10 |
| Other registry test helpers | 18 | [`resources.rs`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/resources.rs#L12) 6; [`user_registry.rs`](../../../server/crates/waddle-xmpp/src/registry/user_registry.rs#L1690) 5; [`actor_outbound.rs`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/actor_outbound.rs#L269) 4; [`binding.rs`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/binding.rs#L184) 2; [`connection_registry.rs`](../../../server/crates/waddle-xmpp/src/registry/connection_registry.rs#L96) 1 |
| State and registration fixtures | 14 | [`test_state.rs`](../../../server/crates/waddle-server/src/server/routes/websocket/test_state.rs#L509) 10; [`dual_registration.rs`](../../../server/crates/waddle-server/src/server/dual_registration.rs#L108) 3; [`state.rs`](../../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L2020) 1 |
| Test-gated hooks in loop and writer | 5 | [`batch_write.rs`](../../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L459) 2; [`connection.rs`](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L1932) 2; [`stream_management/registration.rs`](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L428) 1 |

Counts partition keys, not runtime actions: multiple target references can occur
on one source line. Type imports, type annotations and re-exports need separate
`type-only` / `excluded-type-only` treatment where applicable.

## Behavior and proposed classes

- **Fixture-owned WebSocket state:** The connection-actor module describes a
  [dark test-only slice](../../../server/crates/waddle-server/src/server/routes/websocket/connection_actor.rs#L1)
  that does not replace the live loop or publish a registry binding. It
  constructs `WsConnState`, optionally seeds handled state, and transfers it
  into `ConnectionActorCore` at
  [`connection_actor.rs:624`](../../../server/crates/waddle-server/src/server/routes/websocket/connection_actor.rs#L624),
  [`:734`](../../../server/crates/waddle-server/src/server/routes/websocket/connection_actor.rs#L734),
  and [`:923`](../../../server/crates/waddle-server/src/server/routes/websocket/connection_actor.rs#L923).
  Reservation captures the current binding and bounded sequence at
  [`:2323-2357`](../../../server/crates/waddle-server/src/server/routes/websocket/connection_actor.rs#L2323);
  completion verifies the exact connection, binding, operation and pending
  token before advancing the contiguous prefix at
  [`:2625-2672`](../../../server/crates/waddle-server/src/server/routes/websocket/connection_actor.rs#L2625).
  Fresh binding preparation and installation check for outstanding handled
  operations; an exact idempotent retry may return before those checks.
  Confirmation checks the current connection, attempt, binding and lifecycle
  but does not check the outstanding count. Retirement requires an exact
  binding and zero outstanding operations. See
  [`:2867`](../../../server/crates/waddle-server/src/server/routes/websocket/connection_actor.rs#L2867),
  [`:2967`](../../../server/crates/waddle-server/src/server/routes/websocket/connection_actor.rs#L2967),
  [`:3027`](../../../server/crates/waddle-server/src/server/routes/websocket/connection_actor.rs#L3027),
  and [`:3061`](../../../server/crates/waddle-server/src/server/routes/websocket/connection_actor.rs#L3061).
  These are real state/authority operations within the fixture, with proposed
  `authority;migration-glue` for its state-transition seam; ACK queueing at
  [`:1842`](../../../server/crates/waddle-server/src/server/routes/websocket/connection_actor.rs#L1842)
  additionally concerns `transport`. A token, queued ACK or fixture snapshot
  does not by itself establish a physical write.
- **Prebind and driver limits:** The
  [prebind module](../../../server/crates/waddle-server/src/server/routes/websocket/connection_actor/prebind.rs#L1)
  is reached through that test-support fixture. Its validated branch writes an
  authenticated session and phase only after matching the pending token,
  connection, running lifecycle and identities at
  [`prebind.rs:701-737`](../../../server/crates/waddle-server/src/server/routes/websocket/connection_actor/prebind.rs#L701);
  scripted and validated fixtures must not be treated as equivalent, and
  neither creates a binding. These state writes are authority-relevant fixture
  operations, not proof of a production authentication path. The separately
  gated `complete_fixture_handled` call at
  [`connection_actor_core.rs:103`](../../../server/crates/waddle-server/src/server/routes/websocket/connection_actor_core.rs#L103)
  advances the fixture's tracker. The physical parked driver is gated at
  [`connection_actor_driver_task.rs:453`](../../../server/crates/waddle-server/src/server/routes/websocket/connection_actor_driver_task.rs#L453).
- **Provisional publication/admission:** Under `test` or `test-utils`,
  [`connections.rs:15`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L15)
  conditionally publishes an exact provisional entry while holding the proper
  bind guard; replacement revokes/seals the old entry and updates the SM reverse
  index. [`:60`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L60)
  requires an exact confirmation and current entry before activation. The
  call chain is [`connections.rs:79`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L79)
  -> [`outbound.rs:578`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L578)
  -> [`endpoint.rs:277`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/endpoint.rs#L277)
  -> [`endpoint.rs:232`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/endpoint.rs#L232):
  the exact owner/binding can transition provisional or active to active,
  while a sealed binding refuses activation. Proposed classes for the actual
  admission checks and state transitions are `authority;transport;migration-glue`;
  the connection map and SM reverse-index operations include `derived-index`.
  These are not passive diagnostics. A
  [test-utils legacy sender conversion](../../../server/crates/waddle-xmpp/src/registry/connection_registry/endpoint.rs#L203)
  creates a queue without receiver identity, so its fixture must not be
  equated with a physical endpoint.
- **Characterization seams:** The [seam module](../../../server/crates/waddle-server/src/server/routes/websocket/mod.rs#L61)
  is `test-support` gated. When armed, it can reserve a pending flush,
  enqueue a frame owner-conditionally, inject a claimed-session replay gap and
  delay stale-owner cleanup at
  [`characterization_seams.rs:229`](../../../server/crates/waddle-server/src/server/routes/websocket/characterization_seams.rs#L229)
  and [`:365`](../../../server/crates/waddle-server/src/server/routes/websocket/characterization_seams.rs#L365).
  It can wait for a queue to become nonempty at
  [`:454`](../../../server/crates/waddle-server/src/server/routes/websocket/characterization_seams.rs#L454)
  and pause SM enable before start at [`:519`](../../../server/crates/waddle-server/src/server/routes/websocket/characterization_seams.rs#L519).
  These branches can affect `authority`, `derived-index`, and `transport` in
  the instrumented fixture; they are not all read-only observations. The
  live-queue wait is documented as unbounded; SM-enable hold has its own
  bounded timeout and refusal path. Arming, matching JID/stream, registry owner,
  release and feature conditions matter to each edge.
- **Fixture registry access and controls:**
  [`test_state.rs:321`](../../../server/crates/waddle-server/src/server/routes/websocket/test_state.rs#L321)
  constructs both a connection registry and user actor registry. Its
  [`register_test_connection` at `:509`](../../../server/crates/waddle-server/src/server/routes/websocket/test_state.rs#L509)
  registers, retrieves the same entry and calls the test/support-only
  [`mirror_register` at `dual_registration.rs:109`](../../../server/crates/waddle-server/src/server/dual_registration.rs#L109),
  which delegates at `:115` to the bounded production-shaped mirror ask.
  These are registry `derived-index` and actor `authority` operations, not a
  successful network bind. Test-only roster/blocklist/carbons setters in
  [`resources.rs:12`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/resources.rs#L12),
  [`:63`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/resources.rs#L63),
  and [`:212`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/resources.rs#L212)
  directly update the current entry without the separate owner-checked helper;
  shared-entry routing reads may observe those atomics. Treat these as actual
  fixture writes to derived/shared state, not as proof of current owner.
- [`binding.rs:188`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/binding.rs#L188)
  arms a `test-utils` one-shot probe, and [`:250`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/binding.rs#L250)
  signals it only once a bind-lock future polls pending. It is a contention
  observation, not lock ownership or successful registration. The
  [`PendingUnregisterCount` handler](../../../server/crates/waddle-xmpp/src/registry/user_registry.rs#L1690)
  only reads the pending count; it does not drain cleanup. The
  [`insert_for_test` wrapper](../../../server/crates/waddle-xmpp/src/registry/connection_registry/actor_outbound.rs#L269)
  calls the real custody insert, which checks connection identity and bounded
  capacity and can return the original envelope on refusal; its acceptance is
  not physical socket-write proof. The
  [force-detach race wrappers](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L849)
  call real detach paths with test callbacks; they are not mere counters.

## Distinct indirect edges and failure boundaries

1. **Publication and exact activation.** With `test-support`,
   [`actor_local_endpoint.rs:1217`](../../../server/crates/waddle-server/src/server/routes/websocket/actor_local_endpoint.rs#L1217)
   -> [`RegisterProvisionalConnectionIfExpected`](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L358),
   then [`:1243`](../../../server/crates/waddle-server/src/server/routes/websocket/actor_local_endpoint.rs#L1243)
   -> [`ConfirmProvisionalConnection`](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L438),
   and [`:1262`](../../../server/crates/waddle-server/src/server/routes/websocket/actor_local_endpoint.rs#L1262)
   -> [`activate_provisional_if_confirmed`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L60).
   Timeout or actor error maps to uncertain, not rejection; cleanup seals the
   candidate, attempts exact registry removal, and retries owner-matched actor
   removal as needed. The error can return before that retirement completes at
   [`actor_local_endpoint.rs:1057`](../../../server/crates/waddle-server/src/server/routes/websocket/actor_local_endpoint.rs#L1057)
   and [`:216`](../../../server/crates/waddle-server/src/server/routes/websocket/actor_local_endpoint.rs#L216).
   The physical owner is [not selected by `/ws`](../../../server/crates/waddle-server/src/server/routes/websocket/actor_local_endpoint.rs#L1).
2. **Actual stop-sensitive test edges.** A test sender at
   [`tests/supervised_io/connection_actor.rs:2459`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L2459)
   -> [`PrepareBinding` handler](../../../server/crates/waddle-server/src/server/routes/websocket/connection_actor.rs#L2849),
   followed at [`:2467`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L2467)
   -> [`InstallBinding`](../../../server/crates/waddle-server/src/server/routes/websocket/connection_actor.rs#L2949).
   Distinct test asks at [`:2216`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L2216)
   and [`:2232`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L2232)
   -> [`CompleteHandledInbound`](../../../server/crates/waddle-server/src/server/routes/websocket/connection_actor.rs#L2463)
   exercise later-first completion and contiguous-prefix advancement. These
   source assertions are not a newly run test or production custody proof.
3. **Peer submission currently refuses.** The test handle at
   [`connection_actor_driver_task.rs:1046`](../../../server/crates/waddle-server/src/server/routes/websocket/connection_actor_driver_task.rs#L1046)
   creates a peer envelope, then [`:1048`](../../../server/crates/waddle-server/src/server/routes/websocket/connection_actor_driver_task.rs#L1048)
   -> [`submit_peer_stanza` at `:621`](../../../server/crates/waddle-server/src/server/routes/websocket/connection_actor_driver_task.rs#L621),
   which returns `UnsupportedOperation` unconditionally. Do not infer a
   recipient pass, enqueue or delivery from the constructed envelope.
4. **Resume characterization controls real branches when armed.**
   [`registration.rs:836`](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L836)
   -> [`pause_published_resume_finalization`](../../../server/crates/waddle-server/src/server/routes/websocket/characterization_seams.rs#L229),
   whose [`:270`](../../../server/crates/waddle-server/src/server/routes/websocket/characterization_seams.rs#L270)
   -> [`try_reserve_pending_flush`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L179)
   and [`:286`](../../../server/crates/waddle-server/src/server/routes/websocket/characterization_seams.rs#L286)
   -> [`send_reserved_pending_flush`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L214)
   are conditional on the selected seam mode. Failed resume's
   [`stream_management/registration.rs:461`](../../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L461)
   -> [`delay_failed_resume_cleanup`](../../../server/crates/waddle-server/src/server/routes/websocket/characterization_seams.rs#L365),
   whose delayed [`:391`](../../../server/crates/waddle-server/src/server/routes/websocket/characterization_seams.rs#L391)
   -> [`unregister_if_owner`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L331)
   is separate from the ordinary rollback. `test-support`, armed matching
   identity and release conditions all apply; the source edge does not show a
   reservation or removal actually occurred.
5. **Test fixture shared entry:**
   [`test_state.rs:529`](../../../server/crates/waddle-server/src/server/routes/websocket/test_state.rs#L529)
   -> [`mirror_register`](../../../server/crates/waddle-server/src/server/dual_registration.rs#L109)
   -> [`mirror_register_outcome` at `:120`](../../../server/crates/waddle-server/src/server/dual_registration.rs#L120)
   at `:115` -> [`RegisterUserResource` handler](../../../server/crates/waddle-xmpp/src/registry/user_registry.rs#L1090)
   at `:134`. Its same-entry alias is explicit at `test_state.rs:518-532`;
   actor ask completion, later shared atomic reads and socket output are
   distinct evidence boundaries.

## Reproduction and limitations

Run the following from the repository root. It reads the retained captures and
prints counts only; it performs no row edit, Cargo invocation, or Git operation:

```python
import csv
from pathlib import Path
K = ("tag", "file", "line", "column", "compiled_target_kind",
     "compiled_target_name", "compiled_target_test_mode")
def load(path):
    with Path(path).open(newline="") as stream:
        return list(csv.DictReader(stream, delimiter="\t"))
def key(row):
    return tuple(row[name] for name in K)
def difference(profile):
    base = Path("docs/planning/2046-type-resolved/capture-4219bdd")
    s = load(base / f"test-support-{profile}/w2046-4219-support-{profile}-uses.tsv")
    p = load(base / f"uses-{profile}.tsv")
    return {key(row) for row in s} - {key(row) for row in p}
d = difference("default")
c = difference("clustering")
exclude = {"server/crates/waddle-xmpp/src/registry/user_actor.rs",
           "server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs"}
rest = {item for item in d if item[1] not in exclude}
print(len(d), len(c), d == c, len(rest),
      len({(item[1], item[2]) for item in rest}),
      len({item[1] for item in rest}))
```

Default support input SHA-256:
`c72d8307692492837cf12556c01d31eb0922edc814a8c35dad974912d36633b2`;
default production input SHA-256:
`67acefd0ba27a34b236fd43549ed2b45425e5aabf70ab703c4de615b04e6c8e7`.
Counts establish only this capture-key difference. Text searches and grouped
inspection do not enumerate every caller, feature combination, alias, operation,
or result; no Cargo, Git, test, runtime, or inventory-validator command ran.
No runtime reachability, physical write, completed teardown, or comprehensive
row-level classification follows from this note.
