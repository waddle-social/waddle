# Production authorization writer and acceptance inventory

Status: source inventory for #2047 F0; **production closure is not established**.
Reviewed source baseline: `db6e5338170d71391b69bc80e57626eb13072365`.
Post-baseline drift was manually reviewed through target main
`a020a79c588a297fe8ff90418f8bc74cbf15b676`, and the host-grant writer fence
S2S routing and the host-grant restore incarnation through
`0ac974978f2ba9a664b1964788f37e8fb2a6de18`, outbound S2S rooms through
`9766d9c8175f09d56fff6f4e6f8c65b93b0984eb`, and the host-grant revocation fence
through `46187e69f0638041d109eeb7497d37c6402c5c09` (see the last refresh below);
the original baseline and its limits remain unchanged.
The companion [source manifest](authority-inventory.json) binds the selected
references to exact lines and whole-file SHA-256 values. This inventory adds no
runtime behavior, authorization gate, schema, migration, CI job or activation.
It does not choose PostgreSQL or SpiceDB as a future canonical grant authority.

## Meaning and scope

The proposed invariant to investigate is: every mutation affecting a selected
admission dependency invalidates stale admission before its declared successful
ACK, and each covered sink retains the necessary authorization and ownership
fences through its acceptance boundary. The inventory below records existing
boundaries; it does not assert they implement that invariant.

- **Concrete** means an inspected source call/implementation and its immediate
  caller were found. It is static evidence, not observed runtime reachability
  in every deployment, an exhaustive call graph, or a successful test run.
- **Latent** means a defined capability with no current production caller found
  in the inspected sources. Preserve it as unresolved rather than deleting it
  from the inventory or calling it an active writer.
- **Lexical candidate** means only a raw token match. Imports, comments, tests,
  definitions and disabled `cfg` branches count. The checker never promotes a
  lexical candidate to either of the preceding categories.

The lexical scan covers every `.rs` file below `waddle-server/src` and
`waddle-xmpp/src`, relative to `server/crates/`, including inline and separate
tests. Selected reviewed boundary/test files also have whole-file fingerprints.
Other crates, generated code, migrations, extension modules, external SpiceDB
administrators and SQL clients are **not exhaustively inventoried**. Direct SQL,
trait dispatch, caller-provided dependencies, macro expansion and feature/config
selection require manual review. A source reference does not make an external
or legacy projection the canonical authority.

## Writer and policy-input map

