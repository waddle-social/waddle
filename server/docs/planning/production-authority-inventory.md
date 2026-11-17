# Production authorization writer and acceptance inventory

Status: source inventory for #2047 F0; **production closure is not established**.
Reviewed source baseline: `db6e5338170d71391b69bc80e57626eb13072365`.
Post-baseline drift was manually reviewed through target main
`a020a79c588a297fe8ff90418f8bc74cbf15b676`, and the host-grant writer fence
S2S routing and the host-grant restore incarnation through
`0ac974978f2ba9a664b1964788f37e8fb2a6de18`, outbound S2S rooms through
`9766d9c8175f09d56fff6f4e6f8c65b93b0984eb`, the host-grant revocation fence
through `9b2e2d57ebd13329ddc045b2d1343f03b9619089`, and the effect boundary for
remote principals through `c9111c714cfb821e571cc7f3b63d4a9ef37d11ba`, merged
with them in `763988ff0b1c466d659033ca398623cf9999fc11` and reviewed through
`8f6088ba601528090ec1bc82294b1a12422f5519`, and inbound S2S rooms through
`31fd3f41591d0ff3072dc667e35207c522ae0dc6` and, with remote room
administration, the federated setting and requests carried between
replicas, through `f6eea18f3a360e216b33d689395af8a7d487b8eb`, and with the
placement of requests, the ends of a seat and the decisions that name a
seated remote occupant, through `ab055f27480da80b43e15087f17e699199cce190`, and
with the bounds on that work, the takeover of a restored room's seats and
the issuance check across replicas, through `99f30be6766e510672dbaa8c421a873ab758b9cd`, and Spaces over S2S
through `0ee8556e2eea373a6d7820b525b96f3e2188e540` (see the last refresh below); the original
baseline and its limits remain unchanged.
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
SpiceDB backend. [Lifecycle backend selection](../../crates/waddle-server/src/server/lifecycle.rs#L217) chooses it from server
configuration, except for the explicit missing-SpiceDB + enabled fixed-test-account
branch; [that branch](../../crates/waddle-server/src/server/fixed_account.rs#L19) uses the local backend. Lifecycle also asks
[EnsureSchema](../../crates/waddle-server/src/server/lifecycle.rs#L259). The backend [writes the schema](../../crates/waddle-server/src/permissions/spicedb.rs#L66),
[creates](../../crates/waddle-server/src/permissions/spicedb.rs#L450), [deletes](../../crates/waddle-server/src/permissions/spicedb.rs#L550), [replaces a relation family](../../crates/waddle-server/src/permissions/spicedb.rs#L465), and
[conditionally swaps a family for rollback](../../crates/waddle-server/src/permissions/spicedb.rs#L492). Each RPC has its own external
transaction/ACK. These methods return unit/status rather than a durable F0
operation identity or selected dependency witness. Family preconditions protect
that family; they are not a cross-store admission fence. [FullyConsistent
checks](../../crates/waddle-server/src/permissions/spicedb.rs#L426) return boolean-style CheckResponse results without exposing the graph
revision. The [local test backend](../../crates/waddle-server/src/permissions/actor.rs#L171) and [SQL TupleStore](../../crates/waddle-server/src/permissions/tuple/store.rs#L24) are
separate implementations; do not infer SpiceDB behavior from their tests. E1.

**W2 — Startup/account reconciliation and topology (concrete, configuration-dependent).**
[WebSocket-state startup](../../crates/waddle-server/src/server/http.rs#L1561) invokes account reconciliation behind its
lineage gate. [Provisioning](../../crates/waddle-server/src/server/bootstrap_membership.rs#L143) and [reconciliation](../../crates/waddle-server/src/server/bootstrap_membership.rs#L183) write
server owner/member tuples from account identity and configured owner localparts.
The existing-account wrapper logs a reconciliation failure; the batch is not one
all-or-nothing graph/SQL transaction. [Topology seeding](../../crates/waddle-server/src/server/topology.rs#L109) also writes
managed-channel bookmarks and parent tuples and seeds configured owners. This
includes restart/reconcile paths, not merely first account creation. A future
fence needs an explicit contract for their relation to concurrent revocation. E2.

**W3 — Spaces PubSub owner seeds (concrete, bounded by a seed policy).**
[The owner seed writer](../../crates/waddle-server/src/spaces_pubsub_seed.rs#L143) writes Owner under an
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
before applying the room-actor batch. [Rollback](../../crates/waddle-server/src/server/routes/websocket/handlers/iq/muc_admin.rs#L248) uses expected
family state via SwapExclusiveRelation to avoid overwriting a later writer;
a failed/lost reply can still require reconciliation with actor/durable state.
Administrative channel commands are additional callers. Graph persistence,
actor mutation, durable room projection/effect publication and the IQ result are
distinct observations; a successful single graph write does not establish the
whole command committed. Room configuration, roles, explicit affiliations and
invitations also affect acceptance independently of a channel's inherited read
permission. E3/E6. For a room an enrolled host-grant scope manages, none of
this applies: the handler takes the sender's authority from the canonical
grants, [records one affiliation change in the host-grant store](../../crates/waddle-server/src/server/routes/websocket/handlers/iq/muc_admin_host_grant.rs#L126)
before the room actor applies it, writes no relationship and rolls nothing
back; owner configuration needs the canonical `manage` action and
[destroy is refused](../../crates/waddle-server/src/server/routes/websocket/handlers/iq/muc_owner_moderation.rs#L1321); admin channel and Space
commands [are refused](../../crates/waddle-server/src/admin/mod.rs#L59); and a
[mediated invitation grants nothing](../../crates/waddle-server/src/server/routes/websocket/handlers/message/muc_invite.rs#L272).

**W6 — Group-DM graph and legacy SQL mirror (concrete).**
[Member persistence](../../crates/waddle-server/src/admin/channels.rs#L4006) writes the permission actor and then
[permission_tuples through DbExecute](../../crates/waddle-server/src/admin/channels.rs#L4064). Failure attempts compensation
of the graph write. [Deletion](../../crates/waddle-server/src/admin/channels.rs#L4103) performs the inverse pair and may
restore graph membership if SQL deletion fails. These are separate commits with
possible partial/unknown outcomes, not a PostgreSQL transaction containing the
external graph mutation. Include the compensating paths as writers. Do not treat
this SQL mirror as the complete canonical grant journal. E3 is adjacent recovery
coverage, not an atomic graph/SQL proof. Both writers now
[refuse a group-DM identifier](../../crates/waddle-server/src/admin/channels.rs#L4052) that is the authorization
object of a room an enrolled scope manages, before either statement, since
group DMs and managed rooms share the channel identifier space.

**W7 — PubSub authorization inputs (concrete).** Generic owner-affiliations IQs
[write affiliation rows](../../crates/waddle-server/src/server/routes/websocket/handlers/iq/pubsub_admin.rs#L510); the storage [upsert/delete](../../crates/waddle-server/src/pubsub/implementation.rs#L409)
has no graph write. [Spaces set-role commands](../../crates/waddle-server/src/admin/spaces.rs#L1491) also change these
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
[Space deletion](../../crates/waddle-server/src/admin/spaces.rs#L1123) can remove or restore these inputs. Error results
and best-effort rollback do not prove no mutation happened. There is no single
SQL/graph revision covering the whole operation. A link is used as a projection
in one path and as a repair/discovery input in another; this inventory does not
resolve canonical parent authority. E5.

**W9 — Read/join-triggered repair (concrete).** An authorized Spaces items request
can execute [bookmark backfill](../../crates/waddle-server/src/server/routes/websocket/handlers/iq/spaces_items.rs#L97). It publishes a missing bookmark
and writes the channel's parent only when the channel has none or already has
that Space, retracting the bookmark when the parent write fails; it runs no
stale cleanup, and skips a room an enrolled scope manages or whose enrollment
cannot be told. [Join affiliation resolution](../../crates/waddle-server/src/server/routes/websocket/handlers/presence/muc/access.rs#L96) can invoke
[parent repair](../../crates/waddle-server/src/server/routes/websocket/handlers/presence/muc/access.rs#L554) after a denied channel check. Repair writes a graph
parent only for a channel that has none, naming the Space of the [channel-space link](../../crates/waddle-server/src/server/routes/websocket/handlers/presence/muc/access.rs#L688)
when that Space lists the channel's bookmark; a channel with a parent keeps it,
since only a placement moves it. Repair rechecks the bookmark after writing and
deletes its own write when the bookmark is gone. The
post-write recheck matters: a captured read alone does not prove the stale parent
survives. MAM explicitly disables graph repair. E5/E6. A room an enrolled
scope manages is [resolved from its canonical grants](../../crates/waddle-server/src/server/routes/websocket/handlers/presence/muc/access.rs#L154)
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

**S1 — Join.** [The join handler](../../crates/waddle-server/src/server/routes/websocket/handlers/presence/muc/implementation.rs#L885) freezes an actor admission revision before
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
a members-only room, and only through its channel row. An occupant at
another server enters through [the room host of the S2S router](../../crates/waddle-server/src/server/routes/websocket/s2s_rooms.rs#L833):
the same handler, given the evidence of the stream that carried the join,
decides peer admission and the trusted-home rule for the joiner before the
actor seats it and again on the seat the actor gave, and never creates a
room.

**S2 — Archive/discovery/extension reads.** The [MUC archive gate](../../crates/waddle-server/src/server/routes/websocket/handlers/presence/muc/access.rs#L276)
uses live room configuration when available, direct relations and managed-entry
rules; unmanaged rooms without a live actor fail closed. It is followed by a
separate [archive query](../../crates/waddle-server/src/server/routes/websocket/handlers/iq/archive_inbox_upload.rs#L184). There is no shared authorization snapshot
or transaction spanning those operations. Extensions have another
[room authorization entry](../../crates/waddle-server/src/server/extension_host_adapter/implementation.rs#L276) and [PubSub publication
authorizer](../../crates/waddle-server/src/server/extension_commands/pubsub.rs#L24), including manifest/deployment policy; these are not aliases for the
MAM sink. Rejection/error, returned archive rows and later network transmission
are different boundaries. E6. For a room an enrolled scope manages, the gate
and the room's disco#info decide from the canonical grants, and the archive
page is sent only if [a decision taken after the read](../../crates/waddle-server/src/server/routes/websocket/handlers/presence/muc/access.rs#L474)
still admits the requester. An asker at another server is served by a
second [archive reader](../../crates/waddle-server/src/server/routes/websocket/s2s_rooms.rs#L486): the same gate and the
same decision after the read, each given the evidence of the asker's
stream, around an archive query of its own.

**S3 — PubSub delivery.** The real database [subscriber query](../../crates/waddle-server/src/pubsub/implementation.rs#L376)
filters subscribed rows and outcasts. [Fanout captures that vector](../../crates/waddle-server/src/server/routes/websocket/handlers/pubsub_fanout/implementation.rs#L176), builds
a frozen event, expands resources and attempts the [live queue](../../crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L524) or
[detached SM buffer](../../crates/waddle-xmpp/src/stream_management/session_registry/resources.rs#L398). Private PEP has additional entitlement checks; do not
attribute those checks to generic Spaces fanout. Unsubscription/outcast/config
changes after capture are not uniformly rechecked at each sink. Queue acceptance,
SM custody, later socket write and peer acknowledgement are distinct events.
Best-effort errors/drops are not successful delivery. E4 verifies storage filtering,
not the capture-to-custody race or a no-bytes-after-revoke guarantee. Fan-out
from the Space node of an enrolled scope
[delivers nothing](../../crates/waddle-server/src/server/routes/websocket/handlers/pubsub_fanout/implementation.rs#L116), and a room-bound
extension node delivers only to subscribers who may still read the room.

A full JID that holds no connection is offered to [a delivery hook](../../crates/waddle-xmpp/src/registry/connection_registry.rs#L63),
unset unless the replica hosts rooms for occupants at other servers. The
S2S router [takes a stanza only for a seat](../../crates/waddle-server/src/s2s/route/router/hosting.rs#L597) it holds in the room
the stanza comes from, and only presence and groupchat messages; its queue
taking the stanza is recorded as the delivery of the room's frozen child,
which is transport custody and nothing more.

**S4 — Durable room recipients.** [The membership source](../../crates/waddle-server/src/server/durable_membership.rs#L104) concurrently
reads channel and Space owner/admin/member subjects and excludes channel outcasts.
[Hydration](../../crates/waddle-xmpp/src/muc/room_actor/implementation.rs#L2319) populates an actor mirror; source errors preserve the
session-observed/existing behavior rather than establishing fresh denial. Later
removal handling prunes and refreshes that mirror. These multi-call reads are not
one graph snapshot; the mirror and explicit/live occupants are separate recipient
inputs. E7. A room an enrolled scope manages is answered
[from its canonical grants](../../crates/waddle-server/src/server/durable_membership.rs#L126) instead: its durable
recipients are the subjects whose grants allow reading it.

**S5 — Canonical acceptance and replay.** [Ingress claim checks](../../crates/waddle-server/src/ingress/commit_room.rs#L463) concern
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
source. E8/E9. A message of an occupant at another server is
[submitted by the room host](../../crates/waddle-server/src/server/routes/websocket/s2s_rooms.rs#L714) with
[a principal of its own kind](../../crates/waddle-server/src/ingress/principal.rs#L25) that carries the
stream evidence of the sender's seat; acceptance decides the sender on that
evidence, and a room an enrolled scope manages decides each named recipient
at another server on [the evidence its seats were admitted under](../../crates/waddle-server/src/permissions/host_grant/principal.rs#L294).

## Existing evidence and its limits

These are **test source references inspected, not tests executed by this inventory
change**. Names or a green aggregate run alone do not attest prerequisites, the
backend, common snapshots, race ordering or production closure.

| ID | Existing test reference | What it can support / limit |
| --- | --- | --- |
| E1 | [write_and_delete_tuple_with_typed_payloads](../../crates/waddle-server/src/permissions/actor/tests.rs#L52) | Typed actor behavior; its helper uses an in-memory database/local backend, not SpiceDB. |
| E2 | `owner_reseed_keeps_bans_skips_enrolled_spaces_and_refuses_remote_accounts` in `crates/waddle-server/tests/xep0045_host_grant_fence.rs` (outside the scan roots, so not anchored) | The seed keeps an Outcast on the in-memory and SQL stores, skips an enrolled Space node and refuses remote privileged seeds; executed in the writer fence's focused set (host-grant design, evidence of the writer fence). |
| E3 | [rollback_keeps_affiliation_committed_by_a_later_owner](../../crates/waddle-server/src/server/routes/websocket/handlers/iq/muc_admin.rs#L3645) | Conditional rollback behavior in the test fixture; not cross-store atomicity or F0 writer closure. |
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

The selected evidence was refreshed a twelfth time, through
`9b2e2d57ebd13329ddc045b2d1343f03b9619089`, after reviewing the delta of the
reviewed files since the eleventh refresh:

- The room's own message (`hg-system-message` moved) reads its audience after
  the archive write at most twice before it delivers to no occupant; the
  second audience decision's `system_message_audience` call moved into that
  loop, so the raw scan still holds 1596 records. It delivers to no one the
  first decision did not name and writes nothing it did not write before.

The selected evidence was refreshed a thirteenth time, through
`c9111c714cfb821e571cc7f3b63d4a9ef37d11ba`, the effect boundary for remote
principals, after reviewing the delta of the reviewed files since the sixth
refresh on the effect boundary's own line of history, which did not yet hold
the seventh to twelfth refreshes:

- Room-manifest acceptance (`hg-admission`; `claim` moved), the writer
  fence's gate and its room reads (`hg-gate`, `hg-room-read`) decide whose
  grants are honoured from the replica's principal policy (the served
  account domain and the S2S snapshot it runs) instead of the served
  account domain alone. For a remote principal that only narrows what was
  honoured with S2S off; with S2S on it honours a trusted home's owner and
  admin grants on its `pkix` streams, and no surface serves remote
  principals yet.
- The topology bootstrap (`hg-topology`; `topology` moved), the Spaces
  owner seeds of Space and node creation, projection recovery
  (`hg-recovery`) and room administration take their issuance policy, and
  so their trusted homes, from that same value. The seed itself (`seed`)
  still writes only `set_affiliation_unless_outcast`. Startup refuses a
  trusted home that is a service domain, or a service domain that is not
  canonical, right after loading the S2S snapshot; `startup` and
  `startup-schema` moved and are unchanged.
- Room administration's moderator-role check decides from the occupant's
  witness, which grants the role to served accounts only; the rollbacks
  (`admin-rollback`, `e-rollback`) moved and are unchanged.
- Account bootstrap (`bootstrap`, `reconcile`, `hg-bootstrap-d8`) changed a
  comment only; `startup-call`, `space-admin` and `space-delete` moved.
- The raw scan keeps 1595 records with the same counts. One record changed
  its text: `HostGrantAuthority` records an affiliation change through the
  store's `set_affiliation_as`, which carries the initiator's evidence and
  which the scan's `set_affiliation` token does not match, and a
  documentation reference to `set_affiliation` beside it takes its place in
  the count. The rest moved lines. No production grant writer, grant store
  or acceptance authority was added.

The selected evidence was refreshed a fourteenth time, through
`763988ff0b1c466d659033ca398623cf9999fc11`, the merge of the effect boundary for remote
principals with the revocation fence, after reviewing the merged tree against
both of its parents:

- The writer fence's gate (`hg-gate`, `hg-room-read`, `hg-admission` moved)
  holds, in each managed room's decision, the replica's principal policy and
  the revocation fence's ticket together. The ticket is asked in
  `ManagedRoomGrants::allows_as` and `standing_as`, which the stream-less
  `allows` and `standing` call, so a decision with stream evidence answers
  nothing once its ticket is no longer current either. `still_allows` takes
  the initiator's evidence and asks the same check; the room administration's
  role list passes a local session's (none).
- The revocation sweep (`hg-revocation-sweep`) reads each account's standing
  and read access from the grants as stored under the principal policy,
  without stream evidence; the ticket-free standing read is
  `ManagedRoomGrants::standing_unfenced`. It writes what it wrote before.
- The raw scan holds 1596 records. Against the revocation fence's parent, the
  one record that changed is the effect boundary's own, described in the
  thirteenth refresh (`HostGrantAuthority` records through
  `set_affiliation_as`); against the effect boundary's parent, the records
  that differ are those of the eighth to twelfth refreshes. The rest moved
  lines; `startup`, `startup-schema`, `startup-call`, `bootstrap`,
  `reconcile`, `topology`, `seed`, `admin-rollback`, `space-admin`,
  `space-delete`, `claim`, `e-rollback`, `hg-bootstrap-d8`, `hg-topology` and
  `hg-recovery` moved with them. No production grant writer, grant store or
  acceptance authority was added.

The selected evidence was refreshed a fifteenth time, through
`a63296597d505f0a542432654ba4e3487c799dc0`, after reviewing the delta of the reviewed files
since the fourteenth refresh:

- Every surface of the writer fence names the stream evidence of the
  decision it makes: `ManagedRoomGrants::allows_as` and `standing_as` and
  `HostGrantGate::allows_as` with `PrincipalEvidence::NoStream`, for the
  local sessions and named subjects every surface serves today. The
  stream-less `allows` and `standing` (in `gate.rs`, whose `hg-gate` anchor
  did not move), and the
  stream-less `set_affiliation` of the store and the authority, are built
  for test support only; room administration (`hg-room-admin`) records
  through `set_affiliation_as`. Each decides exactly what it decided before.
  The surfaces' anchors moved with the longer calls: `hg-owner-config`,
  `hg-moderation`, `hg-pin-list`, `hg-system-message`, `hg-mirror-repair`,
  `hg-read-recheck`, `archive-gate`, `join-repair`, `join-repair-link`,
  `hg-group-dm`, `group-dm`, `group-dm-sql` and `group-dm-delete`.
- The revocation sweep (`hg-revocation-sweep`) removes a seated occupant of
  a domain the deployment does not serve only when its grants deny it
  whatever stream its requests arrive on (a ban, no membership recorded, a
  scope that admits nobody); it removes less than before, never more, and
  writes nothing else.
- Reconciliation drops, as an operation of its own that lowers recorded
  rows to their effective affiliation, a recorded increase whose remote
  initiator the replica's snapshot no longer admits or lists as a trusted
  home (`HostGrantStore::drop_unhonoured_increases`, in
  `permissions/host_grant/store.rs` and `authority.rs`). It lowers grant
  rows only; it never raises one and never activates anything.
- Startup's check of the trusted-home list (`server/lifecycle.rs`) and the
  issuance policy count the account domain's call mixer as a service
  domain.
- The raw scan holds 1594 records, two fewer: the room administration's
  call and a documentation reference of `HostGrantAuthority` no longer match
  the `set_affiliation` token. No production grant writer, grant store or
  acceptance authority was added.

The selected evidence was refreshed a sixteenth time, through
`8f6088ba601528090ec1bc82294b1a12422f5519`: no reviewed file and no anchor changed. In
`permissions/host_grant/authority.rs`, outside the reviewed set, a
reconciliation pass looks for increases to drop only while the scope's plan
waits for its projection; that file's three records (`WriteHostGrantProjection`
twice and `set_affiliation`) moved lines. The raw scan holds 1594 records, unchanged in content.

The selected evidence was refreshed a seventeenth time, through
`31fd3f41591d0ff3072dc667e35207c522ae0dc6`, after reviewing the delta of the reviewed
files since the sixteenth refresh. This refresh covers inbound S2S rooms:
occupants at other servers in rooms of this deployment.

- A new reader, a new acceptance producer and a new delivery sink exist,
  and are now part of the reviewed set:
  - the room host of the S2S router (`s2s-room-host`, in
    `server/routes/websocket/s2s_rooms.rs`) decides what a remote principal
    asks of the MUC service, for that principal and the evidence of its
    stream. It reads a room's archive (`s2s-room-archive`: the gate, the
    query, and for a managed room the decision after the read) and submits
    a remote occupant's message to the ingress authority
    (`s2s-room-message`) with a principal that carries its seat's stream
    evidence (`remote-acceptance-principal`, in `ingress/principal.rs`);
  - a hook of the connection registry (`foreign-delivery`) offers a
    stanza for a full JID that holds no connection, and the router takes it
    only for a seat it holds (`s2s-seat-egress`, in
    `s2s/route/router/hosting.rs`): presence and groupchat messages from
    the room the seat is in, nothing else. `send_to` and
    `try_send_to_matching`, which `try_send_to` (`queue`, moved) calls,
    offer the hook a stanza for which they found no connection; recovery's
    `try_send_outbound_to_locally_hosted` does not. The hook is unset
    unless the replica hosts rooms for occupants at other servers.
- It writes no grant. No grant writer, grant store or projection writer was
  added, and the room host records no affiliation: room administration is
  refused to remote principals before any handler.
- The join (`join`, moved) carries the evidence it decides with. For a
  remote principal it decides peer admission first, refuses a stored owner
  or admin entry the trusted-home rule does not honour, never creates a
  room, and gives up a seat the actor gave on a standing that is not
  honoured. The wrapper `resolve_managed_channel_affiliation` is gone; the
  `join-resolver` anchor names `resolve_managed_channel_admission`, which
  it only delegated to. A remote principal's entry never runs the parent
  repair (`join-repair`, `join-repair-link` moved). `hg-join`,
  `archive-gate` and `hg-read-recheck` moved with the evidence-naming forms
  beside them.
- Room-manifest acceptance (`hg-admission`, `claim` moved) decides the
  sender on the evidence its submission carries and a named recipient at
  another server on the evidence its seats were admitted under
  (`hg-seat-evidence`, in `permissions/host_grant/principal.rs`).
- The revocation sweep (`hg-revocation-sweep`, moved) reads the grants as
  a type that admits nothing (`hg-stored-grants`; `hg-gate` and
  `hg-fence-ticket` moved, and the ticket that was always current is
  gone). It decides a seated account on its effective affiliation; a
  remote occupant on its seat's evidence, or, where the replica holds
  none, removes it when the grants deny it on every stream, which now
  includes a domain that is not admitted. It removes more than before in
  those two cases and writes nothing else.
- Claim-move re-seating (`hg-claim-move`, moved) seats a recorded occupant
  at another server only when the replica takes over its seat, and decides
  it on the seat's evidence.
- The `muc#admin` writer of a room no scope manages refuses an owner or
  admin affiliation for an account whose home is neither served nor
  trusted, and the moderator role for an occupant whose seat does not
  honour room moderation, before it persists anything (`room-admin` did
  not move; `admin-rollback` and `e-rollback` moved). In
  `muc_admin_host_grant.rs` the moderator check reads seat evidence;
  `hg-room-admin` and `hg-mirror-repair` did not move.
- Startup (`startup-call`, `hg-recovery` moved) attaches the room host to
  the S2S router when S2S is enabled.
- `standing.rs` gained the effective-affiliation accessor the sweep reads
  (`hg-room-read` moved).
- The raw scan holds 1597 records, three more: the room host's
  `query_messages`, and two `WriteTuple` matches in a test-support helper
  that makes an account a deployment owner. The two `host_grant_affiliation`
  calls of the archive gate changed their arguments and count as before.
  Two tokens were added to the scan, `offer_foreign` and
  `RemoteOccupantPrincipal`, for the new sink and the new producer; they
  match ten further records, in the files named above, the two call sites
  of the hook (`route_to_connection/implementation.rs` and the room
  presence router in the join handler's file) and the re-export in
  `ingress/mod.rs`. The scan holds 1607 records in all.

The selected evidence was refreshed an eighteenth time, through
`f6eea18f3a360e216b33d689395af8a7d487b8eb`, after reviewing the delta of the reviewed
files since the seventeenth refresh. This refresh covers what remote
principals were given since: room administration, rooms opened to other
servers by a setting of their own, the standard message extensions, and a
request carried to the replica that holds its room.

- Remote principals now reach writers. The room host
  (`s2s-room-host`, moved) hands a `muc#admin` request (`s2s-room-admin`)
  and a `muc#owner` or XEP-0425 request (`s2s-room-owner`) to the handlers
  a session's requests reach, through two entry points that pass the
  evidence of the request's stream and no session (`s2s-remote-admin`,
  `s2s-remote-owner`, in `handlers/iq/muc_remote_admin.rs`, new in the
  reviewed set). No writer was added: the handlers are the existing ones,
  and every decision they take for the requester now names the evidence
  it was passed.
  - In a room an enrolled scope manages, `record_host_grant_affiliation`
    (`hg-room-admin`, moved) passes that evidence to
    `set_affiliation_as`, so the store decides a remote initiator under
    the scope's guard on its stream; the owner request's decision
    (`hg-owner-iq`, moved), the configuration's two re-reads
    (`hg-owner-config`, moved) and the moderation decision
    (`hg-moderation`, moved) name it too. `hg-mirror-repair` moved.
  - In any other room the standing the room actor holds for the requester
    is honoured for its evidence before it decides anything
    (`admin-context-honoured`, in `muc_admin.rs`; `room-admin`,
    `admin-rollback` and `e-rollback` moved), and the owner check and the
    moderator check do the same. A remote principal's configuration
    request is passed no session, so the owner relationship a session's
    request writes is not written for it.
  - The cluster's forward of a `muc#admin` list request names its
    requester without evidence and is taken only for a session's request.
- The admin command that sets a room affiliation (`admin-set-affiliation`,
  in `admin/channels.rs`) refuses an owner or admin affiliation for an
  account that is neither served nor of a trusted home before it persists
  anything. `group-dm`, `group-dm-sql`, `group-dm-delete` and
  `hg-group-dm` moved with it. The Spaces relations are still written
  without that check.
- A room is reached by a remote principal only while its `federated`
  setting is on (`s2s-room-federated`), decided before its grants are
  read; `s2s-room-archive` and `s2s-room-message` moved, and the archive
  gate no longer decides a group conversation, which is never federated.
  The setting is a field of the room configuration and a column of the
  channel catalog (`topology.rs` and the join handler read it; the owner's
  configuration writes it); it grants nothing by itself.
- A new producer of stream evidence exists. A replica builds a
  `RemotePrincipal` for a room it holds from the facts of a stream another
  replica verified (`s2s-carried-principal`, in `s2s/route/principal.rs`,
  new in the reviewed set), which arrive over the cluster relay
  (`s2s-relayed-request`, in `server/routes/websocket/s2s_room_relay.rs`,
  new in the reviewed set) and are queued by the router
  (`s2s-carried-request`) after it checked the stanza against them. The
  host asks where a request is decided before any seat is opened for it
  (`s2s-room-placement`); a room the channel catalog says is not
  federated is not carried and is answered where its request arrived. The
  carried facts are the address, the
  strength, the trust-profile kind and the snapshot generation; admission
  and standing are decided by the receiving replica.
  `hg-seat-evidence` moved.
- The room effect outbox offers a recipient without a connection to the
  delivery hook before it looks for a resource registered on another node
  (`outbox-seat-delivery`, in `room_effect_outbox/drain.rs`, new in the
  reviewed set), so a seat receives what a durable mutation caused. The
  hook takes it only for a seat it holds (`s2s-seat-egress`, moved), and
  gained the call that ends a room's seats when its setting is turned off
  (`connection_registry.rs`).
- A remote occupant's message is rebuilt from an allowlist before the room
  plans it; a correction or retraction is the sender's own only when the
  archived message records its account as the author
  (`room_dispatch.rs`, and `groupchat_validation.rs` outside the reviewed
  set). The inbox and activity projections write only for an address the
  deployment serves. Neither is a grant writer.
- `xmpp_state.rs` re-exports the catalog's setter for the setting.
- The raw scan holds 1608 records of the tokens it held, one more: the
  outbox's `offer_foreign`. Four tokens were added to the scan,
  `carried_room_request`, `muc_admin_for_remote`,
  `muc_owner_or_moderation_for_remote` and `CarriedPrincipal`, for the new
  entry points and the new producer; they match 24 further records, in
  the files named above, the router's public method, the carried
  request's type in `s2s/route/local_rooms.rs`, the trait method of the
  remote route in `destination.rs` and the re-exports in `s2s/route.rs`
  and `handlers/iq/mod.rs`. The scan holds 1632 records in all.

The selected evidence was refreshed a nineteenth time, through
`ab055f27480da80b43e15087f17e699199cce190`, after reviewing the delta of the reviewed
files since the eighteenth refresh. This refresh covers the changes to
inbound S2S rooms that followed their review: where a request is placed,
how a seat ends, and how decisions that name a seated remote occupant
read its seats. No grant writer, storage plane or producer of stream
evidence was added.

- Issuance gained a refusal. `seats_honour_privilege` (`hg-seats-honour`,
  new anchor, in `muc_admin_host_grant.rs`) reads the evidence of every
  seat an account holds in a room on the replica; the `muc#admin` writer,
  for a room an enrolled scope manages and for any other
  (`check_privileged_seats`; `admin-rollback`, `e-rollback` and
  `admin-context-honoured` moved), and the admin command that sets a room
  affiliation (`admin-set-affiliation`, not moved) refuse an owner or
  admin affiliation while one of those seats would not honour it, before
  anything is persisted. An account without a seat there is decided as
  before.
- A room's own message decides each occupant of a managed room on the
  evidence of its seats (`hg-system-message`, moved; `occupant_reads`),
  where it passed no stream evidence: a remote occupant receives its copy
  only if every one of its seats admits it to read. It reads grants and
  writes nothing.
- Claim-move re-seating (`hg-claim-move`, moved) still seats a recorded
  occupant at another server only on a seat the replica took over, and
  now has the router end each such seat once the room has seated its
  occupants (`s2s-adopted-seats`, new anchor, in `router/hosting.rs`): the
  room removes the occupant with status 333.
- The join handler undoes a seat it gave on a standing that is not
  honoured without announcing a departure (`MucRemovalCause::Unannounced`,
  in `waddle-xmpp`'s presence module, outside the reviewed set; the
  departure helpers in `cleanup.rs`, `local_departures.rs` and the session
  janitors skip their broadcasts for it). The decision that undoes the
  seat is unchanged.
- `room_dispatch.rs` runs the extensions' enrichment hooks only for a
  sender at a domain this deployment serves.
- The room host (`s2s-room-host`, `s2s-room-archive`, `s2s-room-message`,
  `s2s-room-admin` and `s2s-room-owner` moved) answers two further
  questions of the router, both read-only: every occupant a room seats
  with its generation, and whether it keeps rooms on other replicas. It
  answers "cannot be told" where a room's actor cannot be reached. Where
  a seated occupant's presence enters the room again it says so without
  deciding, and the router asks again with a seat opened for the join.
  `s2s-room-federated` moved: where the room's actor cannot be reached, an
  address that cannot be open to other servers is not found, and only a
  room that may be open is answered with a retryable error.
- `s2s-room-placement` moved and is asked from a different place: the
  router's placement stage, off the workers that hand requests to the
  host, one request of a room at a time, within a bound in time. It is
  still asked before any seat is opened for the request, and a host that
  keeps every room itself is not asked. The cluster's carrier carries
  only a room that may be open to other servers (`s2s-room-catalogued`,
  new anchor, in `s2s_room_relay.rs`; `s2s-relayed-request` moved): one
  whose catalog row says so, or one without a row while the deployment
  opens its new rooms.
- The router (`s2s-seat-egress` and `s2s-carried-request` moved) takes a
  carried request only for a room at the MUC service's domain; opens a
  seat only where a queue for the occupant's domain has a place; sends a
  room's own presence that ends an occupancy, when it did not come as the
  answer to the occupant's request, only once the room confirms that it
  no longer seats that generation; and removes the occupant from its room
  when the host refuses a seated occupant's request in a way that ends
  its occupancy. Each narrows what leaves for a remote JID or what a room
  keeps seated.
- The delivery hook has one more caller. Executing the frame that is a
  sender's own copy of its room message offers it to the hook
  (`s2s-reflection-seat`, in `interpret/effects/immediate.rs`, new in the
  reviewed set), which takes it only for a seat it holds in the room the
  message comes from; the frame is still returned and settled as before,
  and the router does not send the same archived message to a seat twice.
- `server/lifecycle.rs` has the S2S connection manager drain once the
  router has told the remote occupants of its rooms that it is stopping,
  and reads the router's pair queues from the snapshot; `startup` and
  `startup-schema` did not move. `actor-affiliation` and `actor-config`
  moved for a read-only room actor message that lists the generations of
  the seated sessions.
- The raw scan holds 1633 records, one more: the `offer_foreign` of the
  sender's own copy. 87 unchanged records moved because the files above
  gained lines.

The selected evidence was refreshed a twentieth time, through
`99f30be6766e510672dbaa8c421a873ab758b9cd`, after reviewing the delta of the reviewed
files since the nineteenth refresh. This refresh covers fixes to inbound
S2S rooms: the bounds on the work that hands requests to the room host,
how a seat ends, how a restored room's seats are taken over, and the
issuance check of the admin command. No grant writer, storage plane or
producer of stream evidence was added.

- Issuance reads one more source. The admin command that sets a room
  affiliation (`admin-set-affiliation`, not moved) may run on a replica
  that does not hold the room, so it now refuses an owner or admin
  affiliation on `seats_honour_privilege_anywhere` (`hg-seats-honour-anywhere`,
  new anchor, in `muc_admin_host_grant.rs`): the evidence of every seat
  the account holds in the room on any replica, read from the shared seat
  table, as well as on this one; it refuses when the table cannot be
  read. The `muc#admin` writer runs on the room's holder and is unchanged
  but for its refusal text (`admin-rollback`, `e-rollback` and
  `admin-context-honoured` moved).
- The join handler (`join`, moved) still undoes a seat it gave on a
  standing that is not honoured without announcing a departure, and now
  reads the generations the room seats afterwards to send the joiner's
  unavailable presence to the occupants that entered between the seat and
  its undoing. The decision that undoes the seat is unchanged; nothing is
  written.
- Claim-move re-seating (`hg-claim-move`, moved) seats a recorded occupant
  at another server only on a seat of the recorded generation: one this
  replica already holds, when the room's actor was restored on the replica
  that seats it, which goes on; or one it takes over from a replica that
  stopped (`s2s-seat-adoption`, new anchor, in `router/hosting.rs`), which
  ends once the room has seated its occupants (`s2s-adopted-seats`,
  moved). A row taken over for another generation is removed, and its
  occupant is not seated.
- The room host (`s2s-room-host`, `s2s-room-archive`, `s2s-room-message`,
  `s2s-room-federated`, `s2s-room-admin`, `s2s-room-owner` and
  `s2s-room-placement` moved) answers a self-ping, and refuses a subject
  change, with a retryable error where the room's actor or configuration
  cannot be read, instead of the answers for "not joined" and "not
  allowed"; a subject change is put to no room it could not decide. Its
  removal of an occupant whose seat ended now says whether it removed that
  generation, and it removes, pinned to its generation, an occupant of
  another server the router holds no seat for (`s2s-unseated-removal`, new
  anchor). Its own asks of a room actor wait 2 s instead of 5 s.
- The router (`s2s-seat-egress` and `s2s-carried-request` moved) answers
  that a JID at a served domain, or in a room without a seat, holds no
  seat without reading the seat index; refuses a request for a room whose
  share of the queues is full instead of holding its stream; removes a
  room's occupant of another server that holds no seat; and refuses a join
  decided while the replica stops. Each narrows what leaves for a remote
  JID or what a room keeps seated.
- The cluster's carrier (`s2s-room-catalogued` and `s2s-relayed-request`
  moved) carries the same rooms, and reports an owner that did not answer
  or could not be reached as such, so that the router answers that room's
  requests without asking for a while.
- `server/lifecycle.rs` chooses the token the S2S connection manager
  drains on through `S2sRouter::service_stop`, the same choice as before;
  `startup` and `startup-schema` did not move.
- The raw scan holds 1633 records, as before. 74 unchanged records moved
  because the files above gained lines.

The selected evidence was refreshed a twenty-first time, through
`0e81e9463d8b0621b24ffbdaf605edd9a28a19e4`, after reviewing the delta of the reviewed
files since the twentieth refresh. This refresh covers fixes to inbound
S2S rooms: the allowances that bound what one remote account may send,
the answer to a join decided while the replica stops, the end of a seat
taken over from a stopped replica, and the deadline on placing a carried
request. No grant writer, storage plane or producer of stream evidence
was added, and no decision of standing moved.

- The join handler (`join`, moved) undoes a seat it gave on a standing
  that is not honoured as before. The notice to the occupants that
  entered while it was seated now reads the room once, within 2 s, on a
  task of its own, and leaves out an occupant that now holds the joiner's
  nickname; nothing is written.
- The room host (`s2s-room-host`, `s2s-room-message`, `s2s-room-admin`,
  `s2s-room-owner`, `s2s-room-placement` and `s2s-unseated-removal`
  moved) answers no headline, and no message that carries only a chat
  state or a marker, with an error, and passes the join handler the state
  it may run that notice with.
- The router (`s2s-seat-egress`, `s2s-carried-request`,
  `s2s-seat-adoption` and `s2s-adopted-seats` moved) decides every
  allowance of a remote account before its domain's rate and drops or
  answers what is beyond one instead of holding the stream; decides under
  the seat index's lock whether a join may be answered as entered once
  the replica stops; holds a seat taken over from a stopped replica only
  where no seat is held; keeps a taken-over seat's end over a removal of
  the same occupancy as one without a seat; takes a seat's first leave
  beyond its account's allowance on the replica that holds the seat;
  keeps a leave that cannot be placed to place it again, and forgets it
  once its sender's presence enters the room again; and ends a seat on a
  `groupchat` bounce with
  `service-unavailable`. Each narrows what reaches a room or a remote JID,
  or what a room keeps seated.
- The cluster's carrier (`s2s-room-catalogued` and `s2s-relayed-request`
  moved) bounds its lookups and asks for one request by one deadline, and
  reports an owner as silent only where an ask ran out of time.
- The raw scan holds 1633 records, as before. 31 unchanged records moved
  because the files above gained lines.

The selected evidence was refreshed a twenty-second time, through
`468bc0fb99fce93441d7935f3b7ab71086ade73e`, after reviewing the delta of the reviewed
files since the twenty-first refresh. This refresh covers the end of a
remote seat, the leaves kept to be placed again and the carrier's
deadline. No grant writer, storage plane or producer of stream evidence
was added, and no decision of standing moved.

- The router (`s2s-seat-egress`, `s2s-carried-request`,
  `s2s-seat-adoption` and `s2s-adopted-seats` moved) no longer ends a
  seat on a `service-unavailable` bounce, only on the delivery-related
  conditions XEP-0045 §17.1.2 lists; keeps a leave that cannot be placed
  for 120 s from when it was first kept, within a bound for each remote
  account as well; and forgets such a leave once a later presence of its
  sender for the room is placed, taking it out of the room's lane if it
  waits there again. Each narrows what a room keeps seated or what a
  stale leave can end.
- The cluster's carrier (`s2s-room-catalogued` and `s2s-relayed-request`
  moved) reports an owner as silent only for an ask that began with the
  owner's two windows ahead of it; an ask begun later that runs out is a
  retryable refusal that says nothing of the owner.
- The raw scan holds 1633 records, as before. 4 unchanged records moved
  because the files above gained lines.

On the account-traffic branch (W6c), merged after the twenty-second
refresh, the selected evidence was refreshed three times of its own. The
first, through
`69beb8862c2c540a1b85e202bdcc0b457cdac5c2`, on the account-traffic branch
(W6c of issue 2047). In the reviewed set only `server/http.rs` changed:
startup now also attaches the S2S router's account side
(`WebSocketLocalAccounts`) beside its local delivery, and passes the
deployment's served domains to the startup roster prune while S2S is
enabled, so that contacts at domains it does not serve stay in rosters.
Neither writes a grant, a projection or a membership, and the two anchors
in that file (`startup-call`, `hg-recovery`) moved lines only. Outside the
reviewed set, records of `try_send_to`, `record_host_grant_affiliation`,
`EnrolledScopes`, `record_stanza_for_detached_bound_resource` and
`pubsub_affiliations` moved lines in `test_support.rs`,
`handlers/iq/roster/push.rs`, `handlers/presence/subscription/delivery.rs`
and `server/runtime_owner.rs`. The raw scan holds 1594 records, unchanged in
content.

The second, through
`b3ec89bbe41ae4b2692f143120321de43c02c52c`, on the same branch. In the
reviewed set only `server/http.rs` changed: the startup roster prune now
receives the deployment's served domains whether S2S is enabled or not,
so that contacts at domains it does not serve stay in rosters while S2S
is off. It writes no grant, projection or membership; the `startup-call`
anchor moved from line 1568 to 1564 and `hg-recovery` did not move.
Outside the reviewed set, records of `pubsub_affiliations` in
`server/runtime_owner.rs` (a new runtime grant for
`s2s_remote_offline_admissions` above it) and of `EnrolledScopes` and
`record_host_grant_affiliation` in `test_support.rs` (a new test seam that
runs the startup reconciliation above them) moved lines. The raw scan holds
1594 records, unchanged in content.

The third, through
`43e5a44329974d3ea1ee2ebcb47c7472a2a8391c`, on the same branch. In the
reviewed set only `auth/retirement.rs` changed: `retire_in` also deletes,
in the retirement's transaction, the retired account's rows of
`s2s_pending_subscriptions`, `s2s_remote_pubsub_subscriptions` and
`s2s_remote_offline_admissions`, which hold remote senders' subscription
requests, the account's records of remote PubSub subscriptions and the
offline admissions of strangers' messages. It writes no grant, projection
or membership and reads no authority; the `account-retire` anchor moved
from line 119 to 121. Outside the reviewed set, records of
`JoinWithAffiliation` and `JoinAffiliationGrant` in
`server/routes/websocket/cleanup.rs` (a check of remote senders' stored
messages before a terminal redrive above them) and of `try_send_to` in
`handlers/iq/roster/push.rs` (a new parameter of the removal side effects
above them) moved lines. The raw scan holds 1594 records, unchanged in
content.

The selected evidence was refreshed a twenty-third time, through
`0afd0a567668c17a87acc63c3b836f2025a3bb16`, on the integration branch
after the account-traffic branch was merged onto the twenty-second
refresh. No grant writer, storage plane or producer of stream evidence
was added by the merge, and no decision of standing moved.

- `server/http.rs` (`startup-call` and `hg-recovery` moved) both hosts
  the rooms remote occupants use and attaches the router's account side
  at startup; each was reviewed on its own branch.
- `s2s/route/router/hosting.rs` (`s2s-seat-egress`,
  `s2s-carried-request`, `s2s-seat-adoption` and `s2s-adopted-seats`
  moved) names the account side's queue work among what the workers
  that hand requests to the room host ignore: that work is queued only
  on the room traffic queues.
- `auth/retirement.rs` keeps the account-traffic branch's fingerprint.
- The raw scan holds 1633 records, unchanged in content. 22 records moved
  because the merged files gained lines: in `s2s/route.rs`,
  `s2s/route/router.rs`, `s2s/route/router/hosting.rs`,
  `server/routes/websocket/cleanup.rs`, `handlers/iq/roster/push.rs`,
  `handlers/presence/subscription/delivery.rs`, `server/runtime_owner.rs`
  and `test_support.rs`.

The selected evidence was refreshed a twenty-fourth time, through
`4fceb7ea55331c5b46b5fd60e636f91348e4b211`, after reviewing the delta of the reviewed files since the
twenty-third refresh. Only `server/routes/websocket/s2s_rooms.rs`
changed. No grant writer, storage plane or producer of stream evidence
was added, and no decision of standing moved.

- Hosting rooms for remote occupants (`s2s-room-host`, `s2s-room-archive`,
  `s2s-room-message`, `s2s-room-federated`, `s2s-room-admin`,
  `s2s-room-owner`, `s2s-room-placement` and `s2s-unseated-removal`
  moved) also binds the app state's host-grant gate to the ingress
  authority's attachment gate, which startup binds only where a canonical
  grant store exists. The surfaces that decide a seated remote occupant's
  moderator role and an owner or admin standing for its account then read
  the seats' evidence on every deployment; before, on one without a store,
  they found no seat, refused the role to a trusted home's occupant and
  issued a standing to an account seated on a dialback stream. Binding
  the gate already bound changes nothing, and the gate's store still
  decides which rooms are managed.
- The raw scan holds 1633 records, unchanged in content. 4 records in
  that file moved because it gained lines.

The selected evidence was refreshed a twenty-fifth time, through
`18e9f931904581aed7f848d0cd5458c0326b7226`, after reviewing the delta of the reviewed files since the
twenty-fourth refresh. Eight reviewed files changed, four files were added
to the reviewed set, and seventeen anchors moved without a change to their
text.

- The permission actor's write messages (`WriteTuple`,
  `ReplaceExclusiveRelation`, `SwapExclusiveRelation`) gained a refusal
  (`d8-actor-privileged-issuance`): the channel `manager` and `moderator`,
  the Space `moderator` and the role `manager` relations are written for a
  user subject only when its home is a served domain or a trusted home
  (ADR-0020 D8). No production call site names those relations; the actor
  is the one place every writer passes. The policy is attached at startup
  from the account domain and the S2S snapshot the replica runs
  (`d8-actor-policy`); an actor without one, as suites build, asks nothing.
  `local`, `hg-writer-fence` and `hg-reader-fence` moved. No read path
  changed.
- Two writers of a Space's PubSub affiliations decide D8 issuance:
  `spaces:set-role` for the owner and admin roles (`d8-space-set-role`),
  and the `<affiliations/>` change on the Spaces service for the owner,
  publisher and publish-only affiliations (`d8-space-affiliations`), before
  any change of a request is applied. `space-admin`, `space-delete`,
  `hg-admin-space-listing` and `pubsub-admin` moved.
- The Spaces PubSub service is reachable by remote principals
  (`s2s-space-host`), so the PubSub writers of the Spaces service
  (configuration, purge, deletion, affiliations, subscriptions, and publish
  and retract with their channel placement) are reachable from an S2S
  stream through one entry (`s2s-space-remote-change`). They are reached
  only after peer admission, after the node was found open to the asker
  (`s2s-space-open`) and only for a principal whose privilege the effect
  boundary honours (`s2s-space-change`): a trusted home on a `pkix` stream.
  The handlers then decide as for a session, with the remote account as
  their subject: the node's Owner affiliation for the `pubsub#owner`
  requests, and the Space `owner` relationship and authority over the room
  for a placement. No writer issues the Space `owner` relationship to a
  remote account. A remote principal's PubSub affiliation is honoured by
  `s2s-space-honoured`: a ban always, a membership on any admitted stream,
  and the privileged affiliations only with honoured privilege, otherwise
  as nothing.
- Two storage planes were added in the global database (migrations 2012
  and 2013): `s2s_federated_spaces`, the Spaces whose `federated` setting
  is on, written by a Space owner's configuration form through a session
  (`s2s-space-federated-setting`) and removed when the Space is deleted;
  and `s2s_space_subscriptions`, the subscriptions remote accounts made
  themselves (`s2s-space-subscription-record`), one for an account and a
  Space, each with the stream evidence of its request. Neither is a grant store: the first only opens
  a Space to requests that are then decided, and the second only admits
  notifications. Rows of the second are a second source of recorded stream
  evidence beside the seat table (`hg-seat-evidence` moved; the
  constructor is unchanged and its comment names both).
- The PubSub fan-out (`fanout` and `hg-fanout-recheck` moved) skips a
  Space's subscribers at other servers in its local loop and hands the
  event to `s2s-space-notify`, which decides each on the evidence its
  subscription recorded, reads the deliverable subscriptions again for the
  publication, tells a subscriber only of a room it may be told of (the
  catalog's `federated` column, read once for the publication, and
  `HostGrantGate::allows_as` for a managed room; `s2s-space-room-visible`
  is the same rule for an item read) and queues one stanza for each on the
  router (`s2s-space-event-egress`), which checks peer admission again at
  the write. The pass returns before any read on a replica that runs no
  S2S snapshot. Fan-out from an enrolled scope's Space
  still returns before any of it.
- `server/http.rs` attaches the Spaces host where S2S is enabled
  (`startup-call` and `hg-recovery` moved), and the router's hosting module
  reports the Spaces service as reachable where a host is attached and
  shares two rate helpers (`s2s-seat-egress`, `s2s-carried-request`,
  `s2s-adopted-seats` and `s2s-seat-adoption` moved). `startup-schema`
  moved with the lines added to `server/lifecycle.rs`.
- The raw scan holds 1635 records, two more than before, both in
  `server/routes/websocket/s2s_spaces.rs`: the Spaces host's `space_enrolled`
  check and the notification pass's `list_deliverable_subscribers` read.
  The other records are unchanged in content; those in the changed files
  moved with the lines added.

The selected evidence was refreshed a twenty-sixth time, through
`42cbdd9b127e9256dada6ea3a1ad19fe87226510`, after reviewing the delta of the reviewed files since the
twenty-fifth refresh. Twelve reviewed files changed, no file was added to
the reviewed set, twenty-nine anchors moved without a change to their
text, and one anchor's text changed (`s2s-space-notify`, which is no longer
`pub(crate)`).

- A Space deleted through PubSub (`pubsub-admin` moved), by a session or by
  a trusted home's owner through `s2s-space-remote-change`, now runs the
  cascade of `spaces:delete` (`space-delete` moved; the cascade is shared
  by `run_delete` and the PubSub entry `delete_space`, and works on the
  PubSub store, Spaces address and MUC domain of the state that took the
  request). The cascade closes the Space to other servers first, in one
  transaction under the Space's lock, and removes the Space's own `owner`,
  `admin`, `moderator` and `member` relations before the node goes (one
  more raw `DeleteTuple` match). Purging a Space's items through PubSub
  first takes the rooms they name out of the Space (parent tuple and
  link), refusing a Space or room of an enrolled scope (two more raw
  `refuse_host_grant_space` matches and one more `refuse_host_grant_room`,
  all in `admin/spaces.rs`). No placement decision changed.
- The permission actor's refusal (`d8-actor-privileged-issuance` moved)
  now covers the channel and Space `owner` and `admin` relations as well as
  the four relations no writer names; each writer keeps its own check.
  Startup attaches the policy through `with_replica_privileged_issuance`
  (`d8-actor-policy` and `startup-schema` moved), which the test-support
  seam calls.
- `s2s_space_subscriptions` (`s2s-space-subscription-record` moved) gains
  a bound per domain in a Space, a Space advisory lock beside the domain's,
  a re-read of the `federated` setting under that lock, removal of rows of
  domains no longer admitted at a bound, and a thirty-day lapse. Turning
  the setting off (`s2s-space-federated-setting` moved) removes the rows in
  that locked transaction and then the PubSub service's records of remote
  subscribers. An owner's removal of a subscription and a ban
  (`d8-space-affiliations` moved) remove the account's row. None of these
  is a grant store; each only reduces what is sent.
- `s2s-space-notify` now runs on the Spaces host's own task, fed by a
  bounded queue from the fan-out (`fanout`-side change in
  `pubsub_fanout/implementation.rs`; `s2s-space-event-egress` unchanged),
  and reads a room's `federated` setting through the room host's own
  decision (`s2s_rooms.rs`; the `s2s-room-*` anchors and
  `s2s-unseated-removal` moved up with the extracted function). The
  Spaces handlers return typed IQs (`placement`, `parent-rollback`,
  `hg-space-pubsub`, `local`, `hg-writer-fence`, `hg-reader-fence`,
  `space-admin`, `d8-space-set-role`, `s2s-space-host`, `s2s-space-open`,
  `s2s-space-honoured`, `s2s-space-room-visible` and `s2s-space-change`
  moved).
- The raw scan holds 1639 records, four more than before, all in
  `admin/spaces.rs` and described above. The other records are unchanged in
  content; those in the changed files moved with the lines.

The selected evidence was refreshed a twenty-seventh time, through
`3302678966ce3557b815755291b08632d382be1a`, after reviewing the delta of the reviewed files since the
twenty-sixth refresh. Four reviewed files changed (`admin/spaces.rs`,
`pubsub_admin.rs`, the Spaces host `s2s_spaces.rs` and the store
`db/s2s_spaces.rs`), no file was added to the reviewed set, fourteen anchors
moved without a change to their text, and no anchor's text changed.

- A Space deleted through PubSub (`pubsub-admin` moved), by a session or by
  a trusted home's owner through `s2s-space-remote-change`, no longer
  destroys rooms: the cascade (`space-delete` moved) takes a mode, and the
  PubSub entry `delete_space` only takes the Space's rooms out of it (parent
  tuple, link and bookmark). `run_delete`, the admin command, still
  destroys rooms, and now only a room whose parent tuple on the Space the
  cascade found and deleted, so an item or a link alone no longer reaches a
  room's destruction. Both run under a Space deletion lock of their own (a
  transaction-scoped advisory lock on PostgreSQL, a process lock on SQLite),
  and the PubSub entry reads the node again under it. After closing the
  Space to other servers the cascade removes the PubSub service's
  subscription records of accounts at other servers. No parent, owner or
  placement decision was added; the change only narrows what the PubSub
  path removes.
- `s2s_space_subscriptions` (`s2s-space-subscription-record` moved) reads
  the subscriber's ban again under the Space's advisory lock, and the
  removal of one subscription (by its account, its owner or a ban,
  `d8-space-affiliations` moved) takes that lock. The sweep of the whole
  store at the deployment's bound is bounded, reads domains from a rotating
  in-process cursor, passes over rows other transactions hold on
  PostgreSQL, and returns the rows it removed, whose PubSub records the
  Spaces host then removes. None of these is a grant store; each only
  reduces what is sent.
- The Spaces host (`s2s-space-host`, `s2s-space-open`, `s2s-space-honoured`,
  `s2s-space-room-visible`, `s2s-space-change`, `s2s-space-notify` and
  `s2s-space-federated-setting` moved) takes a Space's allowance of
  notified publications before a publication is queued, and counts what is
  discarded at shutdown; `space-admin`, `d8-space-set-role` and
  `hg-admin-space-listing` moved with the lines added above them.
- The raw scan still holds 1639 records, each token's count unchanged; the
  records in the changed files moved with the lines.

The selected evidence was refreshed a twenty-eighth time, through
`72e120b689f7e9e9830b1d5453d1c692345a24ab`, after reviewing the delta of the reviewed files since the
twenty-seventh refresh. Four reviewed files changed (`pubsub_dispatch.rs`,
`spaces_retract.rs`, `spaces_remote.rs` and the fan-out
`pubsub_fanout/implementation.rs`), no file was added to the reviewed set,
three anchors moved without a change to their text, and no anchor's text
changed.

- A retract on a Space node (`parent-retract` moved) now notifies the
  node's subscribers once it stands, through a new service-node retract
  fan-out that honours the request's `notify` attribute; the Spaces
  retract takes the request's node, item and `notify` together, from the
  session's dispatch (`hg-space-pubsub` moved) and from a remote
  principal's change (`s2s-space-remote-change` moved). The fan-out's
  enrolled-Space skip, deliverable re-read and hand-over to the Spaces
  host are those a publish takes. No grant, parent or placement decision
  changed; the retract's checks and its removal of the parent tuple and
  link are as before the notification.
- The raw scan still holds 1639 records, each token's count unchanged; the
  records in the changed files moved with the lines.

The selected evidence was refreshed a twenty-ninth time, through
`0ee8556e2eea373a6d7820b525b96f3e2188e540`, after reviewing the delta of the reviewed files since the
twenty-eighth refresh. Three reviewed files changed (`spaces_retract.rs`,
the Spaces host `s2s_spaces.rs` and the store `db/s2s_spaces.rs`), no file
was added to the reviewed set, five anchors moved without a change to their
text, and no anchor's text changed.

- `s2s_space_subscriptions` (`s2s-space-subscription-record` moved) no
  longer reads the subscriber's ban inside its transaction; the Spaces host
  (`s2s-space-host`, `s2s-space-change` and `s2s-space-notify` moved) reads
  it once the row is committed and removes the row for a ban it finds. The
  ban's own removal of the row still takes the Space's lock. This only
  reduces what is sent; no grant decision changed. `parent-retract` moved
  with a comment.
- The raw scan still holds 1639 records, each token's count unchanged.

The thirtieth to thirty-third refreshes were made on the branch of the
remote direct messages and account retirement slice (W6d) before it was
merged, each from the one before it on that branch and the first from the
twenty-fourth; their commits are that branch's. They are numbered here after
the Spaces refreshes above, which were made on the integration branch
meanwhile.

The selected evidence was refreshed a thirtieth time, through
`ec108dd8dd950a97996cdcfb7890e73800d81513`, after reviewing the delta of the reviewed files since the
twenty-fourth refresh. `auth/retirement.rs`, `ingress/principal.rs`,
`ingress_uow/mod.rs`, `server/http.rs` and
`server/routes/websocket/s2s_rooms.rs` changed. No grant writer or storage
plane of the host-grant authority was added, no producer of stream evidence
changed, and no decision of standing moved.

- The remote principal's acceptance (`remote-acceptance-principal` moved)
  now has two arms, chosen by the stream identity's target. The room arm
  (`RemoteIngressTarget::Room`, which the room host `s2s-room-host`,
  `s2s-room-placement` and `s2s-unseated-removal` moved, sets) is
  unchanged. The account arm (`RemoteIngressTarget::Account`) admits a
  remote sender's direct message to a local account, with no room
  execution: it decides the sender's domain from the S2S admission policy
  of the replica that built the submission, and reads the recipient's
  account row (`FOR SHARE` on PostgreSQL) and the ledger of retired JIDs
  in the acceptance transaction (`ingress_uow/remote_recipients.rs`, a new
  read-only repository registered in `ingress_uow/mod.rs`). It honours no
  grant: the message reaches only the recipient account's archive, inbox,
  resources and offline store, and the plan is refused before commit when
  any effect acts elsewhere (`classify_remote_direct_plan`).
- Retirement (`account-retire` moved) is unchanged; its documentation
  names the second step the operator command runs after it commits, which
  `server/http.rs` starts on every serving replica
  (`spawn_account_session_endings`): it ends the retired account's
  streams and detached sessions and refuses its bindings. It reads the
  ledger and writes only its own lease and acknowledgement tables
  (migration 2015); it writes no grant and decides no standing.
- The raw scan holds 1636 records, 3 more than before: the account arm's
  and the account side's uses of `RemoteOccupantPrincipal`. Other records
  in the changed files moved because those files gained lines.

The selected evidence was refreshed a thirty-first time, through
`00c79c85b1e719823720dc11face5244e02933ee`, after reviewing the delta of the reviewed files since the
thirtieth refresh. `auth/retirement.rs` and `ingress/principal.rs`
changed. No grant writer or storage plane of the host-grant authority was
added, no producer of stream evidence changed, and no decision of standing
moved.

- The account arm of the remote principal's acceptance
  (`remote-acceptance-principal`, its anchor unchanged) passes the served
  account domains of the admission policy to the recipient assertion,
  which now refuses a recipient at a domain the policy does not serve and
  finds the account by the lookup keys every existence check reads
  (`auth::directory::account_lookup_keys`: `users.localpart_key`,
  `native_users.jid_key`), locking the row `FOR KEY SHARE` on PostgreSQL.
  The D1 check is described as made against the snapshot captured when
  the submission was built; what it decides is unchanged. The classifier
  of the planned message (`classify_remote_direct_plan`, outside the
  reviewed files) now classifies every intent as well as every effect.
- Retirement (`account-retire` moved) also deletes the account's offline
  store (`pending_delivery`) in its transaction. It writes no grant and
  decides no standing.
- The raw scan holds 1636 records, unchanged in content; records in the
  changed files moved because those files gained lines.

The selected evidence was refreshed a thirty-second time, through
`9d55985c1890dfe12686551cbf273d35c6c915c0`, after reviewing the delta of the reviewed files since the
thirty-first refresh. `auth/retirement.rs`,
`websocket/handlers/message/muc_invite.rs` and
`websocket/handlers/message/group_dm_invite.rs` changed. No grant writer or
storage plane of the host-grant authority was added, no producer of stream
evidence changed, and no decision of standing moved.

- Retirement (`account-retire`, its anchor unchanged) takes the advisory
  lock every insert into the offline store takes for the account's
  canonical JID before it deletes the account's offline store; from its
  commit on the store refuses every row for the account
  (`pending_delivery/database/retired.rs`, outside the reviewed files). It
  writes no grant and decides no standing.
- A mediated or group DM invitation to a retired account, which the
  offline store refuses, fails as one beyond the store's quota does
  (`MucUserDeliveryError::RecipientRetired`): the group DM invitation
  rolls its member grant back through the same path
  (`hg-group-dm-rollback` moved) and answers the inviter
  `service-unavailable`; a mediated invitation rolls back as for any
  delivery failure.
- The raw scan holds 1636 records, unchanged in content; records in the
  changed files, and in `pending_delivery/tests.rs`, whose quota race case
  names the new refusal, moved because those files gained lines.

The selected evidence was refreshed a thirty-third time, through
`42240e113bc1c879fd631e706542fc157e3aedda`, after reviewing the delta of the reviewed files since the
thirty-second refresh. `auth/retirement.rs` changed. No grant writer or
storage plane of the host-grant authority was added, no producer of stream
evidence changed, and no decision of standing moved.

- Retirement (`account-retire`, its anchor unchanged) also deletes the
  account's offline rows kept under another spelling of its JID: it reads
  the store's recipients that start with the account's localpart, compared
  without letter case, and deletes those whose ledger key is the
  account's, under the same advisory lock. It writes no grant and decides
  no standing.
- The raw scan holds 1636 records, unchanged in content and place.

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
