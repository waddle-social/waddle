# Support-only UserActor sites: partial review

This note groups the requested support-capture difference for `user_actor.rs`
and `user_actor/implementation.rs`. It is **partial**: the counts are exact
for the specified input keys and line partitions, but this is not a
site-by-site semantic review of all 173 rows, a complete caller inventory, or
an inventory-validator result. Classifications below describe source behavior
at the cited anchors and are proposed for subsequent row-level review. The
integrator identified this checkout's server source as `4219bdd`; this review
did not independently verify Git provenance. Recheck line anchors against the
actual source target before incorporating anything.

## Mechanical scope and build meaning

Subtract distinct production keys from distinct support keys on
`(tag, file, line, column, compiled_target_kind, compiled_target_name,
compiled_target_test_mode)`. Default support has 3,251 distinct keys versus
2,899 production keys and **352** support-only keys. Repeating the same
subtraction for clustering gives 352 keys and the same key set; this comparison
deliberately excludes profile and compiled features, so it does not prove
identical compiled contexts or behavior.

Of the 352 default keys, **173** are in the requested files: **41** in
`user_actor.rs` and **132** in its `implementation.rs`, spanning 132 distinct
file/line pairs and 54 distinct tags. All 173 have capture target
`lib / waddle_xmpp / test=false`, target features `["test-utils"]`, and
`source_class_hint=prod`. The hint and `test=false` do **not** make the added
paths ordinary serving paths: the library is compiled with a test feature.
The server manifest [enables `test-utils` for dev-dependencies and through
`test-support`](../../../server/crates/waddle-server/Cargo.toml#L159); the
actual feature graph must be considered for each build.

| Source region | Extra keys | Distinct lines | Observed grouping |
| --- | ---: | ---: | --- |
| `user_actor.rs:1-153` | 30 | 25 | observation types, journal drain and reply |
| `user_actor.rs:154-191` | 8 | 6 | mailbox gate and no-op controls |
| `user_actor.rs:192-228` | 3 | 3 | gated message re-exports |
| `implementation.rs:1-99` | 10 | 10 | journal state, initialization and bounded snapshot helper |
| `implementation.rs:100-176` | 14 | 10 | removal observation hooks |
| `implementation.rs:177-325` | 53 | 37 | registration and owner-gated registration observation hooks |
| `implementation.rs:326-457` | 53 | 39 | publication hold, provisional mirror and confirmation, plus observations |
| `implementation.rs:458-869` | 0 | 0 | no extra keys under this subtraction |
| `implementation.rs:870-923` | 2 | 2 | conflict-close observation hook |

The grouping follows source line regions; several keys can refer to different
symbols on one line, and grouped keys need not share a single operation or
classification. Type imports and signatures need individual `type-only` /
`excluded-type-only` treatment where appropriate rather than being counted as
runtime reads or calls.

## Behavior and proposed classes

- **Observation journal:** [`user_actor.rs:31`](../../../server/crates/waddle-xmpp/src/registry/user_actor.rs#L31)
  gates the test controls under `any(test, feature = "test-utils")`.
  [`implementation.rs:35`](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L35)
  adds copied diagnostic fields, a 64-record journal and overflow counter;
  [`:73`](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L73)
  appends only while capacity remains and otherwise saturating-increments the
  overflow count. [`user_actor.rs:121`](../../../server/crates/waddle-xmpp/src/registry/user_actor.rs#L121)
  drains/reset these into a reply and explicitly marks model comparison
  inconclusive. For the actual journal mutations and reads, `derived-index`
  is a reasonable proposed class for copied diagnostic state, **not** a claim
  that the journal is a serving routing index, durable authority, publication,
  or custody proof. Operations vary between `construct`, `read`, `write`, and
  `read-write`; imports/type spans are separate.
- The observation hooks around **ordinary** release-compiled registration,
  owner-gated registration, removal and conflict close are gated extra code:
  [`implementation.rs:117`](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L117),
  [`:159`](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L159),
  [`:204`](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L204),
  [`:253`](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L253),
  [`:277`](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L277),
  [`:300`](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L300),
  and [`:871`](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L871).
  Snapshot reads and journal append are diagnostic `derived-index` operations;
  they must not be conflated with the adjacent ungated actor-map writes, owner
  checks, binding seals, or transport effects. For example, ordinary
  `RegisterConnection` seals and inserts at
  [`:212-215`](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L212),
  while its copied observation is separately guarded at `:216-228`.
- Bounded binding evidence is diagnostic, not an exact identity witness:
  [`implementation.rs:89`](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L89)
  calls `snapshot_if_capacity`, skipping creation once full. The
  [`BoundedBindingObservation` definition](../../../server/crates/waddle-xmpp/src/ownership/connection_identity.rs#L106)
  describes truncated node prefixes, with 128-byte field limits. The fixture
  claim identity is [synthetic](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L42),
  not an acquired distributed claim. Overflow means omitted later observations,
  not absent transitions. Existing test source checks saturation and UTF-8
  truncation at [`user_actor/tests.rs:322`](../../../server/crates/waddle-xmpp/src/registry/user_actor/tests.rs#L322)
  and [`:374`](../../../server/crates/waddle-xmpp/src/registry/user_actor/tests.rs#L374);
  no test was executed for this review.
- **Mailbox controls:** [`GateMailbox` at `user_actor.rs:157`](../../../server/crates/waddle-xmpp/src/registry/user_actor.rs#L157)
  signals entry and waits for release; [`MailboxNoop` at `:175`](../../../server/crates/waddle-xmpp/src/registry/user_actor.rs#L175)
  is a mailbox ordering probe. Their channel/actor calls are test-control
  `transport`, not outbound XMPP frame delivery. The separate
  [`HoldUserActorForPublication` at `implementation.rs:337`](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L337)
  requires `feature = "test-utils"` specifically, and is publication-test
  `migration-glue`; it parks the mailbox rather than reading production claim
  authority.
- **Provisional mirror is a real mutation even though it is gated:**
  [`RegisterProvisionalConnectionIfExpected` at `implementation.rs:329`](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L329)
  checks bare/full JID agreement, provisional status, presence of a binding
  reference, and the exact expected previous binding at `:372-380`. It records
  rejection on mismatch; otherwise it seals a previous actor binding and inserts
  the candidate at [`:393-397`](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L393).
  The checked mutation is `authority;migration-glue`; the surrounding copied
  observation fields are diagnostic `derived-index`. This handler does **not**
  activate the endpoint.
- **Confirmation is separate from activation:**
  [`ConfirmProvisionalConnection` at `implementation.rs:431`](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L431)
  validates the JID and matches the current entry before returning a cloned
  witness. Its actual check/read and witness construction are proposed
  `authority;migration-glue`; it does not itself activate or write a frame.
  Both provisional handlers require `any(test, feature = "test-utils")`.

## Risk-relevant, distinct source edges

1. **Publication versus activation.** The `test-support`-gated endpoint flow
   [`actor_local_endpoint.rs:1203`](../../../server/crates/waddle-server/src/server/routes/websocket/actor_local_endpoint.rs#L1203)
   publishes a provisional registry candidate, then
   [`:1217`](../../../server/crates/waddle-server/src/server/routes/websocket/actor_local_endpoint.rs#L1217)
   asks the [provisional actor handler](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L358).
   Only a successful answer proceeds to the confirmation ask at
   [`:1243`](../../../server/crates/waddle-server/src/server/routes/websocket/actor_local_endpoint.rs#L1243)
   -> [confirmation handler](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L438),
   followed by [`:1262`](../../../server/crates/waddle-server/src/server/routes/websocket/actor_local_endpoint.rs#L1262)
   -> [`activate_provisional_if_confirmed`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L60).
   The latter calls [`ConfirmedProvisionalMirror::matches`](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L425)
   at [`connections.rs:67`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L67)
   and rechecks the registry entry before activation. The endpoint is
   [release compiled but not selected by `/ws`](../../../server/crates/waddle-server/src/server/routes/websocket/actor_local_endpoint.rs#L1);
   the shown publication flow itself is `test-support` gated.
2. **Ambiguous timeout and compensation.** The mailbox hold at
   [`tests/supervised_io/connection_actor.rs:1987`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L1987)
   -> [`HoldUserActorForPublication` handler](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L344)
   permits a mirror request to remain pending while timeout, caller
   cancellation or lease revocation is exercised. These are separate loss
   branches at [`connection_actor.rs:2028`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L2028).
   In source, failed or undeliverable publication results seal the candidate
   and call `ticket.retire` at
   [`actor_local_endpoint.rs:1057`](../../../server/crates/waddle-server/src/server/routes/websocket/actor_local_endpoint.rs#L1057).
   Retirement attempts to remove the exact registry candidate and asks owner-qualified
   child removal at [`:216-238`](../../../server/crates/waddle-server/src/server/routes/websocket/actor_local_endpoint.rs#L216),
   retrying on other outcomes. A timeout or enqueued ask is not evidence that
   compensation has completed; an observed test assertion is not a fresh test run.
3. **Late child commit after timeout.** The unit-test sender
   [`user_actor/tests.rs:568`](../../../server/crates/waddle-xmpp/src/registry/user_actor/tests.rs#L568)
   -> [`GateMailbox` handler](../../../server/crates/waddle-xmpp/src/registry/user_actor.rs#L162),
   then [`tests.rs:580`](../../../server/crates/waddle-xmpp/src/registry/user_actor/tests.rs#L580)
   -> [`RegisterConnection` handler](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L194),
   demonstrates a distinct source scenario: the ask can time out while
   admitted work later commits after release. The later journal ask at
   [`tests.rs:592`](../../../server/crates/waddle-xmpp/src/registry/user_actor/tests.rs#L592)
   -> [`TakeResourceMutationObservations`](../../../server/crates/waddle-xmpp/src/registry/user_actor.rs#L121)
   is only a bounded child decision observation. A separate
   `test-support` integration sender at
   [`characterization_bind.rs:1369`](../../../server/crates/waddle-server/tests/characterization_bind.rs#L1369)
   parks the child while bind rollback queues owner-aware cleanup; its comments
   expressly delimit the harness's supervisor coverage at `:1339-1340`.

## Reproduction and evidence limits

Run the following from the repository root. It reads the retained captures and
prints counts only; it does not write rows or require Cargo or Git:

```python
import csv
from pathlib import Path

keys = ("tag", "file", "line", "column", "compiled_target_kind",
        "compiled_target_name", "compiled_target_test_mode")
def load(path):
    with Path(path).open(newline="") as f:
        return list(csv.DictReader(f, delimiter="\t"))
def key(row):
    return tuple(row[name] for name in keys)
def extra(profile):
    base = Path("docs/planning/2046-type-resolved/capture-4219bdd")
    support = load(base / f"test-support-{profile}/w2046-4219-support-{profile}-uses.tsv")
    prod = load(base / f"uses-{profile}.tsv")
    return {key(row) for row in support} - {key(row) for row in prod}
default = extra("default")
cluster = extra("clustering")
targets = {"server/crates/waddle-xmpp/src/registry/user_actor.rs",
           "server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs"}
subset = {item for item in default if item[1] in targets}
print(len(default), len(cluster), default == cluster, len(subset))
```

Input-file SHA-256 at inspection time: support default
`c72d8307692492837cf12556c01d31eb0922edc814a8c35dad974912d36633b2`,
production default `67acefd0ba27a34b236fd43549ed2b45425e5aabf70ab703c4de615b04e6c8e7`,
support clustering `f912259f864c89812a0a8e9e38220b90b974bd48374ff74cc473fa09ced6a21c`,
and production clustering
`f6c7795f3af9ec434b8ff75e84d5ebf2df82b484023d28f49a097e0a0884428c`.

This scope does not cover the other 179 support-only keys, all test senders,
all indirect aliases, live actor cutover, or all possible feature combinations.
No TSV was changed, and no Cargo, Git, test, runtime, or inventory-validator
command was run. Counts and source inspection do not prove runtime reachability,
physical delivery, authority, successful cleanup, or inventory completeness.
