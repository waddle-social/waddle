# Current connection inventory and wiring reconciliation

Snapshot for [#2046](https://github.com/waddle-social/waddle/issues/2046),
2026-11-08, against `main@b2597ee60bbcf9cff2824d7306a66b882bf9b118`. This is a
bounded current-source delta to the [type-resolved W1.9 caller inventory](2046-caller-inventory.md)
at `db6e5338` and the [A2 wiring record](2046-wiring-reconciliation.md). Those
records remain historical; their counts, line numbers and rollout statements
must not be treated as current. No Cargo command, runtime test, deployment or
live fleet inspection was performed for this snapshot.

## Method and limits

The original W1.9 pass used compiler-resolved deprecation markers and found
1,502 distinct use sites on its own baseline. That result cannot be advanced to
this revision by moving line numbers: 188 files changed in the selected
WebSocket, clustering, interpreter and registry paths since `db6e5338`.

The [candidate index](2046-current-caller-candidates.md) records current field
declarations, classes and sample textual access locations, method declarations
and sample calls, and named mirror, room and relay candidates. Reproduce it
from the repository root with `python3 docs/planning/2046-current-caller-scan.py`.
Use `--all --output /private/tmp/2046-all-candidates.md` for all matching links;
links are relative to the chosen output directory.
The scanner uses `rg --files` for Rust sources and an [explicit classifier](connection_caller_regions.py)
adapted from the historical `testregions.py`, strips comments and strings, extracts the three
structs and inherent methods, and searches member names and named families. It
fails if the struct fields differ from its reviewed classification list.
It excludes obvious test and `test-support` regions but does **not** evaluate
all feature combinations, including all `test-utils` gates. It cannot resolve
receiver types, aliases, macros, trait calls, struct construction, destructured
borrows, or whether a method mutates a field. Same-named members on unrelated
types are intentionally retained. Its results are candidate locations, not a
production call graph or a read/write count.

| Scope | Declared fields | Field candidate lines | Inherent method declarations | Method candidate lines |
| --- | ---: | ---: | ---: | ---: |
| `ConnectionRegistry` | 7 | 104 | 85 | 442 |
| `ConnectionEntry` | 22 | 485 | 45 | 310 |
| `WsConnState` | 46 | 523 | not scanned | not scanned |

The field totals exclude the registry's test probe and three explicitly
`cfg(test)` socket fields. Method counts include private inherent methods and
are lexical counts after the stated filters, not a substitute for the old
compiler-resolved counts. One source line may appear in several rows. Named
scans found 17 dual-registration candidates in seven files, including the two
definitions; 199 user/resource-mirror candidates in 43 files; 16 room-dispatch
candidates in seven files; and 140 remote-resource-relay candidates in 22 files.
These are search results, not verified numbers of production calls.

Class labels follow W1.9: **A** is current authority for the named decision;
**I** is an index or copy; **T** is transport/lifecycle plumbing; **O** is
duplicate registration or migration glue intended for replacement. A combined
label means the field or family has more than one role; it does not assert a
single future owner. In particular, class O does not make a currently required
mirror safe to delete.

## Changed authority and caller seams

| Seam | Current source evidence and classification | Delta / implication |
| --- | --- | --- |
| Registry and entry state | The [registry fields](../../server/crates/waddle-xmpp/src/registry/connection_registry.rs#L63) combine the full-JID map and SM reverse index (I/T), pending subscriptions and presence/activity (A), and binding slots (A/T). The [entry](../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L303) holds binding and physical identities (A), endpoint/control and relinquishment (T), and shared presence/interest values used by selection (A/I). | The candidate index gives every identified field a class. It does not prove all readers or writers, including alias and borrow paths. |
| Exact local identity | [`LocalConnectionBindingId`](../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L287), [`ConnectionRef` and `BindingRef`](../../server/crates/waddle-xmpp/src/ownership/connection_identity.rs#L66) are distinct identities. The active bind path [constructs a binding-backed entry and publishes the owner](../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L640). | The old description of `registry_owner` as a carbons `Arc` is stale. The [socket field](../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L1838) holds the local binding ID (A), not a durable node/SM authority. |
| Binding permits and provisional endpoint | Registry [provisional publication/activation/retirement](../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L15) and [local permits](../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L409) are A/T. The [batch writer](../../server/crates/waddle-server/src/server/routes/websocket/batch_write.rs#L330) uses the exact permit for the tagged SM-enable response. | This is a narrow live seam plus dark provisional machinery, not a general output or P3 actor cutover. The [#2281 handback record](2046-failed-resume-reservation-handoff.md) limits its claim to an empty receiver with a held reservation. |
| Socket owner and recovery | The live `/ws` path [creates the endpoint and `WsConnState`](../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L454). The [state declaration](../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L1756) now has 46 non-test fields, versus 40 in the old snapshot. New fields include physical/binding identity, I/O relinquishment, stream-open hint and failed-resume handback state. | The socket task remains the live owner (A/T). The [actor core and driver modules](../../server/crates/waddle-server/src/server/routes/websocket/mod.rs#L79) are compiled but explicitly unselected; their fixture is not `/ws`. Shared handles and destructured handler borrows still require type-resolved analysis before assigning every write to one future actor. |
| Dual registration | [Registration](../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L740) waits for actor confirmation, and [rollback](../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L238) removes only the captured local owner. `mirror_register_outcome` and `mirror_unregister` remain O glue with current acceptance/teardown duties; the [module](../../server/crates/waddle-server/src/server/dual_registration.rs#L1) distinguishes the bounded ask from enqueue-only unregister. | The named scan finds 15 lexical call lines plus two definitions, across bind, resume/cleanup, SM janitor and bridge paths. Actor selection is authoritative today; removing this glue before a replacement is unsafe. |
| User/resource mirrors | The [UserActor](../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L22) selects among full-JID entries (A/I); the [UserRegistry entry](../../server/crates/waddle-xmpp/src/registry/user_registry.rs#L70) retains resource clones for hard demotion (I). The remote owner [constructs and copies a remote-hosted entry](../../server/crates/waddle-server/src/clustering/route_bridge/registration/owner.rs#L370), [publishes it](../../server/crates/waddle-server/src/clustering/route_bridge/registration/owner.rs#L442), and [starts a forwarder](../../server/crates/waddle-server/src/clustering/route_bridge/registration/owner.rs#L511). | Remote entry/forwarder and mirrored registration are O/T; local selection is A today. A `ConnectionRef` is now copied by the current registration wire, but that does not turn the mirror into the target P3.2 resource authority. |
| Room dispatch | Both production entry points include the [normal interpreter](../../server/crates/waddle-server/src/server/routes/interpret/implementation.rs#L309) and [relay planning](../../server/crates/waddle-server/src/server/routes/interpret/relay_plan.rs#L28). The [typed room route](../../server/crates/waddle-server/src/server/routes/interpret/room_routing.rs#L31) separates planning from immediate bridge execution (A/T). [Dispatch](../../server/crates/waddle-server/src/server/routes/interpret/room_dispatch.rs#L209) retains the local room actor path; its [bounded fanout check](../../server/crates/waddle-server/src/server/routes/interpret/room_dispatch.rs#L442) fails closed on backend uncertainty. | This supersedes the old direct-branch and fail-open description. It is a routing facade, not durable remote acceptance or actor migration. |

### Registered remote-resource endpoints

The following are registered handler IDs on this source tree when the
clustering build and runtime are selected; no remote exchange was performed.
The [handler declarations](../../server/crates/waddle-server/src/clustering/relay/implementation.rs#L485)
and [compatibility handlers](../../server/crates/waddle-server/src/clustering/relay/remote_resource_compat.rs#L155)
are the source of the versions below. O denotes mirror maintenance and T an
active transport/control path.

| Message suffix | Direction and role | Class |
| --- | --- | --- |
| `remote_resource_register.v4`, `remote_resource_unregister.v2`, `remote_resource_update.v1` | Socket node to account owner; register, remove and update mirrored resource. | O/T |
| `remote_user_side_effect.v4` | Socket to account owner; user-side fanout. | T |
| `remote_resource_write_accepted.v1`, `remote_resource_force_detach.v3` | Owner/requester to socket; output acceptance and force-detach control. | T |
| `remote_resource_route.v9` / `live_resource_route.v3` | Socket to owner; route a stanza. | T/O |
| `remote_resource_frame.v3` / `live_resource_frame.v2` | Owner to socket; frame delivery. | T/O |
| `resource_presence.v1` | Remote presence lookup. | I/T |
| `deliver_ordered.v16` | Ordered relay for existing inter-node paths. | T |

The compatibility helper's fallback on `UnknownMessage` is conditional
([source](../../server/crates/waddle-server/src/clustering/relay/remote_resource_compat.rs#L39)).
Identity-bearing registration v5, update v2 and unregister v3 are
[reserved DTOs without registered handlers](../../server/crates/waddle-server/src/clustering/relay/resource_identity_wire.rs#L1),
not the active versions. The older A2 v13/v3/v8 values are stale.

## Current wiring snapshot

Here **compiled** means included by the identified source feature/build
expression, not a build executed here. **Wired** means a non-test caller exists
under the stated gate. **Configured** means committed chart/GitOps intent, not
observed fleet state. **Test-defined** names source coverage, not a passing run.
All rows have **live-verified: unknown**.

| Boundary | Compiled | Wired | Configured / enabled | Test-defined; execution in this snapshot |
| --- | --- | --- | --- | --- |
| Clustering and legacy relay | [`clustering`](../../server/crates/waddle-server/Cargo.toml#L168) is optional; the [image expression](../../flake.nix#L392) selects it. | Node startup builds [claim, resume and delivery bridges](../../server/crates/waddle-server/src/clustering/node.rs#L961). | Chart [default off](../../server/charts/waddle-server/values.yaml#L1); committed [HelmRelease has two replicas and clustering on](../../infrastructure/waddle.cloud/gitops/waddle-server/helmrelease.yaml#L30). | [Ordered relay](../../server/crates/waddle-server/tests/clustering_ordered_relay.rs) and [cluster E2E](../../server/crates/waddle-server/tests/clustering_cluster_e2e.rs) sources exist; not run. |
| #1661 / #2116 ingress authority | Ingress and main/ingress pool lanes are compiled. | [`create_ingress_authority`](../../server/crates/waddle-server/src/server/http.rs#L889) selects among attested, shared-owner and legacy constructors. | The [restricted startup path refuses unconverted stores](../../server/crates/waddle-server/src/server/lifecycle.rs#L90); [operator preparation](../../server/crates/waddle-server/src/db/runtime_startup.rs#L170) is not serving authorization. | [Physical attestation](../../server/crates/waddle-server/tests/physical_attestation.rs) includes fixture groups; not run. One owner-shared pool is not the completed restricted role/epoch/cutoff contract. |
| #1986 P2.2 durable mailbox and #2118 acceptance / reconciliation | Planning and other existing ingress journals exist. Searches for `MailboxLane`, `mailbox_entries`, `durable_mailbox`, `AcceptanceUow` and `TargetConsumptionUow` under `server/crates` found no matching identifiers. | No target mailbox selector identified by that search. | No target lane activation established. | No target lane test claimed. Identifier absence alone does not prove absence of every differently named partial implementation. |
| #1948 remote-room acceptance | Existing relay and frozen-manifest code is compiled. | Local manifests can be enabled; [clustered new manifests](../../server/crates/waddle-server/src/ingress/commit_room.rs#L11) call a [non-test `false` gate](../../server/crates/waddle-server/src/ingress/commit/commit_hooks.rs#L23). | No completed remote-owner acceptance-before-origin-`h` gate established. Existing relay work remains active under clustering. | Room, relay and ingress tests exist; not run and do not prove #1948 complete. |
| #1662 P3.1 connection actor | [Core and driver](../../server/crates/waddle-server/src/server/routes/websocket/mod.rs#L79) and [local endpoint candidate](../../server/crates/waddle-server/src/server/routes/websocket/actor_local_endpoint.rs#L1) compile. | No production `/ws` selector for the actor; the socket loop remains wired. | Unselected. | Actor and [live WebSocket admission](../../server/crates/waddle-server/tests/websocket_admission_live.rs) tests exist; not run. |
| #1662 P3.2 identity/resource authority | [`NodeLeaseIdentity`, `ConnectionRef`, `BindingRef`](../../server/crates/waddle-xmpp/src/ownership/connection_identity.rs#L17), [`UserClaimIdentity`](../../server/crates/waddle-xmpp/src/ownership/user_claim_identity.rs#L27) and [`SmBinding`](../../server/crates/waddle-xmpp/src/ownership/joint_identity.rs#L78) exist. | Exact local identities participate in live binding; the target durable account-resource transition remains a [dark model](../../server/crates/waddle-xmpp/src/ownership/account_resource_transition.rs). | The reserved identity-bearing resource DTOs are not registered endpoints. | [Identity-wire tests](../../server/crates/waddle-server/src/clustering/relay/resource_identity_wire/tests.rs) and model tests exist; not run. This corrects the old claim that the identity vocabulary did not exist. |
| #1662 P3.3 SM / resume | Existing fenced SM persistence is compiled with clustering; [the full-payload candidate](../../server/crates/waddle-server/src/lib.rs#L49) requires both `test-support` and `clustering`. | Existing SM resume is wired; candidate custody has no production selector. | Existing SM is advertised; target pre-accept custody and joint endpoint/resource/SM activation remain open. | [Cross-node resume](../../server/crates/waddle-server/tests/xep0198_cross_node_resume.rs) and [custody laboratory](../../server/crates/waddle-server/tests/physical_attestation/preaccept_handoff.rs) tests exist; not run. |
| #1662 P3.4 SASL2 | SASL2 parser/handler groundwork exists. | [Dispatch checks](../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L957) a [fixture-only opt-in](../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L1421). | Non-`test-support` builds return false for the switch; inline resume and production SASL2 advertisement remain gated. | [SASL2 WebSocket fixture](../../server/crates/waddle-server/tests/sasl2_dark_ws.rs) exists; not run. This supersedes the old claim that no SASL2 groundwork existed. |
| Rollout and test graph | [Chart defaults](../../server/charts/waddle-server/values.yaml#L41) and [committed HelmRelease](../../infrastructure/waddle.cloud/gitops/waddle-server/helmrelease.yaml#L40) both specify `Recreate`; the latter enables the cutover guard. | CUE retains [test and publish dependencies](../../server/env.cue#L798); [flake](../../flake.nix#L146) retains the all-features test archive. | `.github/workflows` is absent at this base; the [CI pause record](2046-ci-pause.md) records Actions being disabled and the restoration sequence. | [Required Postgres test names](../../server/.config/required-cluster-tests.txt) and the [required-Postgres helper](../../server/crates/waddle-ws-test-support/src/test_postgres.rs#L20) exist. No hosted lane, CI status or live rollout was checked. The historical L-pr workflow links are not current wiring. |

## What this evidence closes

The source snapshot corrects the historical wiring record and supports a
bounded A2 reconciliation. It does not close #2046's **every caller** inventory:
the textual index needs a fresh type-resolved production-feature pass, manual
classification of reads, writes, destructured borrows and indirect calls, and
independent review. It also does not establish that every feature/config cell
or a live deployment has been examined, so the full A2 checklist remains open.
All P2/P3 authority, old-writer cutoff, SM custody, pending transfer, CI,
qualification and activation tasks remain separate from this documentation.
