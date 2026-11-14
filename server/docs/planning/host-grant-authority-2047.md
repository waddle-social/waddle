# Candidate host-grant authority and admission boundary (#2047)

## Status

**Selected with conditions by ADR-0020 decision D5; activation remains
blocked.** This document recommends PostgreSQL as the single canonical
host-grant authority and SpiceDB as its reconstructible policy projection.
[ADR-0020](../adrs/0020-independent-community-federation.md) accepts that
selection on conditions B1, B4, B6 and B7 of the
[blocking ledger](#blocking-review-ledger), and its first slice evaluates
admission in SQL inside the acceptance transaction. The selection discharges no
ledger entry and changes no runtime behavior. The
[federation plan](../../../docs/planning/federation-2047.md) and distributed
actor roadmap retain their ordering and activation gates.

Each ledger entry is an ADR-0020 gate with a decision part and an evidence
part. Under the ADR's gate semantics, the slice that owns an entry decides it
for its scope, records the decision here and has it independently reviewed
before that slice merges into the integration branch; an open entry is not
approval to merge the surfaces it covers. The evidence is discharged by
executed tests: the first grant-authority slice's and the revocation fence's
focused sets run when those slices complete (ADR-0020 D9), and the rest in
the verification phase. The ledger names the owning slice of each entry, and
the [acceptance matrix](#required-native-acceptance-matrix) names the slice
that owns each case.

ADR-0020 D8 also bounds remote authority. A remote principal may hold a
privileged grant (owner, admin or manager standing, or any other grant that
confers a management permission) only when its home domain is on the
operator's trusted-home list, and admission honors that grant only on a
stream with `pkix` strength while the home is still listed. Grant issuance,
bootstrap and repair refuse a privileged grant to a JID of any other home.
Room moderation (the XEP-0045 moderator role and the channel `moderate`
permission) is honored for a remote principal only on a `pkix` stream. A
message author's own `edit` and `delete` are neither. Policy (B2) must encode
all of these.

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
No other channel permission (`send_message`, `view`, `moderate`, `manage` or
`delete`) subtracts `outcast` either, so a room outcast who can view the parent
Space appears to keep access. ADR-0020 D5 requires outcast precedence in every
channel action, made as its own reviewed behavior change (W5o in the plan).
Because every replica writes the built-in SpiceDB schema at startup
([`permissions/actor.rs`](../../crates/waddle-server/src/permissions/actor.rs)),
an older replica would revert that change, so it needs the fenced schema
transition described under projection below (part of B3).
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
need a reviewed decision before the slice that adds them merges into the
integration branch, and executed evidence in verification (B3).

## Admission, custody and lock lifetime

Prepare a witness outside the acceptance transaction: canonical subject, exact
action/resource, authority/scope lifecycle, policy/schema generation, relevant
room state and, where admission reads the projection, the anchored graph
result. A cached boolean is insufficient. In the first slice, admission does
not read the projection: its witness is the PostgreSQL policy state locked in
the acceptance transaction (see [first slice](#first-slice)).

For a remote principal the witness also binds its verified domain, the
authentication strength of the stream it arrived on (`pkix` or `dialback`), the
current ADR-0020 D1 admission decision for that domain, including a denylist
entry for a parent domain, and whether its home is on the trusted-home list
the replica runs. Admission rejects work from a domain that is no longer
admitted; honors privileged authority only for a trusted home on a `pkix`
stream and room moderation only on a `pkix` stream (D8); and enforces any host
policy that requires `pkix` strength (ADR-0020 gate G17).

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
authority locks. B4 must be resolved for the chosen slice, and independently
reviewed, before that slice merges into the integration branch.

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
| Remote handoff | Durable custody, retry identity and recipient binding; queue/socket/remote acceptance is not recipient delivery. The S2S routing slice defines this contract (ADR-0020 gate G8); the existing actor/durable roadmap still applies. |

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
of fencing. PubSub remains blocked (B5) until the writer-fencing slice (W5c in
the plan) decides this contract; the revocation slice (W5d) then turns the
#2056 negative control into a passing fence test.

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
ADR-0020 D5 requires the grant-authority work (the writer-fencing slice, W5c
in the plan) to retire the owner reseeding that overwrites Outcast, the latent
legacy `set_room_affiliation` path and the graph-first MUC admin write, not
only to reject them for enrolled scopes.

A Rust facade is insufficient. Enrollment requires retiring old binaries and
credentials, preventing direct writes to the new namespace, and testing scope
enrollment before legacy fallback/alternate reads. Concrete database permissions,
schema isolation and SpiceDB writer capability boundaries remain B1. Mixed old
and new writers against enrolled scopes are unsupported. A partially covered
community stays unavailable; default-off is not permission to expose only the
sinks converted so far. The first grant-authority slice (W5b in the plan)
decides the store capabilities and credential separation; the writer-fencing
slice (W5c) closes the writer inventory; retiring old binaries and credentials
is part of the rollout runbook that merges the work into main (ADR-0020
gate G24) and of the activation runbook (G22).

## Identity, migration and restoration

Agree one canonical bare account JID across local provisioning/lookup,
[directory normalization](../../crates/waddle-server/src/auth/directory.rs),
verified remote principals and grant keys before adopting #2053's codec.
Parser-stable bytes do not prove principal equality. Reject full JIDs and
non-account service identities as account-grant subjects; retain full JIDs for
routing separately. Pin normalization/codec/schema versions, Unicode behavior
and limits. Same-localpart foreign identities remain distinct. This is B6.

B6 also blocks validation that begins after typed stanza parsing. An independent
B6 probe using the pinned `jid` 0.12.2 and Rust 1.98.1 reported these concrete
results (probe evidence supplied by the reviewer; not rerun for this document):

| Input | Parsed identity / comparison result |
| --- | --- |
| `fußball@example.org` | Becomes `fussball@example.org`. |
| `a@faß.example` | Becomes `a@fass.example`. |
| `a@example.org.` | Retains the trailing dot; differs from `a@example.org`. |
| `a@bücher.example` and `a@xn--bcher-kva.example` | Remain different. |

At the current [XML parse boundary](../../crates/waddle-xmpp/src/protocol/frame.rs),
`parse_stanza` consumes the XML-decoded `Element` into typed IQ/message/presence
structures. `ParsedFrame` retains the typed stanza and language metadata, not
the original decoded address attributes. The
[WebSocket handler](../../crates/waddle-server/src/server/routes/websocket/frame.rs)
receives that already-parsed result. Rejecting non-ASCII only afterward cannot
detect an original address that has already collapsed to ASCII; scanning wire
bytes also misses XML character references that decode to non-ASCII. A proposed
restricted profile therefore requires validation of XML-decoded identity inputs
before lossy JID preparation, including corresponding future S2S and
protocol-payload identity boundaries. Merely checking the resulting typed JID
or adding a new grant-key codec cannot satisfy B6.

One candidate for an isolated test profile is a precisely bounded ASCII account
and service namespace, with explicit permitted localpart/domain grammar,
case/trailing-dot rules, authoritative account bindings and identical checks at
provisioning, authentication, lookup and every enrolled ingress. The profile
and enforcement locations still need review; this is neither an approved
production restriction nor a claim of full XMPP address support. Production
domains remain undecided; fixtures must use isolated test domains. Full Unicode
support requires an agreed implementation of
[RFC 7622 domainpart/localpart rules](https://www.rfc-editor.org/rfc/rfc7622.html#section-3.2)
and the [UsernameCaseMapped profile](https://www.rfc-editor.org/rfc/rfc8265.html#section-3.3),
plus versioned reconciliation of existing account/authority stores. Distinct
source accounts must never be silently merged by parser output. Account-store
canonical bindings, the raw-boundary implementation and the full PRECIS/IDNA
migration remain blocking decisions; the candidate ASCII test profile does not
resolve them. The production authorization ID codec (W1 in the plan) covers
only the codec part of B6. The identity-profile slice (W5i) owns the rest:
choosing the profile, enforcing it on XML-decoded inputs at C2S parsing,
provisioning, authentication and directory lookup, and the canonical account
binding; the S2S routing slice (W5a) applies the same enforcement at S2S
ingress.

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
deployment is unresolved (B7); the restore-incarnation slice (W5r in the plan)
owns it. Graph loss also requires closed admission before reconstruction;
silent active corruption is not covered by an old marker.

Home-account deletion does not automatically revoke remote grants. Waddle homes
enforce durable non-reuse of retired account JIDs across their provisioning
paths (the revocation slice, W5d). Other homes cannot be required to. By
ADR-0020 D8, a reassigned JID at a home that is not on the trusted-home list
may inherit non-privileged grants, including room moderation, as an accepted
limitation, and never privileged ones; a trusted home is trusted for
non-reuse, and an operator who lists it accepts that its reassignments could
pass on privileged grants. After a reported reassignment, the host revokes the
JID's grants through the fence and issues any grant for the new holder only
after confirming the holder with the remote home's operator outside the
protocol.

The identity-profile slice also defines the canonical domain form (RFC 7622
preparation, trailing dot, U-label and A-label equivalence) that ADR-0020 D1's
admission, dialback and trusted-home lists, the verifier's reference
identities and S2S routing compare in (gates G1 and G10).

## First slice

This is ADR-0020 D5's first slice, W5b in the plan. Use existing room-manifest
acceptance, not DirectFrame, for the first production integration: one fresh
isolated community, a managed members-only room and explicit affiliations.
Inheritance, moves and protocol-wide activation remain outside the slice. The
other surfaces in the admission table (joins, protected reads, PubSub and
stream-management fanout, queued work and room administration) and the writer
inventory belong to the writer-fencing slice (W5c), and admission of remote
principals to the effect-boundary slice (W5e).

Before the slice merges into the integration branch, these must be decided for
its scope and independently reviewed: B1 (store capabilities and credential
separation), B4, B6 as it applies to grant subjects, B2 for the actions it
protects, and the B3 marker protocol for the projection writes it makes. It
adds:

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
request/result semantics need a reviewed decision (B2) before the slice merges
into the integration branch. Unmanaged rooms,
alternate grants and unsupported sinks in enrolled scopes reject access.
Release configuration stays off; native fixtures exercise the path without
exposing a partially authorized community.

Under ADR-0020 D5, this slice evaluates admission in SQL inside the
fresh-manifest acceptance transaction. Its witness is the PostgreSQL state
locked in that transaction; the SpiceDB projection is not consulted for its
admission decision, so B3's admission rules do not gate this slice. The slice
still decides the marker protocol for the projection writes it makes. B3 gates
any later admission that reads the projection, which D5 defers until
inheritance needs it.

SQL admission is therefore independent of SpiceDB availability. While SpiceDB
is unavailable or behind, this slice's admission keeps deciding from
PostgreSQL: a revoke takes effect at its PostgreSQL commit, and a grant that
is already effective keeps admitting. A new grant stays recorded but not
effective until its projection is verified and activated, as above. Only a
decision that consults SpiceDB depends on the projection: no admission inside
this slice's enrolled scope does, and the surfaces outside it keep their
existing checks until later slices fence them. A later admission that does
consult the projection fails closed on missing, stale or malformed projection
state (B3).

Clustered manifest activation remains dark until its roadmap dependencies
permit it. Single-node manifest evidence does not prove clustered delivery.
Independent authority contenders can prove database serialization without
claiming remote frozen-child completion. This slice would not complete F0/F3,
enable S2S or advertise an XMPP feature.

### Required native acceptance matrix

Use real PostgreSQL/production SpiceDB, independent processes and deterministic
barriers. Missing prerequisites fail; assert exact inventory/execution count.
Record boundary and backend. These are proposed tests, not results.

Each case names the slice that owns it and, through the federation plan, when
it runs. A case marked W5b belongs to this slice's focused set, which runs when
the slice completes (ADR-0020 D9); it needs only this slice and the slices it
depends on. A case marked W5d belongs to the revocation fence's focused set,
which runs when that slice completes. Every other case is evidence for its
owning slice and runs in the verification phase: step 2 for suites, step 4
for native PostgreSQL and SpiceDB cases, and steps 6 and 7 for cases that need
S2S.

| Schedule/control | Required observation | Owner |
| --- | --- | --- |
| Revoke commits after a candidate admission read, before acceptance | The acceptance transaction re-reads and locks the SQL policy state; no manifest or child is accepted under the stale read. | W5b |
| Acceptance first | Revoke waits for transaction resolution; accepted children retain their replay contract. | W5b |
| Two replicas, cancellation/death at every stage | Recovery uses exact durable identity, not process-local authority. | W5b |
| Lost PostgreSQL commit reply; lost or never-forwarded projection-write reply | Exact readback separates committed, absent and unknown; timeout is not rollback. | W5b |
| Delayed grant, projection write or activation for this slice's relation family | Cannot overwrite/reopen after newer revoke, including CAS-precondition refresh. | W5b |
| Delayed repair or compensation by any other writer | Cannot overwrite/reopen after newer revoke. | W5c (step 4) |
| Restart after revoke/Outcast within the enrolled scope | Legacy mutation routes stay rejected for the scope; no configured-owner or actor-restoration regrant. | W5b |
| Bootstrap, owner reseed and actor restoration after revoke/Outcast, across every writer | No regrant from any retired or fenced writer. | W5c (step 4) |
| PostgreSQL authority unavailable, or a commit outcome uncertain | New admission fails closed; revoke commit and effective grant stay distinct. | W5b |
| SpiceDB unavailable, lagging or malformed while SQL admission runs | SQL admission continues: a revoke is effective at its PostgreSQL commit and already effective grants keep admitting; a new grant does not become effective before its projection is verified; no projection state reopens a revoked grant. | W5b |
| Built-in schema differs from the stored versioned schema at startup | The replica refuses to start rather than rewrite the schema. | W5o (steps 2 and 4) |
| Backend termination between read and effect on the manifest path | No external effect before confirmed canonical acceptance. | W5b |
| #2056 captured Space fanout after a revoke ACK | The stale capture never reaches the queue; the control that showed the unsafe delivery passes as a fence test. | W5d |
| Stalled scope and full queue | Bounded work and independent local/community progress; no network/queue wait under grant locks. | W5b |
| Exact retry and key reuse with changed input | Original outcome versus typed conflict; no new authorization. | W5b |
| Generation and lifecycle-identity exhaustion in the enrolled scope | Stale identity fails; exhaustion fails closed; identities are never reused. | W5b |
| Space, room or node deletion and recreation | Old witnesses, grants and markers do not apply to the new lifecycle. | W5c (step 4) |
| Restored older PostgreSQL state | Restore cannot reopen old authority; old credentials and work are invalid; admission stays closed until verified reconstruction. | W5r (step 4) |
| Initiator revoked while mutation prepared | Target mutation cannot use stale administrator authority. | W5b |
| Negative override/private config/alternate grant | Action policy, especially outcast precedence, not merely positive-path success. | W5b |
| #2052 accepted-child regressions | Frozen target/payload, occupancy and lifecycle remain unchanged. | W5b |
| Full JID or service identity offered as an account-grant subject | Refused; grant keys use the identity profile's canonical bare-JID binding. | W5b |
| Raw versus typed JIDs; XML character references; case, sharp-s, trailing-dot and A-label/U-label pairs | Enforce the approved identity profile before lossy parsing; reject unsupported input without aliasing another account. Provisioning/authentication/lookup/ingress agree on audited bindings. | W5i (step 2); W5a at S2S ingress (step 6) |
| Privileged grant issued through this slice's path to a remote JID, with the trusted-home set supplied as input | Refused for a home outside the set; recorded for a home in it. | W5b |
| Privileged grant for a JID of a home not on the trusted-home list, written by bootstrap or repair | Refused. | W5c (step 4) |
| Stored privileged grant for a remote principal: trusted home on `pkix` and on dialback-only streams; home removed from the list; home never listed, grant stored outside the canonical path | Honored only for a listed home on `pkix`; otherwise ignored, whatever is stored. | W5e (steps 6 and 7) |
| Room moderation by a remote principal on `pkix` and on dialback-only streams; a message author's own edit and delete | Moderation honored only on `pkix`; the author's own edit and delete are not moderation and are not restricted by it. | W5e (steps 6 and 7) |
| Outcast on every channel action, including `moderate`, `manage` and `delete` | Outcast wins every action at this slice's admission. | W5b |
| Acknowledged revoke across two replicas with queued work at joins, protected reads, PubSub and stream-management fanout and room administration | No new protected admission under the revoked grant after the fence, on any replica. | W5d |
| Retired account JID on a Waddle home | Never issued to a new holder through any provisioning path. | W5d |

The W5b focused set is every row marked W5b; the W5d focused set is every row
marked W5d. Both run again on the integrated tree in verification step 4. No
S2S transport is needed for either; do not waive the existing harness
restriction. Broader suites and live tests are separate evidence. Later slices
must close join, read-response, PubSub, topology, actor-restoration and
remote-custody gaps individually.

## Blocking review ledger

Slice names refer to [the federation plan](../../../docs/planning/federation-2047.md),
whose gate table also names the verification step that discharges each entry's
evidence.

| ID | Unresolved decision/evidence | Work blocked | Under ADR-0020 |
| --- | --- | --- | --- |
| B1 | Exhaustive scope writer/reader exclusion, old writer retirement and concrete store capabilities | Enrollment and production authority integration. | D5 condition, gate G11; open. Decided by W5b (store capabilities, credential separation) and W5c (writer inventory and exclusion); old binaries and credentials are retired under the rollout runbook (G24) and the activation runbook (G22). |
| B2 | Complete action/negative/admin/global dependency policy, actor projection, XMPP request/result identity | Affected protected operations and handlers. | Gate G14; open. Decided per protected surface by W5b, W5c and W5e. Includes outcast precedence in every channel action (W5o) and the D8 rules: privileged grants for remote principals only from trusted homes and only on `pkix` streams, and room moderation only on `pkix` streams. |
| B3 | CAS/marker/schema protocol, bounded reconstruction, missing-marker and large-graph rules | Projection integration beyond independently proven bounded operations. | Gate G15; open. Off the first slice's safety path, which admits in SQL. W5b decides the marker protocol for the projection writes it makes, and W5o the fenced schema transition. Any admission that reads the projection needs B3 decided first; no slice in the plan adds one. |
| B4 | Actual claim/authority/child lock order, atomic admission and cancellation/lost-commit ownership | First manifest integration. | D5 condition, gate G12; open. Decided by W5b and extended by W5c for each surface it fences. |
| B5 | PubSub/DirectFrame admission identity; read/join/SM sink contracts | Those sinks and community activation. | Gate G16; open. Decided by W5c; W5d's #2056 control is its evidence. |
| B6 | XML-decoded pre-parse enforcement, canonical JID/account-binding agreement, isolated codec and authoritative reconciliation; ASCII test profile and full PRECIS/IDNA migration unresolved | External identities and enrolled authority keys. | D5 condition, gate G10; open. W1 covers only the codec part; W5i decides the rest, including the canonical domain form, and W5a applies it at S2S ingress. |
| B7 | Restore incarnation surviving rollback and excluding old deployment/work | Recovery capability and pilot activation. | D5 condition, gate G13; open. Decided by W5r. |

The coarse fence's availability cost is a non-blocking tradeoff once bounded
resources and independent progress are proved. Fine-grained fences can follow
only after complete policy/mutation dependencies are demonstrated. Merging this
document, accepting D5 or passing an inventory scan discharges none of these
blockers.
