# Proposed connection identity and ownership contract

Status: **draft for #2046 package A**. This is a planning document, not an
accepted ADR or authorization to change runtime behavior. Replica count and
numerical budgets remain owner decisions informed by #2106's local qualification.

Source baseline: `c5ae693ee281cb3e5f52722af11ac9c989f8b8d2` (2026-10-31).
This document records source inspection, not executed runtime evidence. No
connection identity, storage schema, protocol behavior or activation changes here.

## Scope and decision status

The smallest proposed implementation boundary is W1.3: replace the node-local
carbons-pointer ownership token with an explicit identity through the registry,
actor mirror, retained cleanup and local halves of remote-resource registration.
Account selection authority, connection actors, durable endpoint publication and
arbitrary-node resume remain later work. Even this boundary needs approval of its
affected contracts and package A's implementation prerequisites.

The historical v9 plan supplies candidates, not accepted decisions. In particular,
its proposed actor ADR number is obsolete:
[ADR-0020](../../server/docs/adrs/0020-independent-community-federation.md#status)
is the **Proposed** federation pilot. This document reserves no ADR number and
does not accept or supersede either that ADR or the
[accepted stanza scheduling ADR](../adr/008-stanza-handler-wedge-backstop.md).

## Inherited requirements

These are requirements already recorded by the program, not choices made here:

| Source | Requirement carried into this proposal |
| --- | --- |
| [#2046, target authority and contracts 1–2](https://github.com/waddle-social/waddle/issues/2046) | A full JID is a replaceable alias. Keep node incarnation, physical connection, room claim/lifecycle, occupancy and SM identities distinct. Late work must not mutate a successor or silently retarget an admitted effect. Each field has one eventual authority. |
| [#1662, P3.1–P3.3](https://github.com/waddle-social/waddle/issues/1662) | Preserve the `ConnectionRef`, `NodeLeaseIdentity`, `SmBinding` and `UserClaimIdentity` vocabulary. The eventual physical actor is node-local; durable resume uses a new immutable transport generation with fenced compensation. Successful resume preserves logical resource state. These are target contracts, not claims that those types exist. |
| [#2046, contracts 3–4](https://github.com/waddle-social/waddle/issues/2046) and [#1661](https://github.com/waddle-social/waddle/issues/1661) | Admission, custody, commit, SM append, transport attempt/write and client acknowledgement are different milestones. Timeout does not prove rollback. Identity replacement cannot discard already-admitted responsibility. |
| [#1948](https://github.com/waddle-social/waddle/issues/1948) | Remote room-owner acceptance gates the originating handled prefix; it does not establish recipient delivery. This proposal adds no acceptance ledger or new SM completion rule. |
| [ADR-0017](../../server/docs/adrs/0017-horizontal-scaling-remote-actors.md#decision) | PostgreSQL is clustering authority; SQLite remains the single-node backend. The #2046 baseline retains Kameo. Choosing new P2/P3 backend coverage or a transport replacement is outside this identity slice. |

The existing libSQL ADR is already
[Superseded](../../server/docs/adrs/0004-turso-libsql-database.md#replacement-note).
Closing that historical direction does not decide which new P2/P3 features SQLite
must support.

## Current source evidence

All source links and line references below use the baseline above. The identity
names absent from current Rust source include `ConnectionRef`, `NodeLeaseIdentity`,
`SmBinding`, `UserClaimIdentity` and `SmPrevid`.

| Identity or state | Actual current behavior | Limit for W1.3 |
| --- | --- | --- |
| Registry owner token | [`connections.rs:66–100,142–157,207–214`](../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L66-L100) returns the entry's `Arc<AtomicBool>` carbons handle and compares pointer identity. [`WsConnState.registry_owner`](../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L1791) retains it. | The Boolean value is feature state; the allocation identity is the ownership fence. Replacing the token must preserve both behaviors independently. |
| Existing ingress `ConnectionGeneration` | [`WsConnState::new`, lines 1882–1900](../../server/crates/waddle-server/src/server/routes/websocket/state.rs#L1882-L1900) increments a process-local `AtomicU64` starting at 1. | This is an ingress tag, not a persisted transport-generation CAS. It cannot alone distinguish process restarts. A proposed connection sequence must not silently acquire the durable type's semantics. |
| Node incarnation and authority | [`NodeIdentity`, lines 169–215](../../server/crates/waddle-xmpp/src/ownership/claims.rs#L169-L215) includes `node_id`, `node_epoch` and authority in derived equality. `same_incarnation` compares only the first two; `local()` returns fixed `("local", "local")`. | Fixed local identity is not a unique process incarnation. Neither `same_incarnation` nor a stored identity authorizes publication. |
| Publication and retirement | [`SharedNodeIdentity`, lines 296–354](../../server/crates/waddle-xmpp/src/ownership/claims.rs#L296-L354) gates active publication, waits for guarded work before rotation/disable, and leaves the disabled incarnation inspectable. | New work needs current active authority. Cleanup may need to recognize the old incarnation after disable; it must still fence the exact object being retired. |
| Occupancy identity | [`OccupancySessionGeneration`, lines 3–13](../../server/crates/waddle-xmpp-core/src/occupancy_session.rs#L3-L13) survives resume; the restore path assigns `detached.occupancy_session` at [`implementation.rs:1339`](../../server/crates/waddle-server/src/server/routes/websocket/stream_management/implementation.rs#L1339). | A new physical transport must not imply a new logical occupancy during successful resume. |
| SM session identity | [`stream_management/registration.rs:76–78,166–168`](../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L166-L168) carries resumed session state separately; [`registration.rs:368`](../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L368) publishes its stream ID through the owner-gated registry method. | An SM identifier is not a physical transport identifier. Do not use SM equality to authorize a replacement transport's writes. |
| Remote-resource identity | [`types.rs:22–46`](../../server/crates/waddle-server/src/clustering/route_bridge/types.rs#L22-L46) defines registration UUID and socket generation; [`socket.rs:215,326–333`](../../server/crates/waddle-server/src/clustering/route_bridge/registration/socket.rs#L326-L333) additionally keeps a process-local owner token. | These fields are not the future globally exact `ConnectionRef`. Changing local bookkeeping does not migrate the wire representation, durable endpoint authority or remote fence. |

The [merged W1.9 inventory](2046-caller-inventory.md#04-limits) remains the broad
deletion source. Its counts are at `db6e5338`, its slice assignments are proposals,
and its compiler pass covered the production `clustering` feature set only. The
refresh here is a targeted textual/source pass over identity carriers and callers;
it is not a new compiler-resolved whole-program inventory or a current count of
all routing and outbound producers.

## Refreshed identity seams and callers

| Seam | Current callers / retained state to carry through a future token change |
| --- | --- |
| Bind publication and rollback | [`websocket/registration.rs:234–251,604–620,694–745,847`](../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L604-L620): publish the local entry and occupancy, confirm actor registration, retain exact-owner rollback after a timeout or authority loss. |
| Actor registration and removal | [`dual_registration.rs:122–169,189–215`](../../server/crates/waddle-server/src/server/dual_registration.rs#L122-L169), [`user_actor/implementation.rs:58–80,119–144`](../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L58-L80): shared entry/token, conditional registration and unregister; explicit `None` remains an unconditional-removal case requiring caller audit. |
| Deferred unregister repair | [`user_registry.rs:362–425`](../../server/crates/waddle-xmpp/src/registry/user_registry.rs#L362-L425): deduplicate pending removals by full JID **and token identity**, and distinguish pending cleanup of the current resource from older incarnations. This retained work is part of the token migration. |
| Bind retirement inventory | [`binding.rs:97–135`](../../server/crates/waddle-xmpp/src/registry/connection_registry/binding.rs#L97-L135): old actor tokens retire separately from occupancy. A successful same-occupancy resume still retires the old actor token; cancelled registration must retain that cleanup. |
| Presence and carbons | [`presence/regular.rs:70–97`](../../server/crates/waddle-server/src/server/routes/websocket/handlers/presence/regular.rs#L70-L97), [`presence.rs:33–89,139–147`](../../server/crates/waddle-xmpp/src/registry/connection_registry/presence.rs#L33-L89), [`iq/sans_io.rs:170–178`](../../server/crates/waddle-server/src/server/routes/websocket/handlers/iq/sans_io.rs#L170-L178): presence writes are owner-gated; carbons writes update socket state and a full-JID registry setter, then mirror remotely with an owner token. This is not proof that every update is already generation-fenced. |
| SM publication and failed attachment | [`stream_management/implementation.rs:105–131`](../../server/crates/waddle-server/src/server/routes/websocket/stream_management/implementation.rs#L105-L131), [`stream_management/registration.rs:302–323,373–381`](../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L302-L323): owner-gated stream publication and token cleanup remain distinct from logical SM/occupancy restoration. |
| Send reservations and delayed enqueue | [`sending.rs:30–104,230–237,429–484`](../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L30-L104): reservations/captured sends retain owner identity across capacity waits and enqueue. Token replacement alone must not change awaiting-producer overload or custody behavior. |
| Remote registration's local halves | [`socket.rs:474–479,708–715,903–917,1167–1184`](../../server/crates/waddle-server/src/clustering/route_bridge/registration/socket.rs#L474-L479), [`owner_update.rs:196–218`](../../server/crates/waddle-server/src/clustering/route_bridge/registration/owner_update.rs#L196-L218), [`owner_sweep.rs:255`](../../server/crates/waddle-server/src/clustering/route_bridge/registration/owner_sweep.rs#L255): local conditional updates, resync, unregister and mirrored-entry checks also carry the token. Preserve registration/socket-generation checks as separate evidence. |
| Disconnect, terminal and detached cleanup | [`cleanup.rs:902,1510,1836,2622,2771,3865–3878`](../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L3865-L3878): direct token-gated removal and SM-ID-based removal both exist. Detached cleanup obtains the removed entry's token before actor/remote cleanup. Replacing a token does not replace these external readers or prove physical socket termination. |

Reproduce the focused discovery with `rg -n` for `registry_owner`,
`carbons_handle`, `Arc::ptr_eq`, `owner:.*AtomicBool`, `mirror_register_outcome`,
`mirror_unregister` and `unregister_if_sm_stream_id` under
`server/crates/waddle-xmpp/src/registry/`,
`server/crates/waddle-server/src/server/` and
`server/crates/waddle-server/src/clustering/route_bridge/`. Inspect each hit's
enclosing `cfg`/test module: an `Arc::ptr_eq` can identify a lock or lifecycle,
not an owner token. Do not mechanically replace every pointer comparison.

## Candidate identity representations — neither selected

| Candidate | Potential benefit | Questions blocking implementation |
| --- | --- | --- |
| v9 tuple: node ID, node epoch, connection sequence and nonce | Makes node incarnation and connection uniqueness inspectable without persisting a local token. | Specify typed fields, nonce scope/entropy, collision and sequence-exhaustion behavior, and whether a separate process incarnation is needed when clustering is off. Define the minting point, including pre-bind operation, and how a lease rotation affects an already-created reference. |
| Opaque local connection token plus explicit incarnation association | Can separate ownership identity from both carbons state and durable transport generation with a smaller local API. | Prove uniqueness/lifetime and its mapping to the accepted eventual `ConnectionRef`; avoid a second synonymous authority system or assuming the association itself grants publication authority. Define the same restart, rotation and remote-mirror cases as the tuple. |

Both candidates require a reviewed distinction between a physical transport's
identity, a particular published binding, and an account-owner mirror of that
binding. Decide whether these share a value or need separate typed components;
the current mirror's local pointer must not be mistaken for the socket node's
global identity. Decide when the reference is minted, what survives an unsuccessful
bind, and how late completion is fenced if a binding has already been replaced.
Do not remint an old connection into a fresh node epoch merely to make a stale
operation pass. The representation and invalidation mechanism remain open.

## Transition contract for review

The middle column records current behavior at the linked seams. The final column
is the required question/evidence for a future implementation, not a claim that
the candidate already satisfies it.

| Transition | Current behavior / source | Required identity boundary and review case |
| --- | --- | --- |
| Fresh transport → bind | `WsConnState::new` creates the ingress tag and occupancy; registration installs the entry/token before the actor ask. | Define pre-bind identity and exact publication point. Bind success cannot precede the existing required actor confirmation. Pause before/after publication and cancel. |
| Same full JID → replacement bind | Ordinary [`register_entry`](../../server/crates/waddle-xmpp/src/registry/connection_registry/connections.rs#L66-L82) and [`RegisterConnection`](../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L101-L111) replace an existing entry without comparing owner tokens. Local bind gates serialize admission and retain old lifecycle/retirement state; the conditional registration and owner-gated removal variants compare tokens. | Prove an old register, unregister, send reservation or cleanup cannot address the replacement by alias. Race both binds and late actor replies; inspect the actor tree as well as the socket index. |
| Unregister → retry | `mirror_unregister` reports mailbox admission; `UserRegistryActor` retains token-specific retry work. | Admission is not removal completion. Preserve exact-token deduplication and retry ownership, including `None` callers; no timeout may authorize clearing another incarnation. |
| Presence/carbons update | Presence setters check the entry owner; carbons has a full-JID setter and token-bearing remote update. | Inventory check-to-write windows. Decide whether W1.3 only preserves behavior or a separately reviewed slice fixes an uncovered stale update; do not promise atomic account selection from a token substitution. |
| Transport loss → detach → successful resume | SM/occupancy state is restored, then a fresh entry is published; bind cleanup separately retains the old actor token. | New physical transport identity, preserved logical session/occupancy. Do not evict room occupancy while retiring the old token. The future durable transport-generation CAS remains P3.3 work. |
| Failed resume → fresh bind on the same transport | [`reset_resume_attempt`, lines 312–352](../../server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs#L312-L352) remints occupancy, returns to authenticated pre-bind state, resets SM state/fence and clears presence, carbons and interest flags. The next bind starts a fresh logical session on the existing transport. | Decide how physical transport identity relates to the new binding and occupancy. If the physical identity is retained, what distinct binding fence protects the fresh session from late completion or cleanup of the failed resume? Test failed resume followed by fresh bind without reconnecting, with old cleanup released afterward; preserve fresh-session defaults and the new occupancy. |
| Failed/cancelled registration or resume | Local rollback and late actor cleanup use the original token; SM finalization has separate cleanup. | Specify custody and compensation at each intermediate point. An ask timeout can leave a late handler; retry/cleanup must never infer rollback or clear the winner. |
| Lease loss / node epoch rotation | `SharedNodeIdentity` separates active publication from old incarnation recognition; an existing test covers admission revocation before stalled inventory ([`self_fence.rs:2610–2662`](../../server/crates/waddle-server/src/clustering/self_fence.rs#L2610-L2662)). | Test admission and continuation invalidation, then old-incarnation cleanup. `same_incarnation` is insufficient for new work; full `NodeIdentity` equality is insufficient to recognize active→disabled cleanup identity. Do not equate revocation with observed pump termination. |
| Terminal cleanup / SM expiry | Cleanup can remove by exact token or by matching an SM ID, then pass the removed entry's token onward. | Keep external SM-ID readers and their successor checks explicit. The full retirement protocol and replacement control queries remain D2r/DEFprep; W1.3 must not silently delete their custody. |

## Open decisions and dependencies

| Decision | What can be prepared now | Gate retained |
| --- | --- | --- |
| DEC-0 replicas; DEC-4 numerical workload, latency, memory and recovery budgets | Record metrics, denominators and decision criteria with [#2106](https://github.com/waddle-social/waddle/issues/2106). Local qualification is the selected environment; [#2110](https://github.com/waddle-social/waddle/pull/2110) is the initial non-launching safety slice. | Measurements and owner choice. No numeric thresholds or replica recommendation are selected here. |
| DEC-1 new P2/P3 backend coverage | Distinguish inherited PostgreSQL authority from proposed SQLite feature scope. | Owner decision on support policy before affected schema/runtime work. |
| DEC-3 identity representation and ownership | Resolve the candidate and transition questions above through architecture, protocol and operations review. | No unresolved blocking contract decision before W1.3. No allocation of a new ADR number until adoption is actually prepared. |
| Remaining DEC-3 contracts | Prepare typed outcome/retry, request-graph, endpoint lifecycle, lock-order, scheduling, retirement and custody-handoff matrices against #1661/#1662/#1948. | Separate affected designs remain open. This identity proposal cannot authorize B1, endpoint DDL, connection actor extraction or durable resume. |
| Mailbox/lane choices | Reuse [#1986](https://github.com/waddle-social/waddle/issues/1986) and its [five open questions](p2-2-durable-mailbox-core.md#open-questions-for-the-architecture-review). | P2.1 transaction authority and a reviewed lane design; no duplicate acceptance ledger or parallel P2 program. |
| DEC-2 eventual hint transport; DEC-5 supported mixed versions | Inventory surviving asks and compatibility/cutover options. | v9's current-plus-previous policy is not selected by #2046; the owner must resolve supported versions. No runtime swap during this migration. |

The [A2 wiring record](2046-wiring-reconciliation.md#test-lanes) separately records
the merged CI gate. #1662's claim of partial `user_resources` rows and #1664's slice
index still need issue reconciliation. This proposal does not close those tasks,
W1.8a or package A, and does not waive #2046 A4's measured-budget prerequisite.

## Review and verification gates

1. **Architecture:** challenge both candidate representations across local and
   remote mirrors, active/disabled identity, non-clustering restarts, sequence
   exhaustion, retained retry/custody state and same-JID successor races. Resolve
   the exact binding-versus-transport distinction and absence of new dual authority.
2. **Protocol:** review the inherited resume, resource and occupancy contracts
   against the official XEP-0198/XEP-0045 and RFC 6120/6121 sources, with exact
   source revisions recorded before approving any protocol change. Verify that
   identity replacement changes neither stream order nor the SM handled/replay
   frontier and preserves the #1294/#1389 regression seams. No conformance claim
   follows from this document.
3. **Operations:** challenge publication revocation versus cleanup/termination,
   restart/rotation and compatibility boundaries. Confirm how the accepted local
   design can later compose with durable endpoint/SM fences. Keep measured budgets,
   environment isolation, activation and deployment as separate gates.
4. **Implementation acceptance, after those decisions:** use deterministic
   barriers for the transition table. Preserve and extend
   [`characterization_bind.rs`](../../server/crates/waddle-server/tests/characterization_bind.rs),
   [`characterization_resume_muc.rs`](../../server/crates/waddle-server/tests/characterization_resume_muc.rs),
   registry/mirror stale-token tests and self-fence tests. Cover both default and
   clustering builds. Real PostgreSQL/multiprocess evidence is required for claims
   about durable or cross-node fencing; a local token test does not supply it.

The documentation check is limited to source references, links, whitespace and
decision-status consistency. No runtime test or workload is needed to establish
that these files are proposals. Independent review of the final draft remains
outstanding; classify findings as blocking or non-blocking and stop affected
implementation until blockers are resolved.