**W1 — Graph backend and schema (concrete).** The permission actor owns a private
SpiceDB backend. [Lifecycle backend selection](../../crates/waddle-server/src/server/lifecycle.rs#L214) chooses it from server
configuration, except for the explicit missing-SpiceDB + enabled fixed-test-account
branch; [that branch](../../crates/waddle-server/src/server/fixed_account.rs#L19) uses the local backend. Lifecycle also asks
[EnsureSchema](../../crates/waddle-server/src/server/lifecycle.rs#L256). The backend [writes the schema](../../crates/waddle-server/src/permissions/spicedb.rs#L66),
[creates](../../crates/waddle-server/src/permissions/spicedb.rs#L450), [deletes](../../crates/waddle-server/src/permissions/spicedb.rs#L550), [replaces a relation family](../../crates/waddle-server/src/permissions/spicedb.rs#L465), and
[conditionally swaps a family for rollback](../../crates/waddle-server/src/permissions/spicedb.rs#L492). Each RPC has its own external
transaction/ACK. These methods return unit/status rather than a durable F0
operation identity or selected dependency witness. Family preconditions protect
that family; they are not a cross-store admission fence. [FullyConsistent
checks](../../crates/waddle-server/src/permissions/spicedb.rs#L426) return boolean-style CheckResponse results without exposing the graph
revision. The [local test backend](../../crates/waddle-server/src/permissions/actor.rs#L171) and [SQL TupleStore](../../crates/waddle-server/src/permissions/tuple/store.rs#L24) are
separate implementations; do not infer SpiceDB behavior from their tests. E1.

**W2 — Startup/account reconciliation and topology (concrete, configuration-dependent).**
[WebSocket-state startup](../../crates/waddle-server/src/server/http.rs#L1548) invokes account reconciliation behind its
lineage gate. [Provisioning](../../crates/waddle-server/src/server/bootstrap_membership.rs#L142) and [reconciliation](../../crates/waddle-server/src/server/bootstrap_membership.rs#L182) write
server owner/member tuples from account identity and configured owner localparts.
The existing-account wrapper logs a reconciliation failure; the batch is not one
all-or-nothing graph/SQL transaction. [Topology seeding](../../crates/waddle-server/src/server/topology.rs#L107) also writes
managed-channel bookmarks and parent tuples and seeds configured owners. This
includes restart/reconcile paths, not merely first account creation. A future
fence needs an explicit contract for their relation to concurrent revocation. E2.

**W3 — Spaces PubSub owner seeds (concrete, bounded by a seed policy).**
[The owner seed writer](../../crates/waddle-server/src/spaces_pubsub_seed.rs#L123) writes Owner under an
[`OwnerSeedPolicy`](../../crates/waddle-server/src/spaces_pubsub_seed.rs#L36) through
[`set_affiliation_unless_outcast`](../../crates/waddle-server/src/pubsub/implementation.rs#L445), one conditional
upsert, so an existing or concurrently recorded Outcast is kept. Startup and
node-creation helpers supply configured server owners and/or the creator; the
policy refuses an Owner seed for an account outside the served account domain
and the trusted homes it is given (none, for every current caller), and skips
the Space node of an enrolled host-grant scope. Per-entity failures are logged
and do not abort the batch; node-create seeding is best effort. E2.

**W4 — XMPP permission facade (retired).** The `AppState::set_room_affiliation`
trait method, its server implementation and the non-atomic delete-then-write
helper behind them are removed. No production caller existed in the two scan
roots; out-of-tree implementors of the trait no longer get the method, so the
capability cannot be reached again without a source change. The lexical token
`set_room_affiliation` now matches nothing in the scan roots.

**W5 — MUC and channel administration (concrete).** The active MUC admin handler
[persists managed-channel affiliation](../../crates/waddle-server/src/server/routes/websocket/handlers/iq/muc_admin.rs#L106) through ReplaceExclusiveRelation
before applying the room-actor batch. [Rollback](../../crates/waddle-server/src/server/routes/websocket/handlers/iq/muc_admin.rs#L207) uses expected
family state via SwapExclusiveRelation to avoid overwriting a later writer;
a failed/lost reply can still require reconciliation with actor/durable state.
Administrative channel commands are additional callers. Graph persistence,
actor mutation, durable room projection/effect publication and the IQ result are
distinct observations; a successful single graph write does not establish the
whole command committed. Room configuration, roles, explicit affiliations and
invitations also affect acceptance independently of a channel's inherited read
permission. E3/E6. For a room an enrolled host-grant scope manages, none of
this applies: the handler takes the sender's authority from the canonical
grants, [records one affiliation change in the host-grant store](../../crates/waddle-server/src/server/routes/websocket/handlers/iq/muc_admin_host_grant.rs#L115)
before the room actor applies it, writes no relationship and rolls nothing
back; owner configuration needs the canonical `manage` action and
[destroy is refused](../../crates/waddle-server/src/server/routes/websocket/handlers/iq/muc_owner_moderation.rs#L1321); admin channel and Space
commands [are refused](../../crates/waddle-server/src/admin/mod.rs#L59); and a
[mediated invitation grants nothing](../../crates/waddle-server/src/server/routes/websocket/handlers/message/muc_invite.rs#L272).

**W6 — Group-DM graph and legacy SQL mirror (concrete).**
[Member persistence](../../crates/waddle-server/src/admin/channels.rs#L3997) writes the permission actor and then
[permission_tuples through DbExecute](../../crates/waddle-server/src/admin/channels.rs#L4055). Failure attempts compensation
of the graph write. [Deletion](../../crates/waddle-server/src/admin/channels.rs#L4094) performs the inverse pair and may
restore graph membership if SQL deletion fails. These are separate commits with
possible partial/unknown outcomes, not a PostgreSQL transaction containing the
external graph mutation. Include the compensating paths as writers. Do not treat
this SQL mirror as the complete canonical grant journal. E3 is adjacent recovery
coverage, not an atomic graph/SQL proof. Both writers now
[refuse a group-DM identifier](../../crates/waddle-server/src/admin/channels.rs#L4043) that is the authorization
object of a room an enrolled scope manages, before either statement, since
group DMs and managed rooms share the channel identifier space.

**W7 — PubSub authorization inputs (concrete).** Generic owner-affiliations IQs
[write affiliation rows](../../crates/waddle-server/src/server/routes/websocket/handlers/iq/pubsub_admin.rs#L510); the storage [upsert/delete](../../crates/waddle-server/src/pubsub/implementation.rs#L409)
has no graph write. [Spaces set-role commands](../../crates/waddle-server/src/admin/spaces.rs#L1490) also change these
rows directly, without updating `space#member` in that command. Do not infer
equivalence between a PubSub role change and a graph membership revoke.
[Effective affiliation](../../crates/waddle-server/src/pubsub_authz.rs#L24) combines stored affiliation
and PEP owner derivation; subscribe, publish, read and admin use distinct access
and publish models. Subscription changes, owner-approved subscriptions and node
configuration affect eligibility too. [Node deletion](../../crates/waddle-server/src/pubsub/node.rs#L108) and recreation
change lifecycle and associated storage. Batch operations can have partial effects
before an error; a row's presence alone is not a permission witness. E4. On the
Spaces service, every request that names the Space node of an enrolled
host-grant scope, other than leaving a subscription,
[is refused](../../crates/waddle-server/src/server/routes/websocket/handlers/iq/pubsub_dispatch.rs#L596), so PubSub affiliations and
subscriptions there are never a second grant store.

**W8 — Parent/bookmark/link changes and compensation (concrete).**
[Spaces publish/move](../../crates/waddle-server/src/server/routes/websocket/handlers/iq/spaces_publish.rs#L17) separately changes the bookmark, SQL
channel-space link and graph parent once
[the publisher manages the channel or owns the deployment, or the channel already belongs to the Space](../../crates/waddle-server/src/server/routes/websocket/handlers/iq/spaces_publish.rs#L413),
then runs [stale cleanup](../../crates/waddle-server/src/server/routes/websocket/handlers/iq/spaces_bookmark_cleanup.rs#L11).
[Publish rollback](../../crates/waddle-server/src/server/routes/websocket/handlers/iq/spaces_publish.rs#L542), [retract](../../crates/waddle-server/src/server/routes/websocket/handlers/iq/spaces_retract.rs#L17) and
[Space deletion](../../crates/waddle-server/src/admin/spaces.rs#L1122) can remove or restore these inputs. Error results
and best-effort rollback do not prove no mutation happened. There is no single
SQL/graph revision covering the whole operation. A link is used as a projection
in one path and as a repair/discovery input in another; this inventory does not
resolve canonical parent authority. E5.

**W9 — Read/join-triggered repair (concrete).** An authorized Spaces items request
can execute [bookmark backfill](../../crates/waddle-server/src/server/routes/websocket/handlers/iq/spaces_items.rs#L97). It publishes a missing bookmark
and writes the channel's parent only when the channel has none or already has
that Space, retracting the bookmark when the parent write fails; it runs no
stale cleanup, and skips a room an enrolled scope manages or whose enrollment
cannot be told. [Join affiliation resolution](../../crates/waddle-server/src/server/routes/websocket/handlers/presence/muc/access.rs#L25) can invoke
[parent repair](../../crates/waddle-server/src/server/routes/websocket/handlers/presence/muc/access.rs#L425) after a denied channel check. Repair writes a graph
parent only for a channel that has none, naming the Space of the [channel-space link](../../crates/waddle-server/src/server/routes/websocket/handlers/presence/muc/access.rs#L559)
when that Space lists the channel's bookmark; a channel with a parent keeps it,
since only a placement moves it. Repair rechecks the bookmark after writing and
deletes its own write when the bookmark is gone. The
post-write recheck matters: a captured read alone does not prove the stale parent
survives. MAM explicitly disables graph repair. E5/E6. A room an enrolled
scope manages is [resolved from its canonical grants](../../crates/waddle-server/src/server/routes/websocket/handlers/presence/muc/access.rs#L102)
before any of this runs, so no repair is attempted for it.

**W10 — Room actor state, invitations and restoration (concrete, separate plane).**
[ChangeAffiliation](../../crates/waddle-xmpp/src/muc/room_actor/implementation.rs#L3111), [ApplyAffiliationChange](../../crates/waddle-xmpp/src/muc/room_actor/admin_handlers.rs#L1220),
[configuration changes](../../crates/waddle-xmpp/src/muc/room_actor/implementation.rs#L2762), [mediated-invite grants](../../crates/waddle-xmpp/src/muc/room_actor/mediated_invites/authorization.rs#L84),
creator ownership and resolver synchronization affect live/durable room policy.
Explicit grants have different provenance from resolver-derived grants. Actor
mailbox serialization, admission revisions and durable room ownership protect
particular actor transitions; they are not a common SpiceDB graph revision.
Restoration/recovery may replay an acknowledged durable transition. A future
writer inventory must include those paths, not just incoming IQ handlers. E3/E6.

## Readers and acceptance sinks

**S1 — Join.** [The join handler](../../crates/waddle-server/src/server/routes/websocket/handlers/presence/muc/implementation.rs#L865) freezes an actor admission revision before
external managed-channel resolution, checks connection ownership before its
final ask and supplies an affiliation grant to [JoinWithAffiliation](../../crates/waddle-xmpp/src/muc/room_actor/occupancy_handlers.rs#L52).
The actor checks ownership/restoration and its member admission revision, then
applies provenance-aware affiliation and room admission rules. CreatorOwner,
Resolver, Unaffiliated and HostOwned are different branches. Open/members-only,
outcast and [deployment-channel overrides](../../crates/waddle-server/src/server/managed_channel_policy.rs#L10) make this policy
richer than a generic `direct OR (parent AND member)` model. Source evidence here
does not establish that every graph writer advances the actor revision. E6. A
room an enrolled host-grant scope manages is entered only with the standing
its canonical grants honour (Outcast for a ban, nothing without a grant), as
a members-only room, and only through its channel row.

**S2 — Archive/discovery/extension reads.** The [MUC archive gate](../../crates/waddle-server/src/server/routes/websocket/handlers/presence/muc/access.rs#L222)
uses live room configuration when available, direct relations and managed-entry
rules; unmanaged rooms without a live actor fail closed. It is followed by a
separate [archive query](../../crates/waddle-server/src/server/routes/websocket/handlers/iq/archive_inbox_upload.rs#L184). There is no shared authorization snapshot
or transaction spanning those operations. Extensions have another
[room authorization entry](../../crates/waddle-server/src/server/extension_host_adapter/implementation.rs#L276) and [PubSub publication
authorizer](../../crates/waddle-server/src/server/extension_commands/pubsub.rs#L24), including manifest/deployment policy; these are not aliases for the
MAM sink. Rejection/error, returned archive rows and later network transmission
are different boundaries. E6. For a room an enrolled scope manages, the gate
and the room's disco#info decide from the canonical grants, and the archive
page is sent only if [a decision taken after the read](../../crates/waddle-server/src/server/routes/websocket/handlers/presence/muc/access.rs#L357)
still admits the requester.

**S3 — PubSub delivery.** The real database [subscriber query](../../crates/waddle-server/src/pubsub/implementation.rs#L376)
filters subscribed rows and outcasts. [Fanout captures that vector](../../crates/waddle-server/src/server/routes/websocket/handlers/pubsub_fanout/implementation.rs#L176), builds
a frozen event, expands resources and attempts the [live queue](../../crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L519) or
[detached SM buffer](../../crates/waddle-xmpp/src/stream_management/session_registry/resources.rs#L398). Private PEP has additional entitlement checks; do not
attribute those checks to generic Spaces fanout. Unsubscription/outcast/config
changes after capture are not uniformly rechecked at each sink. Queue acceptance,
SM custody, later socket write and peer acknowledgement are distinct events.
Best-effort errors/drops are not successful delivery. E4 verifies storage filtering,
not the capture-to-custody race or a no-bytes-after-revoke guarantee. Fan-out
from the Space node of an enrolled scope
[delivers nothing](../../crates/waddle-server/src/server/routes/websocket/handlers/pubsub_fanout/implementation.rs#L116), and a room-bound
extension node delivers only to subscribers who may still read the room.

**S4 — Durable room recipients.** [The membership source](../../crates/waddle-server/src/server/durable_membership.rs#L104) concurrently
reads channel and Space owner/admin/member subjects and excludes channel outcasts.
[Hydration](../../crates/waddle-xmpp/src/muc/room_actor/implementation.rs#L2319) populates an actor mirror; source errors preserve the
session-observed/existing behavior rather than establishing fresh denial. Later
removal handling prunes and refreshes that mirror. These multi-call reads are not
one graph snapshot; the mirror and explicit/live occupants are separate recipient
inputs. E7. A room an enrolled scope manages is answered
[from its canonical grants](../../crates/waddle-server/src/server/durable_membership.rs#L126) instead: its durable
recipients are the subjects whose grants allow reading it.

**S5 — Canonical acceptance and replay.** [Ingress claim checks](../../crates/waddle-server/src/ingress/commit_room.rs#L460) concern
exact actor ownership. [Admission coordinates](../../crates/waddle-server/src/ingress/commit_room.rs#L40) fence a new room snapshot
once: later revisions do **not** revoke children of an already accepted manifest.
Replay additionally validates occupancy/lifecycle and current claim ownership.
The [actor-owned replay guard](../../crates/waddle-server/src/ingress/room_manifest_replay.rs#L266) keeps a short outer claim transaction
through inner acceptance so caller timeout cannot release ownership prematurely;
the inner sink owns canonical rows. It is not a single F0 authorization-journal
transaction. Existing [lock order](../../crates/waddle-server/src/ingress_uow/mod.rs#L3) is epoch → exact claim → fenced
child rows. On the immediate, non-planning fallback, the existing
[pre-fanout ownership check](../../crates/waddle-server/src/server/routes/interpret/room_dispatch.rs#L133)
now retries within a fixed budget and
[bounces on backend uncertainty](../../crates/waddle-server/src/server/routes/interpret/room_dispatch.rs#L511)
without fanout or actor demotion. That is an exact room-ownership availability
backstop, not a new grant writer, grant store or cross-store authorization
revision. Authenticated planned groupchat continues to assert the room claim in
the ingress transaction. Cluster manifest activation remains dark in this
source. E8/E9.

## Existing evidence and its limits

These are **test source references inspected, not tests executed by this inventory
change**. Names or a green aggregate run alone do not attest prerequisites, the
backend, common snapshots, race ordering or production closure.

| ID | Existing test reference | What it can support / limit |
| --- | --- | --- |
| E1 | [write_and_delete_tuple_with_typed_payloads](../../crates/waddle-server/src/permissions/actor/tests.rs#L52) | Typed actor behavior; its helper uses an in-memory database/local backend, not SpiceDB. |
| E2 | `owner_reseed_keeps_bans_skips_enrolled_spaces_and_refuses_remote_accounts` in `crates/waddle-server/tests/xep0045_host_grant_fence.rs` (outside the scan roots, so not anchored) | The seed keeps an Outcast on the in-memory and SQL stores, skips an enrolled Space node and refuses remote privileged seeds; executed in the writer fence's focused set (host-grant design, evidence of the writer fence). |
| E3 | [rollback_keeps_affiliation_committed_by_a_later_owner](../../crates/waddle-server/src/server/routes/websocket/handlers/iq/muc_admin.rs#L3597) | Conditional rollback behavior in the test fixture; not cross-store atomicity or F0 writer closure. |
| E4 | [database_deliverable_subscribers_excludes_outcast](../../crates/waddle-server/src/pubsub/tests.rs#L997) | Storage filtering; no post-capture fanout guarantee. |
| E5 | [items authorization before backfill](../../crates/waddle-server/src/server/routes/websocket/tests/iq.rs#L3348), [repair only projected parent](../../crates/waddle-server/src/server/routes/websocket/tests/muc.rs#L4882) | Selected repair gates/selection; not all mutation interleavings or authority choice. |
| E6 | [members-only join requires explicit affiliation](../../crates/waddle-server/src/server/routes/websocket/tests/muc.rs#L1145), [MAM denies non-member](../../crates/waddle-server/src/server/routes/websocket/tests/iq.rs#L5533) | Current action-specific entry semantics; test state uses local fixtures, not an inherited graph fence. |
| E7 | [durable channel/Space users excluding outcasts](../../crates/waddle-server/src/server/durable_membership_tests.rs#L101) | Membership-source derivation; local permission actor, not a common snapshot. |
| E8 | [stale admission but preserved accepted children](../../crates/waddle-server/src/ingress/xep0045_room_manifest_tests.rs#L488) | The current accepted-manifest contract; not a new revocation policy. |
| E9 | [claim update blocked through commit](../../crates/waddle-server/src/ingress_uow/tests.rs#L546) | Claim lock lifetime when actually executed against PostgreSQL. [The fixture call](../../crates/waddle-server/src/ingress_uow/tests.rs#L1633) returns `None` without `WADDLE_TEST_POSTGRES_URL` in optional local mode, but panics when `WADDLE_TEST_REQUIRE_POSTGRES=1`; source presence or aggregate success outside required mode is not native execution evidence. |

Later isolated #2050/#2051/#2052/#2057/#2059/#2060/#2061 work was not present at
this baseline. Some of its characterization and fixture sources are now present
on target main, but are not silently promoted to production evidence. Their
leaf, accepted-child, lost-response, fixture, parent-move and finite multi-edge
results must be assessed at their own exact heads and do not prove this
inventory closed.

## Manual drift check and safe next proof

Run from the repository root:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 server/scripts/check_authority_inventory.py
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s server/scripts -p 'test_check_authority_inventory.py'
```

The checker validates whole-file hashes and exact source anchors for the selected
files, plus a digest/count of raw known-token matches in the two scan roots. It
reports **STALE/INVALID**, never semantic closure. It does not update evidence.
Whole-file hashes and matched line numbers deliberately cause benign churn;
review changed findings and anchors before refreshing them. No CI/publisher hook
or production dependency invokes this checker. The manifest is reviewed evidence,
not an authenticated runtime attestation or an automatically maintained call graph.

The original source review remains based at
`db6e5338170d71391b69bc80e57626eb13072365`. The selected fingerprints, anchors
and lexical snapshot were refreshed only after manually reviewing their delta
through target main `746765db0c3fdcd37d5e0505cde5b9e44b5d5a2b`:

- #2058 changed `server/http.rs` filesystem operations to async equivalents and
  released database guards before canonicalization. It did not change the
  reconciliation writer, its lineage gate, acknowledgement or error behavior.
  The same call moved from line 1036 to 1038, so its reviewed file fingerprint,
  anchor and documentation link were refreshed.
- The raw scan gained 21 records. Nineteen are confined to
  `recovery_room_child_contract_tests.rs`, `room_subject.rs` tests and the
  `space_fanout_postgres` test fixture/storage adapter. The other two are
  comments in `muc_durable/implementation.rs` and `room_dispatch.rs` that name
  the already-existing `assert_room_claim` ingress boundary. Six unchanged
  records moved because tests and modules gained lines. No known-token
  production invocation was added or removed.
- #2079 changed the existing immediate room pre-fanout ownership check from
  fail-open on backend error to bounded retry and fail-closed bounce without
  demotion, as recorded in S5. It did not add a host-grant writer, storage plane
  or canonical authorization decision.
- #2054's [foreign Carbons control guard](../../../docs/planning/remote-carbons-2047.md)
  is local routing policy: it rejects unsupported remote targets without
  authenticating or contacting a peer and does not alter grant authority. The
  merged [host-grant](host-grant-authority-2047.md),
  [S2S handshake](s2s-handshake-2047.md) and
  [verifier](s2s-verifier-2047.md) documents remain proposed designs with their
  implementation and activation gates intact. They add no runtime remote
  authority or federation readiness.

No new production grant writer, grant store or acceptance authority was found
in this target-main delta. This is a selected drift review, not proof that
unknown vocabulary, external writers or unreviewed paths do not exist.

The selected evidence was refreshed again after reviewing the subsequent delta
through target main `a020a79c588a297fe8ff90418f8bc74cbf15b676`:

- #2051, #2057, #2059, #2060 and #2061 add isolated characterization,
  recovery, fixture and finite-fence tests. The raw scan gained 15 records:
  every added match is in #2060's test-only parent-move harness or delegating
  storage adapter. They exercise existing production implementations but add
  no production caller or authority writer.
- Three selected PostgreSQL tests now obtain their mandatory database URL
  through `require_test_postgres_url`; this changes prerequisite enforcement,
  not the tested permission/PubSub operations. #2091 only changes telemetry
  assertions in selected MUC tests. The #2087 live-stanza queue seam is compiled
  only with `test-support`.
- #2082, #2085 and #2109 change liveness and bounded shutdown. Their edits
  move the lifecycle/startup anchors but leave backend
  selection, `EnsureSchema`, account reconciliation and their acknowledgement
  behavior unchanged.
- #2063 and #2064 reject unsupported foreign discovery/session controls, while
  #2070 forwards replies only to local full JIDs. These destination/routing
  guards neither authenticate a federation peer nor create a grant writer,
  grant store or canonical application-authorization decision.
- #2078 serializes owner-side remote-resource state updates per exact
  registration through a bounded FIFO; `Unavailable` and late completion remain
  possible. #2093 makes unregister-mirror ACK mean mailbox admission/custody,
  retains retry ownership and does not claim child completion.
- #2086 adds bounded node-identity retirement writes after an ordinary stop
  during re-registration. A timeout remains an unknown write outcome. This
  changes cluster-ownership lifecycle, not application-grant authority.
- #2088, #2089 and #2105 add characterization/qualification contracts. #2103
  raises the single SQLite resume-checkpoint transaction-acquisition bound from
  100 ms to 1 s while admission remains owned; it does not add a retry. None of
  these changes the selected grant writers or sinks.

No new production grant writer, grant store or acceptance authority was found
in this second target-main delta. This remains a selected static review: new
vocabulary, external writers and unreviewed files can still escape it.

The selected evidence was refreshed a third time for the host-grant writer
fence, through `33e65f08ce566e76cd07a5c3e5383240520edb24`:

- The canonical host-grant store (`permissions/host_grant`) is a grant store
  and its acceptance admission an acceptance authority; both are anchored
  (`hg-gate`, `hg-room-read`, `hg-admission`). The writer and sink
  dispositions of enrolled scopes are recorded in the host-grant design's
  writer fence section; the anchors prefixed `hg-` mark each fence.
- The `set_room_affiliation` facade (W4) is removed, so its three anchors
  and the in-crate seed-override test (E2's former reference) are retired.
- Eight tokens were added to the lexical scan for the fence's own
  vocabulary. The raw scan changed in every file the fence touches; the
  review of each change is the design's disposition table, not this checker.

The selected evidence was refreshed a fourth time for the enrolled set and
the remaining fences of the writer fence, through `e9feae31492f2461964555facf4fade642927ca9`:

- Whether an object is enrolled is answered from each replica's in-memory
  enrolled set (`hg-enrolled-set`), which the gate and the permission
  actor's legacy reads share; a managed room's grants are still read from
  PostgreSQL per decision.
- New fences are anchored: moderation, call setup, the pin list, room
  system messages, channel search, Spaces node discovery, the admin
  listing, extension bots, extension state and the extension host's room
  checks, the mirror repair, claim-move re-seating, the invitation
  rollbacks, the room actors' members-only binding and channel placement.
- Eight tokens were added to the lexical scan for that vocabulary. The raw
  scan changed in every file those fences touch; their review is again the
  design's disposition table.

The selected evidence was refreshed a fifth time, through
`6e4cceebb627b36dd48931bcaf1cb30c0f0835af`, after reviewing the delta of the
reviewed files since the fourth refresh:

- Space parents move only by an authorised placement. Join-triggered repair
  writes a parent only for a channel that has none; `join-repair-link` now
  names `linked_space_node`, which replaced `space_parent_repair_nodes` and
  no longer chooses among bookmarked Spaces. Read-triggered backfill runs no
  stale cleanup and skips rooms an enrolled scope manages. The stale cleanup
  that placement runs also removes other Spaces that are the channel's parent
  without listing its bookmark, and restores what it removed on failure. W9
  above states the current behaviour.
- The fences fail closed in more cases. The admin listing withholds every
  room the enrolled set names, even one the gate then answers as unmanaged.
  Group DM creation refuses a managed identifier before writing anything.
  Room administration arms the mirror repair before recording the first
  change, so a batch refused part-way or a commit that could not be confirmed
  is still repaired, and the repair re-reads the grants until their
  generation stops moving. A channel-wide mention in a managed room needs the
  sender's `moderate` grant and is not broadcast when the grants cannot be
  read. Room system messages without WebSocket state, and search results the
  store does not hold as managed, are withheld; search now reads grants by
  channel. The enrolled set records its age from the start of each load,
  drops a load that finished after a newer one, and warns when a reload
  shrinks it. The call gate's documentation now names which decisions pass it.
- The server startup and HTTP wiring gain the S2S configuration load,
  listener acquisition and router start. Those edits move the `startup*`,
  `startup-call` and `hg-recovery` anchors without changing backend
  selection, `EnsureSchema`, account reconciliation or projection recovery.
  S2S inbound routing refuses every stanza for the MUC and Spaces services
  and delivers only correlated IQ responses to local account resources, so
  it reaches no selected writer or sink.
- The raw scan keeps 1590 records. Six changed, all in files the fence
  touches: the parent-repair imports and delete, the backfill (which lost the
  retraction of its stale-cleanup rollback and gained its enrollment check),
  the room-admin call's error binding and the enrolled-set re-export. The
  rest moved lines. No token was added, and no production grant writer,
  grant store or acceptance authority was found.

The selected evidence was refreshed a sixth time, through
`f41951085420e0581c92e11a96d6aa3a1c25f2f4`, the merge of the host-grant
restore incarnation and the fixes after it, after reviewing the delta of the reviewed files since
the fifth refresh:

- Room-manifest acceptance (`claim`, `hg-admission`) compares the authority
  binding it reads under the scope's guard with its attachment's, refuses a
  room of an enrollment only SpiceDB holds, and asks the replica's
  attachment gate to verify again when the binding changed. Nothing it
  admits was widened.
- The permission actor gains witness messages (read, list, replace, clear)
  and the restored-scope takeover; each writes only the host-grant
  projection definition, conditioned on the exact stamps it read, and the
  takeover is reachable only with a store-produced `RestoredPlan`. The
  legacy tuple writers and their writer and reader fences are unchanged;
  their anchors moved.
- The writer fence's gate (`hg-gate`) reads its attachment from the
  attachment gate at each decision, so a refused or re-verified authority
  closes every surface. Projection recovery (`hg-recovery`) waits for the
  first attachment and runs under the replica's issuance policy, the one
  room administration (`hg-room-admin`) builds, now from one place, since a
  reconstruction it reaches records lowered grants. Startup moves
  `startup-call` without changing backend selection, `EnsureSchema` or
  account reconciliation.
- The raw scan holds 1595 records, five more: three documentation
  references to `WriteHostGrantProjection` beside the new actor messages,
  and one `write_relationships` and one `delete_relationships` in the
  witness writers, both confined to the projection definition. The rest
  changed only their line or the import list they sit in. No production
  grant writer, grant store or acceptance authority was added.

The selected evidence was refreshed a seventh time, through
`0ac974978f2ba9a664b1964788f37e8fb2a6de18`, after reviewing the delta of
the reviewed files since the sixth refresh:

- The writer fence's gate (`hg-gate`) reads the attachment only while the
  attachment gate is attached (`HostGrantAttachmentGate::verified_attachment`).
  While that gate verifies a changed authority again, and after that
  verification failed, it still holds the earlier attachment for
  acceptance's binding comparison, and every surface outside acceptance
  answers unavailable for a managed room. A second, different attachment
  gate offered to it makes it refuse every managed room; startup
  (`startup-call` moved) checks the binding and, when it fails, logs an
  error and verifies nothing. Projection recovery (`hg-recovery`) moved
  without changing.
- Room administration's background reconciliation runs no pass while the
  attachment gate is not attached; the mirror repair (`hg-mirror-repair`)
  moved without changing and decides through the gate above.
- The surfaces still compare no binding per decision and do not ask the
  attachment gate about rooms of an enrollment only SpiceDB holds; both
  are recorded in the host-grant design as preconditions for the first
  enrollment.
- The raw scan holds 1595 records, unchanged in count and content; only
  lines moved, in the files above and in the fenced WebSocket test state,
  which now binds its gate to the ingress authority's attachment gate. No
  production grant writer, grant store or acceptance authority was added.

The selected evidence was refreshed an eighth time, through
`9766d9c8175f09d56fff6f4e6f8c65b93b0984eb`, after reviewing the delta of the reviewed files since the
seventh refresh:

- S2S startup (`server/lifecycle.rs`) gives the S2S router a second store
  in the global database, the remote room occupancies local resources hold
  in rooms on other domains. The store records S2S routing state only; it
  writes no grant, affiliation or acceptance row, and the `startup` and
  `startup-schema` anchors did not move.
- The raw scan holds 1595 records, unchanged in count and content; 13 moved
  lines in `routes/websocket/cleanup.rs`, `routes/interpret/carbons.rs`,
  `runtime_owner.rs` and `test_support.rs`, which gained remote room
  cleanup, carbon rules for remote rooms, table grants for the two new S2S
  tables and a test re-export. No production grant writer, grant store or
  acceptance authority was added.

The selected evidence was refreshed a ninth time, through
`80c940af28c8bd4f6cce2b44c3094469bbb8ff3c`, the host-grant revocation fence
and the non-reuse of retired account JIDs, after reviewing the delta of the
reviewed files since the seventh refresh on the revocation fence's own line of
history, which did not yet hold the outbound S2S rooms of the eighth. Four
files joined the reviewed set: `permissions/host_grant/fence.rs`,
`server/host_grant_revocation.rs`, `auth/retirement.rs` and `auth/native.rs`.

- The writer fence's gate (`hg-gate`) decides a managed room only while the
  replica holds a lease of the revocation fence, and every decision carries
  the fence's ticket (`hg-fence-ticket`): `allows` and `standing` answer
  nothing once the replica has installed a newer generation of the scope. A
  revoke is acknowledged (`hg-fence-ack`) once every replica that holds a
  lease recorded its generation. The fence's two relations hold leases and
  acknowledgements, never a grant; nothing they hold admits anything.
- Room-manifest acceptance (`hg-admission`) and every mutation of the grant
  store take an advisory lock in front of the scope's guard. What either
  admits or records is unchanged.
- Room administration (`hg-room-admin`) waits for the acknowledgement after
  the store recorded a change, and a role change is applied by the room actor
  only while the sender's decision is current. The join (`hg-join`) hands
  its decision's ticket to the room actor. `actor-recovery` moved: the actor
  gains `RemoveUnadmittedOccupant` and the admission-checked forms of
  `JoinWithAffiliation` and `ApplyAdminItems`, which seat or apply nothing
  the plain messages would not.
- A new writer of room-actor state, the revocation sweep
  (`hg-revocation-sweep`): from a read of a managed room's canonical grants
  it removes occupants the grants do not admit and lowers mirrored
  affiliations to the recorded ones, through `ApplyAffiliationChange`,
  `ChangeAffiliation` and `RemoveUnadmittedOccupant`. It never seats an
  occupant, never raises an entry above the recorded affiliation and writes
  no authorization relationship. The path that restores seats after a room's
  claim moved (`hg-claim-move`) asks for it once the seats are restored.
- Space fan-out (`fanout`, `hg-fanout`) reads each captured subscription
  again before the first hand-over to its subscriber (`hg-fanout-recheck`,
  `PubSubStorage::subscription_deliverable`); it delivers to no one the
  capture did not hold. The #2056 control and its storage adapter moved from
  the library tests to an integration test, outside the scanned roots.
- A new writer of account state, `retire_account` (`account-retire`): it
  records a JID in `retired_account_jids` and deletes that account's rows.
  It writes no authorization relationship and no grant; the account's
  deployment relations and room grants stay as they are. Native registration
  (`account-retired-native`), identity-provider provisioning and session
  creation create no row for a JID the ledger holds.
- `startup-call` and `hg-recovery` moved: startup starts the fence and
  installs the sweep, without changing backend selection, `EnsureSchema` or
  account reconciliation.
- The raw scan holds 1597 records, two more. The control and its adapter
  left the scan: `set_affiliation` (5), `publish_item` (3), `retract_item`
  (2), `delete_node` (2) and `list_deliverable_subscribers` (4, against 2
  added where the storage trait documents and implements the new read).
  Added: three `JoinWithAffiliation` and three `ApplyAdminItems` in the
  admission-checked actor messages, three `ApplyAffiliationChange` and two
  `ChangeAffiliation` in the sweep and the actor's removal, three
  `EnrolledScopes` in the fence, one `room_enrollment` in the gate and one
  `pubsub_affiliations` in the single-subscription read. No legacy tuple
  writer, grant store or acceptance authority was added, and none was
  widened.

The selected evidence was refreshed a tenth time, through
`939db7350afb262b044d17a1abef370fef609776`, the merge of the attached-only gate into the
revocation fence and the fixes after it, after reviewing the delta of the
reviewed files since the ninth refresh:

- The revocation fence (`hg-fence-ack`, `hg-fence-ticket` moved) holds a
  lease only while the attachment gate the surfaces read is attached and no
  other gate was offered (`hg-gate`), measures it on the boot-time clock,
  and lapses a lease whose renewal is answered after it ran out. Its two
  relations still hold leases and acknowledgements only; registration and
  renewal also read PostgreSQL's clock. Nothing they hold admits anything.
- Owner configuration (`hg-owner-config`) reads the grants again under the
  room's configuration lock and refuses an initiator they no longer let
  configure the room; it writes nothing it did not write before. The
  muc#admin role list, an extension's archive query (`extension` moved),
  the reserved-nick node and the room's own message
  (`hg-system-message`) decide again after their read; each answers or
  delivers less, never more. Room administration and owner configuration
  wait at most 15 s for the configuration lock (`room-admin`,
  `admin-rollback`, `e-rollback`, `group-dm*`, `hg-group-dm` moved).
- Claim-move re-seating (`hg-claim-move`) asks for the revocation sweep
  when its seating ask failed; the sweep writes as before.
- Space fan-out (`hg-fanout-recheck`) lists the node's deliverable
  subscribers again once per publication instead of reading each captured
  subscription; the storage trait's single-subscription read is removed
  (`pubsub-write`, `hg-unless-outcast` moved). The fan-out delivers to no
  one the capture did not hold.
- `account-retire` moved: only its documentation changed.
- The raw scan holds 1596 records, one fewer: the single-subscription read
  took one `pubsub_affiliations` and two `list_deliverable_subscribers`
  (the trait default and its documentation) with it, the batched read adds
  one `list_deliverable_subscribers`, and the second audience decision of a
  room's own message one `system_message_audience`. No legacy tuple writer,
  grant store or acceptance authority was added, and none was widened.

The selected evidence was refreshed an eleventh time, through
`46187e69f0638041d109eeb7497d37c6402c5c09`, the merge of the revocation fence
with the outbound S2S rooms, after reviewing the merged tree against both of its
parents:

- No reviewed file and no anchor differs from the revocation fence's tenth
  refresh; the outbound S2S rooms changed none of the reviewed files.
- The raw scan holds 1596 records, identical in content to the tenth refresh;
  the 13 lines the eighth refresh recorded as moved in
  `routes/websocket/cleanup.rs`, `routes/interpret/carbons.rs`,
  `runtime_owner.rs` and `test_support.rs` moved again, as they had there. No
  production grant writer, grant store or acceptance authority was added.

The checker establishes only that these exact files, anchors and raw matches
are the ones reviewed.

Unknown spellings/aliases, new code outside the roots, external writers and changes
to unmatched unreviewed files can escape detection. An explicit passing negative
control documents that limitation. A literal scan cannot decide whether an ask
runs, a test is enabled, a storage trait resolves to PostgreSQL, or a permission
branch is the one that justified admission. Comments/tests count as candidates.

The smallest follow-up is one real call-site characterization tied to this map:
freeze an inspected read, acknowledge a competing production mutation, then
observe the exact acceptance/custody boundary and independent-scope progress.
Keep the current outcome as a labeled characterization/unsafe control. Parent
repair must preserve its post-write revalidation rather than pausing after an
arbitrary lookup and declaring stale authority accepted. Actor work must remain
owned beyond caller cancellation; no absence-of-output or sleeps-as-order proof.

Positive production fencing is blocked on a declared canonical mutation/ACK
contract; complete participating writers including seeds/rollback/repair; exact
action/principal/lifecycle/schema dependency rules and alternative grants; and a
reviewed integration with the real claim and acceptance transaction/lifetime.
The policy for already accepted children is separate from new admission. This
inventory neither changes that contract nor promises byte draining after revoke.
