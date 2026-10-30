# Candidate host-grant authority and admission boundary (#2047)

## Status and decision requested

**Proposed design for review; implementation and activation are blocked.** This
document recommends PostgreSQL as the single canonical host-grant authority and
SpiceDB as its reconstructible policy projection. It does not approve that
selection, satisfy F0/F3, authorize implementation, or change runtime behavior.
[ADR-0020](../adrs/0020-independent-community-federation.md) remains Proposed.
The [federation plan](../../../docs/planning/federation-2047.md) and distributed
actor roadmap retain their ordering and activation gates.

Review whether this candidate has an implementable safety boundary and
adequately identifies work preceding its first production integration.
Accepting this document must not turn unresolved gates into implied approval
to implement affected behavior.

## Evidence and assumptions

Production sources were inspected at main
`14fa2533efb4d7377af18bc61437e5843d072972`. The
[writer/sink inventory in #2062](https://github.com/waddle-social/waddle/blob/80a31f06a19b4eaf93469b57fce911d94bbee221/server/docs/planning/production-authority-inventory.md)
records selected boundaries at its stated earlier baseline. Its lexical checker
is not a call graph or proof that all writers were found. No runtime tests were
executed for this document. Related PR evidence was read, not rerun:

| Evidence | Relevant result and limit |
| --- | --- |
| [#2050](https://github.com/waddle-social/waddle/pull/2050), head `ecaed6a342298afef7dd3c84c6d064840a91fa14` | Real PostgreSQL/SpiceDB leaf admission schedules; no production writer closure. |
| [#2051](https://github.com/waddle-social/waddle/pull/2051), head `aaaca3a3603e1e92980c5f8dc1f01524b2af0d92` | Reconstructible PostgreSQL intent versus SpiceDB marker/tombstone authority; no production selection. |
| [#2057](https://github.com/waddle-social/waddle/pull/2057), head `09f99df8689489675a6857e61e898bb02a7824a2` | Actual lost graph-write response versus never-forwarded request; no cross-store atomicity. |
| [#2052](https://github.com/waddle-social/waddle/pull/2052), head `255eae4f98423c9f06d164cff87e490dba958b3b` | Existing accepted room children and persisted PostgreSQL recovery; no clustered grant fence. |
| [#2053](https://github.com/waddle-social/waddle/pull/2053), head `6d15068a423a5f823c48a9a9892a63a62dae9a06` | Reversible identifier bytes; canonical principal normalization/migration unresolved. |
| [#2056](https://github.com/waddle-social/waddle/pull/2056), head `bcde1f018dd478eb61fe23ffa5acffafd194d19c` | Captured Space fanout reaches the local queue after Outcast commits; unsafe characterization. |
| [#2059](https://github.com/waddle-social/waddle/pull/2059), head `ffffccc01b1455be16e11cf274895eddb3abbb38` | Supervised native fixture; no authority selection. |
| [#2060](https://github.com/waddle-social/waddle/pull/2060), head `7c32a41743e5b9afed05cc8292c263e9814d1bb2` | Production parent-move overlap; separate consistent reads, not one admission snapshot. |
| [#2061](https://github.com/waddle-social/waddle/pull/2061), head `4579250976dd5cd3b9c4fea0d3d43c3af930373a` | Finite synthetic positive-branch fences; not complete production policy or graph reconstruction. |

Assume a fresh isolated pilot authority namespace can be provisioned; do not
assume existing stores are empty. Each deployment owns its PostgreSQL, SpiceDB,
credentials and administration. PostgreSQL continues to own session lifecycle,
exact claims and contiguous frontiers. No shared federation control plane,
HTTP membership protocol, arbitrary-node resume, new XMPP namespace, feature
advertisement or S2S activation is proposed.

The failure model includes concurrent replicas, stale work, cancellation,
process death, dependency outage, lost replies, old binaries and restoration.
Database/SpiceDB administrators are trusted operators, but their mutation and
restore paths must be fenced operationally. An unfenced administrator changing
an active projection invalidates the argument; a previously verified marker
does not cover it. Credential separation and restore exclusion remain blockers.

## Candidate canonical model

Use a qualified community key `(canonical service JID, opaque Space node)` and
separate lifecycle identity. Begin with one coarse admission generation per
community. Any policy mutation invalidates that community's earlier witnesses,
even if only one member changes. This availability cost avoids treating a
positive relation as the complete production policy.

Canonical PostgreSQL state includes complete reconstructible grant/topology
intent, typed subject/resource identities, grant provenance, revoke tombstones,
policy/schema version, authority incarnation, monotonic generation, operation
ID, immutable operation fingerprint and `closed`/`active` admission state.
A digest alone is not reconstructible intent. Keep immutable operation outcomes
separate from the latest active state. Exhaustion fails closed; generations and
lifecycle identities are never reused.

This replaces grant authority for an enrolled scope; it is not another
independently writable grants store. Existing `permission_tuples` is a partial
mirror and cannot be promoted by assumption. PubSub subscriptions/affiliations,
explicit room affiliations and community roles retain distinct semantics.
Policy-relevant changes enter the canonical boundary; protocol projections
cannot independently regrant authority.

Every action needs explicit policy. The production
[graph schema](../../crates/waddle-server/src/permissions/schema.rs) does not
subtract `channel#outcast` from `read`, while
[MUC admission](../../crates/waddle-server/src/server/routes/websocket/handlers/presence/muc/access.rs)
gives outcast precedence and separately handles members-only/deployment rules.
Witnesses bind negative overrides, configuration, lifecycle and action as well
as positive grants. Community ownership does not imply deployment authority;
Space subscription does not grant child membership. Complete policy is B2.

A deployment-wide policy input cannot change outside these witnesses. Either
its mutation closes every affected community before acknowledgement, or each
dependent admission also locks a versioned deployment-policy guard. Choosing
and auditing that dependency is part of B2; a community generation alone cannot
fence a changed deployment override, schema or other global authorization input.

## Mutation, acknowledgement and projection

A command carries stable internal operation identity, immutable typed input,
authenticated initiator/provenance and expected scope generation. An exact retry
returns its recorded result; identity reuse with changed input fails. Mapping
external XMPP request identity and results/retries to this internal operation
requires separate review before handler integration. An IQ ID alone is not a
deployment-wide durable idempotency key.

The proposed mutation transaction acquires lineage/epoch protection, applicable
exact ownership claims, then sorted affected authority guards, before affected
policy/child rows. It revalidates the initiator under the same locked policy;
an earlier admin check cannot survive that admin's revocation. It records the
complete new intent/tombstone, advances generation, closes admission and records
projection/control work, then commits.

**That commit is the proposed revocation linearization and success boundary.**
It means no new protected admission can use the revoked generation, provided
every applicable sink participates. It does not mean actor ejection, graph
projection or network draining has completed. Ejection and cache/projection
updates remain durable work; new protected use must reject at the canonical
fence while pending. A protocol operation promising more cannot reuse this ACK
definition without review.

A grant is recorded while closed and becomes effective only after verified
projection and exact-operation activation. Do not report it effective earlier.
Canonical commit, projection verification, activation and delivery are separate
facts. A timeout is unknown, not rollback or permission to compensate. Resolve
uncertain commits by exact operation/payload readback from the authoritative
primary; unavailable/contradictory evidence stays closed. Historical success
may be returned for a subsequently superseded operation, but cannot be described
as today's active policy.

SpiceDB mutation occurs outside the PostgreSQL transaction. A bounded relation
change and its operation marker must share one atomic `WriteRelationships`
request, conditioned on the exact previous marker. The marker binds authority
incarnation, scope lifecycle, generation, operation and intended projection.
The API must reject movement from a newer observed generation to an older one.
A failed CAS does not let a stale worker refresh the precondition and replay old
intent. It stops; reconciliation reloads current canonical authority.

Read marker, relevant relationships and permission at one anchored graph
revision. The current boolean
[SpiceDB check API](../../crates/waddle-server/src/permissions/spicedb.rs)
does not expose that witness. Missing/conflicting markers, newer-than-canonical
graph state, malformed shapes and schema mismatch fail closed. Grant absence
does not establish a committed revoke. Resolve a lost graph-write reply with
exact marker/content readback, not timeout or unconditional retry.

Recovery reconstructs only current canonical intent. Activation compares exact
incarnation/lifecycle/generation/operation/policy version and stays closed on
change. Repair is a versioned operation, not an in-place active overwrite.
Startup compares expected schema and fails on mismatch instead of replacing it;
schema changes require a fenced transition. Missing-marker initialization is
allowed only in a proven fresh namespace, never automatic restored-state repair.

A parent move atomically closes both communities and the channel lifecycle,
records one canonical binding and coordinates both projections. Reopening
verifies the same move operation on both sides. Bookmarks cannot choose parent
authority. Bounded atomic writes may suffice for the first relation family;
larger graphs, chunking, multi-scope reconstruction and exact comparison rules
need proof before their implementation (B3).

## Admission, custody and lock lifetime

Prepare a witness outside the acceptance transaction: canonical subject, exact
action/resource, authority/scope lifecycle, policy/schema generation, anchored
graph result and relevant room state. A cached boolean is insufficient.

For a fresh room manifest, revalidate and retain authority locks inside the
same PostgreSQL transaction that accepts the canonical message and seals frozen
children. Integration points are
[commit_transaction](../../crates/waddle-server/src/ingress/commit/implementation.rs)
and [commit_room](../../crates/waddle-server/src/ingress/commit_room.rs).
Revocation waits for earlier acceptance, or wins and admission rejects its stale
or closed witness. No graph RPC, actor ask, queue-capacity wait, socket write or
client ACK belongs inside those locks.

The owner must survive caller cancellation until transaction resolution.
Uncertain acceptance may execute no new external effect until exact canonical
message/child readback proves acceptance. A local task/token cannot prove a
PostgreSQL transaction survived a partition.

The proposed lock insertion is an **audit requirement**, not an ordering proof.
Existing [ingress rules](../../crates/waddle-server/src/ingress_uow/mod.rs) order
epoch, exact claim, then fenced children; some commit paths already lock stream
or canonical identity before later room checks. Enumerate actual acquisition
orders across admin, restore, replay and SM transactions. If a new guard follows
a child another operation acquires after the guard, refactor or reject the
integration. Never call an inner claim-taking transaction while retaining outer
authority locks. B4 blocks implementation until resolved for the chosen slice.

Accepted children retain the
[ingress acceptance contract](../operations/ingress-effect-acceptance.md).
`commit_room` treats coordinates as a one-time acceptance fence;
`commit_transaction` preserves prior acceptance instead of replacing it with a
later rejection. [Replay](../../crates/waddle-server/src/ingress/room_manifest_replay.rs)
still checks frozen payload/target, occupancy/lifecycle and current claim.
Later revocation does not erase accepted children. They may complete after the
revoke ACK as preaccepted work, without new recipients or today's substituted
payload. No-store and unknown-started-send policy remain unchanged. Altering
this contract requires a separately reviewed behavioral slice.

| Surface | Required boundary; unresolved integration |
| --- | --- |
| Join/room admin | Authoritative durable transition with policy/claim fence; mailbox ACK or in-memory affiliation is insufficient. |
| MAM/disco/protected reads | Bounded response/page acceptance preserving action policy; no separate check/query/send race or unbounded stream holding locks. |
| Fresh room fanout/archive | Canonical manifest acceptance with required sender/recipient inputs; execution consumes frozen children. |
| PubSub live/SM fanout | Separate admission/handoff contract; subscriber capture is not acceptance. |
| Remote handoff | Durable custody, retry identity and recipient binding; queue/socket/remote acceptance is not recipient delivery. Existing actor/durable roadmap and F2 still apply. |

### DirectFrame counterexample

Current [PubSub fanout](../../crates/waddle-server/src/server/routes/websocket/handlers/pubsub_fanout/implementation.rs)
captures subscribers, builds an event, and calls `try_send_to`, falling back to
[detached SM append](../../crates/waddle-xmpp/src/stream_management/session_registry/resources.rs).
Its request contains node/item/publisher inputs, not canonical publication/child
acceptance identity. #2056 demonstrates a stale capture reaching the queue.

A lock around `try_send_to` is insufficient: replica A checks while holding a
shared PostgreSQL guard; its backend dies or loses authority, releasing the
server-side lock; replica B commits/ACKs revoke; A enqueues using its earlier
read. A Rust transaction object does not prevent this ordering.

A durable pre-queue record creates a new contract for identity, payload/target
binding, lost commits, retries, expiry, storage hints and dropped notifications.
It is not existing delivery custody by assertion. Detached SM could instead
check authorization in its actual persistence transaction, or consume an already
accepted canonical obligation. Neither is implemented here. Do not introduce a
replayable headline/offline queue or change no-store semantics under the label
of fencing. PubSub remains blocked (B5).

## Writer exclusion and policy-input inventory

This covers all categories in inspected #2062 plus external/schema/restore
writers. It is a **closure checklist, not an exhaustive call-graph claim**.
Each needs exact callers, backend, principal, mutation/ACK and negative tests
before enrollment.

| Boundary | Required treatment for enrolled scopes |
| --- | --- |
| Permission actor/backend writes, exclusive replace/swap, schema | Canonical typed operations; reject legacy mutation and alternate-backend fallback. |
| Account bootstrap/reconciliation and topology startup | Explicit operation-keyed provisioning; restart/config/localpart cannot regrant a revoked subject. |
| Spaces owner seeds | Replace Outcast-to-Owner reseeding with scoped canonical provisioning; PubSub role is not independent authority. |
| Legacy XMPP `set_room_affiliation` facade | Reject enrolled scopes until migrated; latent capability is not excluded by absent discovered callers. |
| MUC/channel admin, role/config changes and rollback | Exact expected generation; actor failure cannot trigger an older unconditional grant. |
| Group-DM graph/SQL mirror and compensation | Partial mirror is not canonical; compensation is a newly authorized/versioned operation. |
| PubSub owner-affiliation IQ, Spaces set-role, subscribe/unsubscribe/approval, access/publish config | Distinct inputs enter the scope boundary; subscriptions never become community grants. |
| Space/room/node deletion and recreation | Retire lifecycle and invalidate old witnesses/grants/markers. |
| Parent publish/move/retract/delete, bookmark/link writes and rollback | Canonical topology determines projections; both move scopes participate. |
| Read-side bookmark backfill, join parent repair, extension membership | Repair only canonical state; captured bookmarks/graph are not authority. Preserve MAM's no-repair rule. |
| Actor explicit/resolver/invite/creator affiliation, restoration and member hydration | Preserve provenance; stale/failure-retained mirrors cannot authorize new effects. |
| Extension manifests/deployment grants, admin tools and other crates | Enumerate capabilities outside the scanned roots; unsupported scope operations reject. |
| Direct SQL/SpiceDB clients, migrations, schema, backup/restore | Restrict credentials, fence deployment generation and retire stale processes; source scans cannot prove exclusion. |

Concrete examples: [owner seeding](../../crates/waddle-server/src/spaces_pubsub_seed.rs)
overwrites Outcast; [MUC admin](../../crates/waddle-server/src/server/routes/websocket/handlers/iq/muc_admin.rs)
persists graph state before actor application and conditional rollback;
[member hydration](../../crates/waddle-server/src/server/durable_membership.rs)
uses separate graph reads and retains existing mirror behavior on failure.

A Rust facade is insufficient. Enrollment requires retiring old binaries and
credentials, preventing direct writes to the new namespace, and testing scope
enrollment before legacy fallback/alternate reads. Concrete database permissions,
schema isolation and SpiceDB writer capability boundaries remain B1. Mixed old
and new writers against enrolled scopes are unsupported. A partially covered
community stays unavailable; default-off is not permission to expose only the
sinks converted so far.

## Identity, migration and restoration

Agree one canonical bare account JID across local provisioning/lookup,
[directory normalization](../../crates/waddle-server/src/auth/directory.rs),
verified remote principals and grant keys before adopting #2053's codec.
Parser-stable bytes do not prove principal equality. Reject full JIDs and
non-account service identities as account-grant subjects; retain full JIDs for
routing separately. Pin normalization/codec/schema versions, Unicode behavior
and limits. Same-localpart foreign identities remain distinct. This is B6.

A fresh namespace requires explicit emptiness/isolation evidence and exact-JID,
named-community bootstrap with audit/recovery. Existing scopes require reviewed
inventory/reconciliation from identifiable authoritative sources. Never decode
ambiguous legacy substitutions to infer identity or union conflicting PubSub,
graph, room and SQL values. Unresolved conflicts stay closed. Retire old
credentials/writers before enrollment and record the cutover incarnation.
Rollback closes new scopes; it cannot automatically reopen old authority.

Restoring older PostgreSQL can restore an old generation and `active` flag.
Restore/startup therefore needs an independently enforced authority-incarnation
transition, invalidation of old credentials/work, and verified reconstruction
before admission. An epoch stored only inside the restored database is
insufficient. The mechanism surviving rollback and excluding the previous
deployment is unresolved (B7). Graph loss also requires closed admission before
reconstruction; silent active corruption is not covered by an old marker.

Home-account deletion does not automatically revoke remote grants. Durable
non-reuse of retired account JIDs across controlled provisioning paths, or a
separately approved lifecycle design, remains a pilot release requirement.

## Proposed first slice after design approval

Use existing room-manifest acceptance, not DirectFrame, for the first production
integration: one fresh isolated community, a managed members-only room and
explicit affiliations. Inheritance, moves and protocol-wide activation remain
outside the slice.

After B1-B4 and B6 are resolved for this scope, it would add:

1. Typed canonical operation/receipt/witness APIs, PostgreSQL journal and closed
   admission state with exact-operation recovery.
2. One bounded explicit room-affiliation family through production permission
   actor/SpiceDB, rejecting all legacy mutation routes for this scope.
3. Final witness validation in the real fresh-manifest transaction, preserving
   accepted-child replay and no-store behavior.
4. Native tests using production persistence, permission actor and acceptance
   paths, without claiming other joins, MAM, disco, PubSub or remote delivery
   have acquired an admission fence.

Expected files: `permissions/{actor,spicedb,schema}.rs`, a focused authority
repository/migration, `ingress/commit/implementation.rs`, `ingress/commit_room.rs`,
`ingress_uow`, and narrow MUC-admin integration. Actor projection and external
request/result semantics need preimplementation review. Unmanaged rooms,
alternate grants and unsupported sinks in enrolled scopes reject access.
Release configuration stays off; native fixtures exercise the path without
exposing a partially authorized community.

Clustered manifest activation remains dark until its roadmap dependencies
permit it. Single-node manifest evidence does not prove clustered delivery.
Independent authority contenders can prove database serialization without
claiming remote frozen-child completion. This slice would not complete F0/F3,
enable S2S or advertise an XMPP feature.

### Required native acceptance matrix

Use real PostgreSQL/production SpiceDB, independent processes and deterministic
barriers. Missing prerequisites fail; assert exact inventory/execution count.
Record boundary and backend. These are proposed tests, not results:

| Schedule/control | Required observation |
| --- | --- |
| Revoke after graph allow, before acceptance | No new manifest/child under stale witness. |
| Acceptance first | Revoke waits for transaction resolution; accepted children retain their replay contract. |
| Two replicas, cancellation/death at every stage | Recovery uses exact durable identity, not process-local authority. |
| Lost PostgreSQL commit reply, lost/never-forwarded graph reply | Exact readback separates committed, absent and unknown; timeout is not rollback. |
| Delayed grant/repair/compensation/activation | Cannot overwrite/reopen after newer revoke, including CAS-precondition refresh. |
| Bootstrap/restart after revoke/Outcast | No configured-owner or actor-restoration regrant. |
| Authority/projection/schema unavailable or malformed | New admission fails closed; revoke commit and effective grant stay distinct. |
| Backend termination between read and effect | No external effect before confirmed canonical acceptance; retain DirectFrame unsafe control. |
| Stalled scope and full queue | Bounded work and independent local/community progress; no network/queue wait under grant locks. |
| Exact retry and key reuse with changed input | Original outcome versus typed conflict; no new authorization. |
| Lifecycle/ABA/exhaustion/restored-old-state | Stale identity fails; restore cannot reopen old authority. |
| Initiator revoked while mutation prepared | Target mutation cannot use stale administrator authority. |
| Negative override/private config/alternate grant | Action policy, especially outcast precedence, not merely positive-path success. |
| #2052 accepted-child regressions | Frozen target/payload, occupancy and lifecycle remain unchanged. |

When code is introduced, run formatting, strict Clippy, relevant native/XEP
suites and required workspace checks. No S2S transport is needed; do not waive
the existing harness restriction. Broader suites and live tests are separate
evidence. Later slices must close join, read-response, PubSub, topology,
actor-restoration and remote-custody gaps individually.

## Blocking review ledger

| ID | Unresolved decision/evidence | Work blocked |
| --- | --- | --- |
| B1 | Exhaustive scope writer/reader exclusion, old writer retirement and concrete store capabilities | Enrollment and production authority integration. |
| B2 | Complete action/negative/admin/global dependency policy, actor projection, XMPP request/result identity | Affected protected operations and handlers. |
| B3 | CAS/marker/schema protocol, bounded reconstruction, missing-marker and large-graph rules | Projection integration beyond independently proven bounded operations. |
| B4 | Actual claim/authority/child lock order, atomic admission and cancellation/lost-commit ownership | First manifest integration. |
| B5 | PubSub/DirectFrame admission identity; read/join/SM sink contracts | Those sinks and community activation. |
| B6 | Canonical JID agreement, isolated codec and authoritative reconciliation | External identities and enrolled authority keys. |
| B7 | Restore incarnation surviving rollback and excluding old deployment/work | Recovery capability and pilot activation. |

The coarse fence's availability cost is a non-blocking tradeoff once bounded
resources and independent progress are proved. Fine-grained fences can follow
only after complete policy/mutation dependencies are demonstrated. Merging this
document or passing an inventory scan discharges none of these blockers.
