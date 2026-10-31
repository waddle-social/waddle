# Production authorization writer and acceptance inventory

Status: source inventory for #2047 F0; **production closure is not established**.
Reviewed source baseline: `db6e5338170d71391b69bc80e57626eb13072365`.
Post-baseline drift was manually reviewed through target main
`a020a79c588a297fe8ff90418f8bc74cbf15b676`; the original baseline and its
limits remain unchanged.
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
SpiceDB backend. [Lifecycle backend selection](../../crates/waddle-server/src/server/lifecycle.rs#L114) chooses it from server
configuration, except for the explicit missing-SpiceDB + enabled fixed-test-account
branch; [that branch](../../crates/waddle-server/src/server/fixed_account.rs#L16) uses the local backend. Lifecycle also asks
[EnsureSchema](../../crates/waddle-server/src/server/lifecycle.rs#L125). The backend [writes the schema](../../crates/waddle-server/src/permissions/spicedb.rs#L32),
[creates](../../crates/waddle-server/src/permissions/spicedb.rs#L64), [deletes](../../crates/waddle-server/src/permissions/spicedb.rs#L165), [replaces a relation family](../../crates/waddle-server/src/permissions/spicedb.rs#L79), and
[conditionally swaps a family for rollback](../../crates/waddle-server/src/permissions/spicedb.rs#L106). Each RPC has its own external
transaction/ACK. These methods return unit/status rather than a durable F0
operation identity or selected dependency witness. Family preconditions protect
that family; they are not a cross-store admission fence. [FullyConsistent
checks](../../crates/waddle-server/src/permissions/spicedb.rs#L40) return boolean-style CheckResponse results without exposing the graph
revision. The [local test backend](../../crates/waddle-server/src/permissions/actor.rs#L113) and [SQL TupleStore](../../crates/waddle-server/src/permissions/tuple/store.rs#L24) are
separate implementations; do not infer SpiceDB behavior from their tests. E1.

**W2 — Startup/account reconciliation and topology (concrete, configuration-dependent).**
[WebSocket-state startup](../../crates/waddle-server/src/server/http.rs#L1057) invokes account reconciliation behind its
lineage gate. [Provisioning](../../crates/waddle-server/src/server/bootstrap_membership.rs#L86) and [reconciliation](../../crates/waddle-server/src/server/bootstrap_membership.rs#L120) write
server owner/member tuples from account identity and configured owner localparts.
The existing-account wrapper logs a reconciliation failure; the batch is not one
all-or-nothing graph/SQL transaction. [Topology seeding](../../crates/waddle-server/src/server/topology.rs#L88) also writes
managed-channel bookmarks and parent tuples and seeds configured owners. This
includes restart/reconcile paths, not merely first account creation. A future
fence needs an explicit contract for their relation to concurrent revocation. E2.

**W3 — Spaces PubSub owner seeds (concrete, intentional override).**
[The owner seed writer](../../crates/waddle-server/src/spaces_pubsub_seed.rs#L59) calls PubSubStorage::set_affiliation. Startup and
node-creation helpers supply configured server owners and/or the creator.
Existing Outcast is explicitly overwritten by Owner. Per-entity failures are
logged and do not abort the batch; node-create seeding is best effort. This is
existing configured-owner policy, not evidence that a future revocation fence
may silently regrant access. E2.

**W4 — XMPP permission facade (latent writer capability).** The
[AppState trait](../../crates/waddle-xmpp/src/app_state.rs#L136) and [server implementation](../../crates/waddle-server/src/server/xmpp_state.rs#L117) expose
[set_room_affiliation](../../crates/waddle-server/src/server/xmpp_permission_state.rs#L160). Its implementation independently deletes
owner/admin/member/outcast and optionally creates the replacement, with separate
actor asks. Errors propagate after any earlier operations; it is not the atomic
family replacement used by the active MUC admin path. No current production call
to this method was found in the two scan roots. That absence is a static-search
finding, not a guarantee for generic/out-of-tree callers. E1 does not exercise
this facade against SpiceDB.

**W5 — MUC and channel administration (concrete).** The active MUC admin handler
[persists managed-channel affiliation](../../crates/waddle-server/src/server/routes/websocket/handlers/iq/muc_admin.rs#L87) through ReplaceExclusiveRelation
before applying the room-actor batch. [Rollback](../../crates/waddle-server/src/server/routes/websocket/handlers/iq/muc_admin.rs#L148) uses expected
family state via SwapExclusiveRelation to avoid overwriting a later writer;
a failed/lost reply can still require reconciliation with actor/durable state.
Administrative channel commands are additional callers. Graph persistence,
actor mutation, durable room projection/effect publication and the IQ result are
distinct observations; a successful single graph write does not establish the
whole command committed. Room configuration, roles, explicit affiliations and
invitations also affect acceptance independently of a channel's inherited read
permission. E3/E6.

**W6 — Group-DM graph and legacy SQL mirror (concrete).**
[Member persistence](../../crates/waddle-server/src/admin/channels.rs#L3872) writes the permission actor and then
[permission_tuples through DbExecute](../../crates/waddle-server/src/admin/channels.rs#L3908). Failure attempts compensation
of the graph write. [Deletion](../../crates/waddle-server/src/admin/channels.rs#L3947) performs the inverse pair and may
restore graph membership if SQL deletion fails. These are separate commits with
possible partial/unknown outcomes, not a PostgreSQL transaction containing the
external graph mutation. Include the compensating paths as writers. Do not treat
this SQL mirror as the complete canonical grant journal. E3 is adjacent recovery
coverage, not an atomic graph/SQL proof.

**W7 — PubSub authorization inputs (concrete).** Generic owner-affiliations IQs
[write affiliation rows](../../crates/waddle-server/src/server/routes/websocket/handlers/iq/pubsub_admin.rs#L495); the storage [upsert/delete](../../crates/waddle-server/src/pubsub/implementation.rs#L409)
has no graph write. [Spaces set-role commands](../../crates/waddle-server/src/admin/spaces.rs#L1428) also change these
rows directly, without updating `space#member` in that command. Do not infer
equivalence between a PubSub role change and a graph membership revoke.
[Effective affiliation](../../crates/waddle-server/src/pubsub_authz.rs#L24) combines stored affiliation
and PEP owner derivation; subscribe, publish, read and admin use distinct access
and publish models. Subscription changes, owner-approved subscriptions and node
configuration affect eligibility too. [Node deletion](../../crates/waddle-server/src/pubsub/node.rs#L108) and recreation
change lifecycle and associated storage. Batch operations can have partial effects
before an error; a row's presence alone is not a permission witness. E4.

**W8 — Parent/bookmark/link changes and compensation (concrete).**
[Spaces publish/move](../../crates/waddle-server/src/server/routes/websocket/handlers/iq/spaces_publish.rs#L15) separately changes the bookmark, SQL
channel-space link and graph parent, then runs [stale cleanup](../../crates/waddle-server/src/server/routes/websocket/handlers/iq/spaces_bookmark_cleanup.rs#L4).
[Publish rollback](../../crates/waddle-server/src/server/routes/websocket/handlers/iq/spaces_publish.rs#L403), [retract](../../crates/waddle-server/src/server/routes/websocket/handlers/iq/spaces_retract.rs#L8) and
[Space deletion](../../crates/waddle-server/src/admin/spaces.rs#L1092) can remove or restore these inputs. Error results
and best-effort rollback do not prove no mutation happened. There is no single
SQL/graph revision covering the whole operation. A link is used as a projection
in one path and as a repair/discovery input in another; this inventory does not
resolve canonical parent authority. E5.

**W9 — Read/join-triggered repair (concrete).** An authorized Spaces items request
can execute [bookmark backfill](../../crates/waddle-server/src/server/routes/websocket/handlers/iq/spaces_items.rs#L100), including parent repair and stale
cleanup/compensation. [Join affiliation resolution](../../crates/waddle-server/src/server/routes/websocket/handlers/presence/muc/access.rs#L11) can invoke
[parent repair](../../crates/waddle-server/src/server/routes/websocket/handlers/presence/muc/access.rs#L227) after a denied channel check. Repair uses bookmarks
and the [channel-space link](../../crates/waddle-server/src/server/routes/websocket/handlers/presence/muc/access.rs#L357), writes graph parents, and rechecks
bookmark validity after writing, deleting stale repairs when necessary. The
post-write recheck matters: a captured read alone does not prove the stale parent
survives. MAM explicitly disables graph repair. E5/E6.

**W10 — Room actor state, invitations and restoration (concrete, separate plane).**
[ChangeAffiliation](../../crates/waddle-xmpp/src/muc/room_actor/implementation.rs#L2962), [ApplyAffiliationChange](../../crates/waddle-xmpp/src/muc/room_actor/admin_handlers.rs#L1115),
[configuration changes](../../crates/waddle-xmpp/src/muc/room_actor/implementation.rs#L2618), [mediated-invite grants](../../crates/waddle-xmpp/src/muc/room_actor/mediated_invites/authorization.rs#L84),
creator ownership and resolver synchronization affect live/durable room policy.
Explicit grants have different provenance from resolver-derived grants. Actor
mailbox serialization, admission revisions and durable room ownership protect
particular actor transitions; they are not a common SpiceDB graph revision.
Restoration/recovery may replay an acknowledged durable transition. A future
writer inventory must include those paths, not just incoming IQ handlers. E3/E6.

## Readers and acceptance sinks

**S1 — Join.** [The join handler](../../crates/waddle-server/src/server/routes/websocket/handlers/presence/muc/implementation.rs#L842) freezes an actor admission revision before
external managed-channel resolution, checks connection ownership before its
final ask and supplies an affiliation grant to [JoinWithAffiliation](../../crates/waddle-xmpp/src/muc/room_actor/occupancy_handlers.rs#L52).
The actor checks ownership/restoration and its member admission revision, then
applies provenance-aware affiliation and room admission rules. CreatorOwner,
Resolver, Unaffiliated and HostOwned are different branches. Open/members-only,
outcast and [deployment-channel overrides](../../crates/waddle-server/src/server/managed_channel_policy.rs#L10) make this policy
richer than a generic `direct OR (parent AND member)` model. Source evidence here
does not establish that every graph writer advances the actor revision. E6.

**S2 — Archive/discovery/extension reads.** The [MUC archive gate](../../crates/waddle-server/src/server/routes/websocket/handlers/presence/muc/access.rs#L83)
uses live room configuration when available, direct relations and managed-entry
rules; unmanaged rooms without a live actor fail closed. It is followed by a
separate [archive query](../../crates/waddle-server/src/server/routes/websocket/handlers/iq/archive_inbox_upload.rs#L184). There is no shared authorization snapshot
or transaction spanning those operations. Extensions have another
[room authorization entry](../../crates/waddle-server/src/server/extension_host_adapter/implementation.rs#L240) and [PubSub publication
authorizer](../../crates/waddle-server/src/server/extension_commands/pubsub.rs#L24), including manifest/deployment policy; these are not aliases for the
MAM sink. Rejection/error, returned archive rows and later network transmission
are different boundaries. E6.

**S3 — PubSub delivery.** The real database [subscriber query](../../crates/waddle-server/src/pubsub/implementation.rs#L376)
filters subscribed rows and outcasts. [Fanout captures that vector](../../crates/waddle-server/src/server/routes/websocket/handlers/pubsub_fanout/implementation.rs#L137), builds
a frozen event, expands resources and attempts the [live queue](../../crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L349) or
[detached SM buffer](../../crates/waddle-xmpp/src/stream_management/session_registry/resources.rs#L371). Private PEP has additional entitlement checks; do not
attribute those checks to generic Spaces fanout. Unsubscription/outcast/config
changes after capture are not uniformly rechecked at each sink. Queue acceptance,
SM custody, later socket write and peer acknowledgement are distinct events.
Best-effort errors/drops are not successful delivery. E4 verifies storage filtering,
not the capture-to-custody race or a no-bytes-after-revoke guarantee.

**S4 — Durable room recipients.** [The membership source](../../crates/waddle-server/src/server/durable_membership.rs#L82) concurrently
reads channel and Space owner/admin/member subjects and excludes channel outcasts.
[Hydration](../../crates/waddle-xmpp/src/muc/room_actor/implementation.rs#L2193) populates an actor mirror; source errors preserve the
session-observed/existing behavior rather than establishing fresh denial. Later
removal handling prunes and refreshes that mirror. These multi-call reads are not
one graph snapshot; the mirror and explicit/live occupants are separate recipient
inputs. E7.

**S5 — Canonical acceptance and replay.** [Ingress claim checks](../../crates/waddle-server/src/ingress/commit_room.rs#L216) concern
exact actor ownership. [Admission coordinates](../../crates/waddle-server/src/ingress/commit_room.rs#L25) fence a new room snapshot
once: later revisions do **not** revoke children of an already accepted manifest.
Replay additionally validates occupancy/lifecycle and current claim ownership.
The [actor-owned replay guard](../../crates/waddle-server/src/ingress/room_manifest_replay.rs#L89) keeps a short outer claim transaction
through inner acceptance so caller timeout cannot release ownership prematurely;
the inner sink owns canonical rows. It is not a single F0 authorization-journal
transaction. Existing [lock order](../../crates/waddle-server/src/ingress_uow/mod.rs#L3) is epoch → exact claim → fenced
child rows. On the immediate, non-planning fallback, the existing
[pre-fanout ownership check](../../crates/waddle-server/src/server/routes/interpret/room_dispatch.rs#L131)
now retries within a fixed budget and
[bounces on backend uncertainty](../../crates/waddle-server/src/server/routes/interpret/room_dispatch.rs#L471)
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
| E2 | [seed_owners_on_node_overrides_existing_outcast](../../crates/waddle-server/src/spaces_pubsub_seed.rs#L164) | The intentional seed override; in-memory PubSub test, no concurrent restart/revoke proof. |
| E3 | [rollback_keeps_affiliation_committed_by_a_later_owner](../../crates/waddle-server/src/server/routes/websocket/handlers/iq/muc_admin.rs#L3284) | Conditional rollback behavior in the test fixture; not cross-store atomicity or F0 writer closure. |
| E4 | [database_deliverable_subscribers_excludes_outcast](../../crates/waddle-server/src/pubsub/tests.rs#L997) | Storage filtering; no post-capture fanout guarantee. |
| E5 | [items authorization before backfill](../../crates/waddle-server/src/server/routes/websocket/tests/iq.rs#L3190), [repair only projected parent](../../crates/waddle-server/src/server/routes/websocket/tests/muc.rs#L4867) | Selected repair gates/selection; not all mutation interleavings or authority choice. |
| E6 | [members-only join requires explicit affiliation](../../crates/waddle-server/src/server/routes/websocket/tests/muc.rs#L1145), [MAM denies non-member](../../crates/waddle-server/src/server/routes/websocket/tests/iq.rs#L5378) | Current action-specific entry semantics; test state uses local fixtures, not an inherited graph fence. |
| E7 | [durable channel/Space users excluding outcasts](../../crates/waddle-server/src/server/durable_membership_tests.rs#L101) | Membership-source derivation; local permission actor, not a common snapshot. |
| E8 | [stale admission but preserved accepted children](../../crates/waddle-server/src/ingress/xep0045_room_manifest_tests.rs#L486) | The current accepted-manifest contract; not a new revocation policy. |
| E9 | [claim update blocked through commit](../../crates/waddle-server/src/ingress_uow/tests.rs#L543) | Claim lock lifetime when actually executed against PostgreSQL. [The fixture call](../../crates/waddle-server/src/ingress_uow/tests.rs#L1631) returns `None` without `WADDLE_TEST_POSTGRES_URL` in optional local mode, but panics when `WADDLE_TEST_REQUIRE_POSTGRES=1`; source presence or aggregate success outside required mode is not native execution evidence. |

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
