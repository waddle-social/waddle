# Candidate host-grant authority and admission boundary (#2047)

## Status

**Selected with conditions by ADR-0020 decision D5; activation remains
blocked.** This document recommends PostgreSQL as the single canonical
host-grant authority and SpiceDB as its reconstructible policy projection.
[ADR-0020](../adrs/0020-independent-community-federation.md) accepts that
selection on conditions B1, B4, B6 and B7 of the
[blocking ledger](#blocking-review-ledger), and its first slice evaluates
admission in SQL inside the acceptance transaction. The selection discharges no
ledger entry. The
[federation plan](../../../docs/planning/federation-2047.md) and distributed
actor roadmap retain their ordering and activation gates.

The first slice is implemented (`permissions/host_grant`, migration 2001).
What it decided for its scope is recorded under
[First-slice decisions](#first-slice-decisions); the evidence for those
decisions is the first slice's focused set, executed as described under
[Running the first slice's focused set](#running-the-first-slices-focused-set).
No scope is enrolled by any release configuration, and nothing enrolls one at
runtime, so the slice changes no behavior of an existing deployment beyond
its migration, the built-in authorization schema's version, one indexed
lookup on legacy channel and Space permission writes, a privilege check of
the runtime role when a PostgreSQL replica starts, and the refusal of a room
address that is not in canonical form (see
[Rollout part](#rollout-part-g24)).

The restore slice (W5r in the plan, condition B7) is implemented as well
(`permissions/host_grant/restore` and `witness`, migration 2002, the
`waddle-server host-grant` operator subcommand). Its decisions are recorded
under [Restore incarnation decisions](#restore-incarnation-decisions), and
its operator procedure is the
[restore runbook](../../../docs/operations/authz-host-grant-restore.md). Its
suite has run against disposable stores (see
[Running the restore slice's cases](#running-the-restore-slices-cases));
verification step 4 has not.

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
confers a management permission) or room moderation (the XEP-0045 moderator
role and the channel `moderate` permission) only when its home domain is on
the operator's trusted-home list, and admission honors either only on a
stream with `pkix` strength while the home is still listed. Grant issuance,
bootstrap and repair refuse a privileged or moderator grant to a JID of any
other home, whose accounts hold only member-level grants. A grant that is not
honored contributes nothing: it yields neither `moderate` nor the
member-level `read` and `send_message` the schema would otherwise derive from
it. A message author's own `edit` and `delete` are neither privileged nor
moderation; outcast removes them in that room (ADR-0020 D5). Policy (B2) must
encode all of these.

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
channel action and every message action in that room, including a message
author's own `edit` and `delete`, while a moderator can still remove an
outcast's messages through the moderator's own `moderate`. It is made as its
own reviewed behavior change (W5o in the plan).
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
admitted; honors privileged authority and room moderation only for a trusted
home on a `pkix` stream, and lets a grant that is not honored contribute
nothing, including no member-level permission derived from it (D8); and
enforces any host policy that requires `pkix` strength (ADR-0020 gate G17).

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
insufficient. The restore slice (W5r in the plan) decided the mechanism that
survives rollback and excludes the previous deployment for the first slice's
scope; see [Restore incarnation decisions](#restore-incarnation-decisions),
including what it leaves open. Graph loss also requires closed admission
before reconstruction; silent active corruption is not covered by an old
marker.

Home-account deletion does not automatically revoke remote grants. Waddle homes
enforce durable non-reuse of retired account JIDs across their provisioning
paths (the revocation slice, W5d). Other homes cannot be required to. By
ADR-0020 D8, a reassigned JID at a home that is not on the trusted-home list
may inherit member-level grants, as an accepted limitation, and never room
moderation or privileged grants; a trusted home is trusted for non-reuse, and
an operator who lists it accepts that its reassignments could pass on
privileged grants and room moderation. After a reported reassignment, the
host revokes the JID's grants through the fence and issues any grant for the
new holder only after confirming the holder with the remote home's operator
outside the protocol.

The identity-profile slice also defines the canonical domain form (RFC 7622
preparation, trailing dot, U-label and A-label equivalence) that ADR-0020 D1's
admission, dialback and trusted-home lists, the verifier's reference
identities, S2S routing and discovery compare in (gates G1 and G10). The same
form classifies the domains ADR-0020 D3 refuses: both RFC 7622 IP-address
forms, other names a resolver or address parser accepts as an IPv4 address,
such as `2130706433`, `0x7f.1` and `127.1`, and unqualified single-label
names.

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
| Restored older PostgreSQL state | Restore cannot reopen old authority; old credentials and work are invalid; admission stays closed until verified reconstruction. The rows below are its cases. | W5r (step 4) |
| Undeclared restore that lost projected operations, or operations only the journal witness saw; projection of an incarnation the journal does not record; a scope that admits without a marker | The fence closes that scope as `fenced` (and reconciliation on the same evidence); the server starts and other scopes are unaffected. | W5r (step 4) |
| Projection and room witnesses of a scope the store does not hold | Its rooms are refused, not served as unmanaged, whether or not the replica is attached (before any pass has read them, from the room's witness); other rooms are unaffected; the server starts. | W5r (step 4) |
| Adopted lineage; configured restore epoch other than the recorded one; PostgreSQL position behind its witness, or on an unacknowledged system or timeline; scopes that admit beside a SpiceDB without this authority's markers and position witness | The fence refuses the authority and the replica holds no attachment; the owner phase refuses an adopted lineage and an older epoch; each names only the refusal's kind. A regression is latched and stays refused after the server writes past the witness. An acknowledgement is bound to the witness it supersedes and spent with it; it accepts a later timeline of the same system or a regression on the witnessed timeline, never a regression across timelines or another system. A SpiceDB without the projection closes nothing. A database without a scope beside a projection without one is not refused. The position is read under the restricted runtime profile. | W5r (step 4) |
| Owner-phase act (retirement) the database lost | The next pass that passes witnesses it, retired as the scope is; the fence closes the restored scope. | W5r (step 4) |
| Declared restore with an active, a never-activated and a retired scope; a transition whose run stopped after its first commit | New incarnation, epoch and lineage recorded; every scope not retired or exhausted closed as `restored` at its next generation in bounded batches, effective standing lowered to `none`; nothing admits and no initiator is honoured; a rerun finishes a stopped transition and otherwise changes nothing; an older epoch and the runtime role are refused. | W5r (step 4) |
| Command, activation, projection pass and replica attachment from before a declared restore | Stale generation, superseded activation, no takeover from a plan that is not a `RestoredPlan`, and no acceptance on a replica whose attachment names another binding; a replica configured for the earlier epoch does not attach. | W5r (step 4) |
| A replica whose binding no longer matches (server restart, declared restore); periodic passes | The acceptance is refused and the replica verifies the fence once in place: it admits again with the new binding when the fence passes. While it fails the replica stays ready, except one configured with an epoch below the recorded one. A periodic pass drops the attachment on a refusal, on a runtime role that no longer matches, and after three passes in a row that could not complete. | W5r (step 4) |
| Reconstruction after a restore that lost a projected revoke, ban or grant; after one that lost nothing; beside relationships no canonical row names; of a remote privileged grant | Lost revokes and bans stay in force; lost grants are not recreated; recorded increases are kept; subjects only the projection held are recorded at `none` or `outcast` and dropped from it; nothing is raised in what admission honours; the scope is `reconstructed` once the rebuilt projection is read back and activated, and admits only after an operator reopens it. | W5r (step 4) |
| Operator revoke and reopen | A revoke or ban applies in a closed scope at its commit, never raises, keeps an owner and replays; a reopen opens only a scope whose latest operation is verified, and one a restore lost operations in only by name with the operator's confirmation. | W5r (step 4) |
| Restore to before an earlier declared restore, with and without the configuration rolled back | Refused by the configured epoch, by the position witness's epoch, and otherwise the scope is closed for the projection's unknown incarnation; a new declaration reconstructs over the projection of the lost incarnation. | W5r (step 4) |
| Reconstruction that does not fit one projection write; a malformed family; a missing marker | The scope stays closed with a contradiction; nothing is recorded or written. | W5r (step 4) |
| Initiator revoked while mutation prepared | Target mutation cannot use stale administrator authority. | W5b |
| Negative override/private config/alternate grant | Action policy, especially outcast precedence, not merely positive-path success. | W5b |
| #2052 accepted-child regressions | Frozen target/payload, occupancy and lifecycle remain unchanged. | W5b |
| Full JID or service identity offered as an account-grant subject | Refused; grant keys use the identity profile's canonical bare-JID binding. | W5b |
| Raw versus typed JIDs; XML character references; case, sharp-s, trailing-dot and A-label/U-label pairs | Enforce the approved identity profile before lossy parsing; reject unsupported input without aliasing another account. Provisioning/authentication/lookup/ingress agree on audited bindings. | W5i (step 2); W5a at S2S ingress (step 6) |
| Privileged or moderator grant issued through this slice's path to a remote JID, with the trusted-home set supplied as input | Refused for a home outside the set; recorded for a home in it. | W5b |
| Privileged or moderator grant for a JID of a home not on the trusted-home list, written by bootstrap or repair | Refused. | W5c (step 4) |
| Stored privileged grant for a remote principal: trusted home on `pkix` and on dialback-only streams; home removed from the list; home never listed, grant stored outside the canonical path | Honored only for a listed home on `pkix`; otherwise ignored, whatever is stored. | W5e (steps 6 and 7) |
| Room moderation by a remote principal: trusted home on `pkix` and on dialback-only streams; home not on the list, on `pkix`; a message author's own edit and delete | Moderation honored only for a listed home on `pkix`; the author's own edit and delete are not moderation and are not restricted by it. | W5e (steps 6 and 7) |
| Former admin of a home removed from the trusted-home list, holding no separate member-level grant | Cannot moderate or read through that grant: an unhonored grant contributes no permission. | W5e (steps 6 and 7) |
| Outcast on every action the SQL policy protects, including channel `moderate`, `manage` and `delete` and an outcast author's own message `edit` and `delete` | The SQL policy evaluates outcast for every action it protects, and outcast wins each at this slice's admission. | W5b |
| Acknowledged revoke across two replicas with queued work at joins, protected reads, PubSub and stream-management fanout and room administration | No new protected admission under the revoked grant after the fence, on any replica. | W5d |
| Retired account JID on a Waddle home | Never issued to a new holder through any provisioning path. | W5d |
| Groupchat planned by the room planner for a revoked member, a member of the parent Space without a room grant, and a seated account without a grant | The acceptance is reduced to the admitted audience: none of them is routed, projected, pushed or notified anything, nothing is recorded for a later replay to deliver, and the admitted members receive the message. | W5b |
| Plan that holds work a managed room does not account for; room address that is not canonical | The stanza is refused with a committed error of type `cancel`; a sender who is not admitted is told only that. | W5b |
| Stored owner or admin grant of a remote account at this slice's admission and as a mutation's initiator | Contributes nothing, member-level access included, and does not count as the owner a room keeps; a member-level grant of a remote account is honored. | W5b |
| Replica whose runtime role does not match the store's privilege profile | Holds no attachment and serves no enrolled scope; rooms no scope manages are unaffected. | W5b |
| Enrollment and retirement with the runtime credential | Refused by PostgreSQL before anything changes. | W5b |
| Projection read that does not answer | The permission actor keeps answering what it decides without SpiceDB; the read ends at its deadline with an unknown outcome. | W5b |

The W5b focused set is every row marked W5b; the W5d focused set is every row
marked W5d. Both run again on the integrated tree in verification step 4. No
S2S transport is needed for either; do not waive the existing harness
restriction. Broader suites and live tests are separate evidence. Later slices
must close join, read-response, PubSub, topology, actor-restoration and
remote-custody gaps individually.

## First-slice decisions

These are the decisions the first grant-authority slice made for its scope.
They are decisions, not evidence: the acceptance matrix above is their
evidence and has not been run. Later slices extend each of them for the
surfaces and writers they add, and must not rely on a bound stated here
without restating it.

### What the slice covers

- A scope is a community key (service JID and Space node) with exactly one
  lifecycle. A key is enrolled once; a retired or exhausted scope is never
  enrolled again, so a lifecycle identity cannot be reused and has no ordinal
  to exhaust. Recreation of a scope under a new lifecycle is not built.
- A scope manages at most 8 rooms and holds at most 200 grant rows, counting
  revoked subjects (`HOST_GRANT_SCOPE_MAX_ROOMS`,
  `HOST_GRANT_SCOPE_MAX_GRANT_ROWS`). A change to a subject that already
  holds a row is always recorded, so a full scope can still revoke and ban;
  only a grant to a new subject is refused at the bound. The bound is what
  keeps one projection write inside one `WriteRelationships` request; a
  larger scope needs the chunking decision this slice does not make.
- A managed room is members-only and its grants are the explicit room
  affiliations `owner`, `admin`, `member`, `outcast` and `none`. Nothing is
  inherited from the parent Space.
- The store is PostgreSQL only. SQLite holds no host-grant relation, no scope
  can be enrolled there, and every room keeps its existing admission.
- Nothing calls the mutation API from an XMPP handler. Mapping an XMPP
  request to an operation identity is undecided and belongs to the slice
  that integrates a handler.
- A managed room accepts groupchat messages from its members, including a
  correction, a retraction and link-preview references of such a message.
  It accepts nothing else that is planned for it: occupant private
  messages, the room's own system messages, groupchat sent by an extension
  bot, subject changes, pins and call signalling are refused (see
  [B2](#b2-policy-for-the-action-this-slice-protects-g14-part)).

### Canonical model as built

Migration 2001 creates seven relations.

| Relation | Holds |
| --- | --- |
| `host_grant_authority` | The deployment's authority incarnation, one row. Every scope and every projection marker is bound to it; a scope bound to another incarnation admits nothing and accepts no grant change, and its projection is neither written nor activated. Migration 2002 adds the restore epoch the incarnation was established under, the database lineage it is bound to, a fixed authority identity and an operator's position acknowledgement; only a declared restore changes the incarnation (see [Restore incarnation decisions](#restore-incarnation-decisions)). |
| `host_grant_scope_lifecycles` | One row per enrollment: lifecycle identity and scope key, both unique. Never deleted, and updated once at most: retirement stamps `retired_at_ms`. Only the schema owner writes it. |
| `host_grant_scopes` | The latest state of a scope: incarnation, policy version, generation, the operation that recorded it, the latest generation whose projection was verified and activated, and `closed` or `active` with the reason for `closed` (`enrolled`, `retired`, `exhausted`; migration 2002 adds `restored`, `reconstructed`, `fenced` and the `lost_operations` mark). Its row lock is the scope's authority guard. |
| `host_grant_rooms` | The rooms a scope manages and each room's authorization object. A binding is immutable. |
| `host_grants` | One row per room and subject: the recorded affiliation, the effective affiliation, the generation and operation that last changed it, and why issuance accepted it. A revoke is a row whose affiliation is `none`. |
| `host_grant_operations` | The immutable outcome of each operation: identity, fingerprint of its input, kind, initiator, incarnation, generation and the digest of the intended projection. One operation per generation of a lifecycle. |
| `host_grant_operation_changes` | The grant changes of each operation, in order, with the previous affiliation. |

An operation carries a stable identity and its fingerprint covers every
input, including the generation it was decided under and its initiator. An
exact retry returns the recorded outcome; the same identity with any other
input is a typed conflict; neither authorizes anything again.

Each recorded operation advances the scope's generation by one. A generation
is `i64`; the largest value is reserved as terminal. An operation that finds
no working generation left does not make its change: it closes the scope at
the terminal generation, permanently, and is recorded as the operation that
exhausted it. Because the change is not made, the digest that operation
records is the digest of the grant rows as they stand, not of the projection
the command intended.

Recorded and effective are separate columns. A reduction of a subject's
access (a revoke, an outcast, a demotion) lowers the effective affiliation in
the transaction that records it, so it takes effect at that commit. An
increase leaves the effective affiliation as it was and takes effect only
when the operation's projection has been written, verified and activated.
Affiliations are ordered `outcast < none < member < admin < owner`, and the
effective affiliation after an operation is the lower of the previous
effective affiliation and the recorded one, so lifting a ban together with a
new grant keeps the subject an outcast until the grant is activated.

This differs from the general proposal above in one respect, as the first
slice's text requires: a mutation does not close the scope's admission.
Admission in this slice reads only the effective columns, under the scope's
guard, so withholding the increase is sufficient and already effective
grants keep admitting. The coarse generation still orders every operation of
the scope and still invalidates any witness prepared outside a transaction.
A scope is `closed` only before its first activation, after retirement and
after exhaustion.

### B1: store capabilities and credential separation (G11)

PostgreSQL. The relations are created by the migration, as the schema owner,
and carry no trigger, function or foreign key. The runtime role's privileges
come from the owner phase's grant matrix
(`server/runtime_owner.rs`):

| Relation | Runtime role | Why |
| --- | --- | --- |
| `host_grant_authority` | `SELECT` | Only the owner changes the incarnation. |
| `host_grant_scope_lifecycles` | `SELECT` | Only the owner enrolls a scope. |
| `host_grant_rooms` | `SELECT` | Only the owner binds a room. |
| `host_grant_scopes` | `SELECT`, `UPDATE` | The runtime locks the guard and advances the state; it cannot insert a scope. |
| `host_grants` | `SELECT`, `INSERT`, `UPDATE` | Grants are upserted; a revoke is a row. |
| `host_grant_operations`, `host_grant_operation_changes` | `SELECT`, `INSERT` | The journal is append-only. |

Consequences this slice relies on: the runtime credential cannot enroll a
scope, bind a room, bootstrap a grant or retire a scope, so no restart,
configuration value or account name can create authority or end a scope; it
cannot delete or truncate any host-grant row; and it cannot rewrite an
operation's outcome.

Enrollment and retirement are owner-phase operations. Each is one
transaction that writes the lifecycle ledger: enrollment inserts the scope's
row with its rooms and bootstrap grants (`HostGrantStore::enroll_scope`),
which pass the same issuance rules as any grant, and retirement stamps
`retired_at_ms` on that row before it closes the scope
(`HostGrantStore::retire_scope`). The runtime role holds `SELECT` on the
ledger and nothing else, so PostgreSQL refuses either on a runtime handle
before anything is changed, and the store reports `OwnerPhaseRequired`. A
retirement that finds no generation left has stamped the ledger and closes
the scope as exhausted.

The owner phase refuses a schema with a relation the matrix does not classify
and a runtime role that already holds a privilege beyond it.
`HostGrantStore::verify_runtime_privileges` reads the same profile back on a
runtime handle: a restricted login role with exactly these privileges and no
trigger or rule on the relations.

A PostgreSQL replica runs that check when it starts, before it builds the
state that serves connections, and keeps the result as a
`HostGrantAttachment`. An acceptance transaction admits a room that a scope
manages only on a replica that holds one. A replica whose runtime role does
not match the profile, or that could not read it, therefore treats every
enrolled scope as unavailable: a groupchat to a managed room is refused with
a committed `service-unavailable`, rooms that no scope manages are
unaffected, and the failure is logged at error level by its kind, without an
identifier. The attachment also carries the account domain the replica
serves, which is what admission honors privileged grants for (B2). Later
privilege changes by the owner are not observed until the next start.

Not established by this slice:

- A holder of the runtime credential can still write any grant row with its
  own SQL. The runtime is the authority's writer; the store's invariants are
  those of `HostGrantStore`, not of the database. Such a holder can also
  close a scope by writing its state row; it cannot stamp the ledger.
- A replica whose runtime role is the schema owner fails the check, so it
  serves no enrolled scope. Enrolled scopes need the restricted runtime.
- The attested runtime's pinned catalog profile does not cover these
  relations; they are verified by the store's own check only.
- A binary built before this slice does not know the relations. It neither
  reads nor writes them, and it admits a managed room as an unmanaged one.
  Retiring such binaries before any scope is enrolled is part of the rollout
  and activation runbooks (G24, G22).

SpiceDB. A deployment has one preshared key and SpiceDB has no privilege
narrower than the key, so there is no credential that could write the marker
but not a grant. The boundary is therefore built from three properties:

- In the server, the only writer of an enrolled scope's affiliation family is
  `WriteHostGrantProjection`. The legacy tuple messages (`WriteTuple`,
  `DeleteTuple`, `ReplaceExclusiveRelation`, `SwapExclusiveRelation`) refuse
  a tuple whose object, or whose Space or channel subject, is the channel of a
  managed room or the Space of an enrolled scope. The refusal is read from
  PostgreSQL on every such write, so it holds after a restart and on every
  replica, and a store that cannot be read refuses the write. The lookup
  waits at most 500 ms for a lock and runs at most 1 s, so a stalled
  database refuses the write instead of holding the permission actor. A
  retired scope keeps its objects closed to legacy writes.
- On the wire, every projection write is conditioned on the scope's marker
  (B3), so a writer that holds the key and an older intent cannot apply it.
- Admission in this slice never reads SpiceDB, so a relationship written by
  any other holder of the key admits nothing through the fresh-manifest path.
  Such a relationship is found when a projection is verified, and then keeps
  the pending increase from being activated.

A key holder that does not carry the guard (an older binary, an operator
tool) can still write relationships beside the projection. This slice does
not exclude it; the writer inventory and old-writer retirement do (W5c, G24,
G22). The surfaces that still decide from SpiceDB are not fenced by this
slice either.

For the writer inventory (W5c): two writers reach the legacy tuple mirror
without passing the permission actor, and so without its refusal for an
enrolled scope. `persist_durable_group_dm_member_tuple` and
`delete_durable_group_dm_member_tuple` (`admin/channels.rs`) insert and
delete `permission_tuples` rows with their own SQL, keyed by a group-DM
identifier as the channel object. Neither is reachable for a managed room
today; both need the same treatment as the actor's tuple messages before a
scope can share an identifier space with group DMs.

### B4: lock order, atomic admission, cancellation and lost commits (G12)

The scope's guard is the row lock on its `host_grant_scopes` row. A store
transaction (mutation, retirement, activation) takes it `FOR UPDATE`; an
acceptance transaction takes it `FOR SHARE`.

The acquisition order of the fresh-manifest acceptance transaction
(`ingress/commit/implementation.rs`), as audited:

1. the ingress protocol epoch row, shared, when the transaction begins;
2. the principal's session row, shared;
3. for a relayed stanza, the room's ownership claim, shared;
4. the room owner's mailbox lane row;
5. the stream-management stream row, then the origin's mailbox lane row;
6. the origin alias and the canonical message row;
7. for a locally owned clustered room, the room's ownership claim, shared;
8. the durable room row of the manifest's coordinates, shared, without waiting;
9. **the scope guard, shared**, and under it the room binding, the authority
   incarnation and the room's grant rows, read without row locks;
10. every row the acceptance records: effect intents, receipts, archive rows,
    the sealed manifest, the mailbox decision and the stream position.

The guard is taken after every ownership claim of the transaction and before
the first row that records what is accepted. The rows of steps 4 to 6 are
ingress identity rows that the existing path locks before its later room
checks, and the canonical message row of a new stanza is inserted there; the
guard does not change their order, and a refusal decided at step 9 commits
that row as a denial.

The order of a store transaction: the protocol epoch row, shared (the
guarded-store entry takes it, so an epoch this binary does not implement
refuses the write); the scope guard, exclusive; then host-grant rows only.
A store transaction takes no ownership claim and no ingress row, and it
performs no RPC, actor call, queue wait or socket write.

No cycle exists between the two: a transaction that holds a guard
exclusively waits for nothing an acceptance transaction can hold except the
guard itself and the epoch row, which both take shared. Two store
transactions of one scope serialize on its guard, and no operation of this
slice takes two guards. A later operation that needs several guards takes
them in ascending lifecycle identity, and a later surface that takes the
guard takes it after its claims; an inner transaction that takes a claim is
never started while a guard is held.

Waits are bounded. The acceptance transaction's lock wait is its existing
bound (100 ms): a scope whose guard is held exclusively fails that acceptance
with the `Timeout` class, nothing is accepted, and other scopes and unmanaged
rooms are unaffected. A store transaction waits at most 3 s for a lock and
5 s per statement and then returns `Busy` with nothing changed.

Known limit: PostgreSQL does not queue share lockers behind a waiting
exclusive locker of a row. A continuous overlap of acceptance transactions in
one scope can therefore make a mutation of that scope time out repeatedly. It
never acknowledges a revoke that did not commit, but it can delay one. This
slice does not change the guard. Whether it needs a fair primitive is a
decision of the revocation slice (W5d), which defines what a revoke's
acknowledgement promises and by when.

Atomic admission. The decision and what it admits are one transaction: the
policy state is read under the guard, the manifest is sealed and its children
recorded, and the guard is released by the same commit. Therefore:

- a revoke that committed before the acceptance took the guard is seen, and
  the acceptance is refused; a revoke that starts later waits until the
  acceptance has committed or rolled back;
- a refusal is a committed denial with a stanza error, as for any other
  committed denial: it owns the stanza's origin identity and a retransmission
  resolves to it, whatever has changed since. No refusal is therefore of
  type `wait`;
- a retransmission of an accepted stanza replays the acceptance and is not
  decided again, so a later revoke does not erase accepted children and a
  later grant does not add any;
- the plan is reduced to the admitted audience before anything is recorded
  (B2), and after sealing the manifest is checked against that audience
  again: a child that targets anyone outside it, or a recorded intent the
  reduced plan could not hold, fails the transaction.

A managed room accepts nothing through an acceptance that seals no manifest:
a transaction that cannot seal one (a clustered deployment below the mailbox
epoch, or a plan without a manifest snapshot) is refused for a managed room.

Cancellation and lost commits. Each store transaction runs on a task of its
own, so a caller that stops waiting does not abandon it; it resolves and is
found by its operation identity. A commit that is not acknowledged returns
`CommitOutcomeUnknown`, never a rollback, and so does a task that ended
without reporting, which may have committed before it did: the caller reads
the operation back (`operation_outcome`), which answers `Committed` or
`Absent` from the primary, and a failed readback leaves it unknown. Retrying
the same command is equally safe.

An enrollment has no guard to wait on before its first insert, so two
submissions of one enrollment meet at a unique key instead. The one that
loses reads the journal again after the other committed: an exact retry
returns the recorded outcome, and the same identity with other input is a
conflict, whichever submission reached the key first. The acceptance transaction keeps its existing rule: an
unacknowledged commit returns no decision, so no external effect can execute,
and recovery works from the canonical rows.

### B3: marker protocol for this slice's projection writes (G15, part)

Schema version 2 adds the definition `waddle_grant_projection`, which holds
one marker relationship per scope lifecycle and confers no permission:

```
waddle_grant_projection:<lifecycle>#marker@waddle_grant_projection:<stamp>
```

The stamp is `<incarnation>_<generation>_<operation>_<digest>`: the authority
incarnation, the scope generation, the operation that recorded it and the
SHA-256 of the intended projection (the policy version, the lifecycle and
every relationship, in canonical order). A marker has one spelling; anything
else in the definition for that lifecycle, or more than one relationship, is
malformed.

A projection write is one `WriteRelationships` request that states the whole
affiliation family of the scope: for every grant row, including revoked
subjects, the relation of its affiliation is touched and the other three are
deleted; the previous marker is deleted and the next one touched. Its
preconditions:

- replacing a marker: that exact marker exists;
- the first write of a lifecycle: the lifecycle holds no marker, and the
  affiliation family of every managed room holds no relationship. This is
  the slice's evidence of a fresh namespace. A scope whose rooms already hold
  affiliation relationships is never opened.

A write is refused before any RPC if the marker it would replace is not of an
older generation, or belongs to another lifecycle or incarnation, or if the
intent names a room or one subject's grant in a room more than once: a
request states each relationship in exactly one update, and SpiceDB rejects
one that does not, which would read as an unknown outcome. A write whose
precondition fails writes nothing, and its writer stops: there is no call
that writes an intent against whatever marker is present. Any other failure
leaves the outcome unknown.

The projection RPCs do not run in the permission actor's mailbox turn. The
actor hands its SpiceDB backend to a task of the caller's request, so the
33 reads of one observation and the write do not hold up permission checks
and tuple writes queued behind them. Each RPC has a 5 s deadline and a whole
observation 20 s.

Reconciliation (`HostGrantAuthority::reconcile`) is the only path to
activation and is also recovery; it keeps no state between passes. A pass
reads the scope's current intent from PostgreSQL in one snapshot, reads the
marker, and decides:

| Marker | Pass |
| --- | --- |
| The current operation's | The write was applied, whether or not its reply arrived. Verify and activate. |
| The journal's marker for an older generation | Write the current intent conditioned on exactly that marker, verify, activate. |
| None, and no projection of this lifecycle was ever activated | Write the current intent into the fresh namespace, verify, activate. |
| None, after an activation | Contradiction: nothing is written. |
| Malformed, another incarnation, a generation that is not older, or an older generation the journal does not record | Contradiction: nothing is written. |

A pass reads PostgreSQL before SpiceDB, so another replica can record and
project a newer operation between the two reads. A marker that is not older
than the pass's plan is therefore compared with the canonical state again:
it is a contradiction only if that state is still the one the pass read,
and otherwise the pass is superseded and the next one starts over.

Verification reads the marker fully consistently and the family at exactly
the marker's revision, and compares the family with the canonical rows
exactly; a relationship the projection never writes, or a missing one, is a
contradiction. A successful comparison yields a `VerifiedProjection`
witness. Activation (`HostGrantStore::activate`) is crate-private and takes
that witness and nothing else, and only verification constructs one, so no
caller can make a recorded increase effective without a projection that was
read back. Test fixtures that exercise the canonical store without SpiceDB
use `test_support::activate_host_grant_marker`. Activation compares
lifecycle, incarnation, generation, operation and policy version under the
guard, and changes nothing for an operation that is no longer current.

A recovery pass (`HostGrantAuthority::recover_pending`) follows the pending
scopes page by page, in lifecycle order, to the end. A scope that cannot
progress (a contradiction, a projection that does not answer) is visited
once per pass and never keeps a scope behind it from being reconciled.

A contradiction or an unknown outcome never changes the grants: the
recorded increase stays ineffective, effective grants keep admitting, and a
revoke still takes effect at its commit. Nothing in the projection can make a
revoked grant effective, because admission does not read it. The restore
slice closes the scope on the three contradictions that show the database
lost operations (`AheadOfCanonical`, `UnknownMarker`, and a
`ForeignIncarnation` the journal never held); see
[The fence a replica passes](#the-fence-a-replica-passes).

Not decided here: repair of a contradiction, projections larger than one
request, and any admission that reads the projection. Reconstruction of a
projection after a restore is decided by the restore slice (see
[Verified reconstruction](#verified-reconstruction-g15-part)). A retired or exhausted scope has no
projection plan; its relationships and marker stay as they were.

### B2: policy for the action this slice protects (G14, part)

The protected action is the acceptance of a fresh groupchat in a managed
room. The policy is a table over the effective affiliation
(`permissions/host_grant/policy.rs`), mirroring what the authorization schema
grants the four affiliation relations, without the parent Space:

| Effective affiliation | Allowed |
| --- | --- |
| `owner`, `admin` | every action except channel `delete` |
| `member` | `view`, `read`, `send_message`, and the author's own correction and retraction |
| `none`, `outcast` | nothing |

- The sender needs `send_message`, or the stanza is refused. The sender is
  the principal the acceptance is made for: the account of an authenticated
  stream, the actor of an extension, or the room itself for a room's own
  broadcast. An extension actor or a room holds an affiliation only if one
  is granted, so both are refused in a managed room.
- `outcast` wins for every action. A subject holds one affiliation per room,
  and admission also refuses a subject whose recorded affiliation is
  `outcast` whatever its effective column holds.
- A correction or a retraction of one's own message is a send, so an outcast
  author loses both, and so does any subject that cannot send.
- No explicit affiliation confers channel `delete`.
- Grants held by other stores (legacy tuples, durable room affiliations,
  Space membership, the room actor's occupancy) admit nothing.

The audience of a managed room is its admitted subjects and nobody else.
The planner still plans a groupchat for whoever the room actor and the
legacy stores name: seated occupants, accounts with a durable room
affiliation, members of the parent Space and mentioned accounts. The
acceptance transaction reduces that plan under the guard, before it records
anything (`ingress/host_grant_plan.rs`):

- Every principal the plan names needs `read`: the target of a route, the
  owner of an inbox row and of its push, the owner of a notification and the
  recipient of its recovery item, whether the planner named them as an
  occupant, a durable member or a mentioned account.
- A principal that does not hold `read` is dropped: its intents and the
  effects that would carry them out are removed from the plan, so no child
  is frozen for it, no inbox row, push, notification or recovery item is
  written for it, and nothing is recorded that a later replay or recovery
  could deliver to it. It does not refuse the stanza. A member whose grant
  was revoked while the room still seats it therefore receives nothing, and
  the other members receive the message.
- Work of the room itself that names no principal is kept: its archive
  entry, a retraction tombstone and its replay scrub, link-preview
  references.
- Classification is by allow-list, and the match over intent and effect
  variants is exhaustive in the source. Work that is neither of the above
  refuses the whole stanza: direct-message work, carbons, invitations,
  offline queues, call signalling, a relay to a remote room owner, and
  changes of room state (subject, pins, system messages), which this policy
  does not decide. A new variant does not compile until it is classified.
- An extension that observes a room's messages reads them. An observer
  holds no grant, so its observation is dropped from a managed room's
  acceptance like any other unadmitted reader.
- The planner is not filtered. Filtering it would save planning work only;
  the acceptance is the boundary. Applying the binding to the room actor's
  configuration, and fencing the planner's sources, belong to the
  writer-fencing slice (W5c).

What a managed room does not accept, as a consequence: occupant private
messages (planned without a manifest, and not a copy of a groupchat), the
room's own system messages and groupchat sent by an extension bot (their
sender holds no grant), subject changes, pins and call signalling. Mediated
invitations and declines are not planned as a room acceptance and keep
their existing checks, like every other surface this slice does not fence.

Refusals: `forbidden` (type `auth`) when the sender is not admitted;
`service-unavailable` (type `cancel`) when the scope admits nothing, when
the replica holds no verified attachment to the grant store (B1), when no
manifest can be sealed, when the plan holds work the allow-list does not
admit, or when the room's address is not in canonical form. A sender who is
not admitted is told only that. A refusal is a recorded outcome that a
retransmission resolves to for good, so none is of type `wait`.

Remote privilege at this boundary (ADR-0020 D8). Honoring a privileged or
moderator grant of a remote principal requires its home to be trusted and
the stream it arrived on to have `pkix` strength, and neither is visible
here. Until the effect-boundary slice (W5e) supplies them, this boundary
fails closed: an `owner` or `admin` grant, the only privileged or moderator
affiliations this slice holds, whose subject is not an account of a domain
this deployment serves is honored as no grant
(`policy::honoured_affiliation`). At admission it yields neither `moderate`
nor the `read` and `send_message` the table would derive from it, for the
sender and for every named principal; as a mutation's initiator it
authorizes nothing; and it does not count as the owner a room must keep.
Member-level and negative grants of remote accounts are honored as stored.
The served account domains are the replica's configured account domain:
admission takes them from its attachment (B1) and a mutation from its
issuance policy. A replica that serves no account domain honors no
privileged grant. Both sets must come from one configuration value: issuance
records an owner or admin grant as `local` because its subject's domain is
served, and admission honors it only if it agrees. No issuance policy is
built from configuration in this slice; when one is, it takes the account
domains from the value the attachment takes them from. W5e replaces this rule with the trusted-home and `pkix`
evaluation. Issuance is unchanged by it.

A mutation's initiator is an account whose authority is evaluated from the
state under the guard, in the transaction that records the change: only an
effective owner grants or removes owner and admin standing, an effective
owner or admin changes other standing, and a room always keeps an owner who
can administer it: one whose standing is recorded, effective and honored. A
standing that was revoked, demoted or not yet activated authorizes nothing,
nor does a privileged standing of a remote account (above), and a command
decided under an earlier generation is refused as stale first. An
enrollment is refused unless every room starts with an owner of a served
account domain.

Issuance to remote accounts (ADR-0020 D8). `IssuancePolicy` takes the domains
the deployment serves and the trusted-home set as inputs. An owner or admin
affiliation for an account whose domain is neither served here nor an exact
member of that set is refused before anything is recorded; member-level and
negative affiliations are issued to any account; an identity of a local
service is refused as a subject. The accepted basis (`local`, `trusted_home`,
`unprivileged`) is recorded with the grant. A grant recorded for a trusted
home is not honored by this slice's admission or as an initiator, as
described above; whether it is honored on a `pkix` stream is decided by
W5e.

### B6 as it applies to grant subjects

A grant subject is `GrantSubject`: a bare account JID that the authorization
ID codec's `AuthzUserJid` accepts, that is, in the identity profile's
canonical form and within the codec's limits. A full JID and a JID without a
localpart are refused rather than reduced. The SQL grant key and the
projected subject are that canonical text.

### Rollout part (G24)

- Migration 2001 is the first migration after the schema baseline. Under the
  restricted runtime the owner phase applies it and grants its relations
  from the matrix, and the runtime's ledger preflight refuses a database
  whose ledger lacks it; a PostgreSQL server that owns its schema applies it
  at startup. A server of the previous release refuses a ledger that holds a
  version it does not know. The owner phase therefore runs with the new
  image before any new server starts, and old and new servers do not share
  the database across it. On SQLite the migration creates nothing.
- The schema tooling that compares a history-built database with a
  baseline-built one (`scripts/schema_history.py`) does not yet account for
  relations added after the baseline.
- The built-in authorization schema is version 2. Its upgrade from version 1
  follows [the schema version runbook](../../../docs/operations/authz-schema-versions.md).
- Every legacy tuple write now reads PostgreSQL once for each channel or
  Space object it names, in a transaction of its own with a 500 ms lock and
  1 s statement bound. On SQLite the permission actor carries no store and
  nothing changes.
- With no scope enrolled, acceptance of a room message adds one indexed
  lookup on PostgreSQL. It changes one outcome: the lookup is made by the
  room's canonical address, and a room address that is not in the identity
  profile's canonical form (a rooted domain, for example) is refused with
  `service-unavailable` instead of being accepted as a room no scope
  manages. The identity profile already refuses such addresses where
  stanzas are parsed.
- A PostgreSQL replica checks its runtime role's privileges on the
  host-grant relations when it starts. A failure does not stop the
  replica, which serves no enrolled scope until the check passes. A
  failure that a later attempt may pass (a failed statement, an expired
  lock or statement bound, bounds that could not be installed) is retried
  in the background with a backoff capped at one minute; a role that does
  not match the profile is not retried. Each failure is logged by its kind,
  as an error when the database holds a scope that is enrolled and not
  retired or when that cannot be read, and otherwise as information. A
  deployment whose runtime role owns the schema therefore logs the mismatch
  at every start, as an error once a scope is enrolled, and must move to
  the restricted runtime before it enrolls one. While a replica is
  unattached, each refused acceptance for a managed room is also logged
  as a warning by its kind, at most once a minute.
- The owner phase is what retires a scope as well as what enrolls one. The
  runtime role's grant matrix is unchanged by that: it reads the lifecycle
  ledger and cannot write it.

### Running the first slice's focused set

From `server/`, against a disposable PostgreSQL 17 cluster and a disposable
SpiceDB:

```sh
export WADDLE_TEST_POSTGRES_URL=postgres://USER@HOST:PORT/DATABASE
export WADDLE_TEST_REQUIRE_POSTGRES=1
export WADDLE_TEST_SPICEDB_ENDPOINT=http://127.0.0.1:50051
export WADDLE_TEST_SPICEDB_PRESHARED_KEY=KEY
export WADDLE_TEST_SPICEDB_DISPOSABLE=1
cargo nextest run -p waddle-server --features test-support --locked \
    --run-ignored all \
    -E 'binary(host_grant_authority) | binary(xep0045_host_grant_admission) | binary(xep0045_host_grant_audience) | binary(host_grant_projection) | binary(authz_schema_version) | binary(channel_outcast_precedence) | binary(authz_codec_incarnation) | binary(authz_id_codec)'
```

The PostgreSQL login must be a superuser. Each case creates and drops its
own database, and one case of `host_grant_authority` creates a role, grants
it privileges in that database and connects as it; since PostgreSQL 16 a
login that holds `CREATEROLE` without being a superuser cannot arrange that
for a database it does not own. The SpiceDB is emptied by every
`live_spicedb_` case. `host_grant_authority`,
`xep0045_host_grant_admission` and `xep0045_host_grant_audience` need
PostgreSQL only; `host_grant_projection` and the `live_spicedb_` cases of
`authz_schema_version` need both stores. `xep0045_host_grant_audience`
plans its messages with the production room planner against a real room
actor; the other admission cases build their plans by hand.

The focused set was executed on 2026-11-14 against PostgreSQL 17.11 and
SpiceDB 1.56.1 with nextest's default parallelism: two consecutive runs
passed 110 of 110 cases. An earlier run of the same revision failed one
admission case with an ingress substrate timeout; the case passed alone and
in both later runs. An expired bound under parallel load is the likely
cause, but it has not been confirmed. That SpiceDB used the memory
datastore, so the `live_spicedb_` and `host_grant_projection` cases are
evidence against SpiceDB's API and consistency semantics, not against a
PostgreSQL-backed SpiceDB datastore.

## Restore incarnation decisions

These are the decisions the restore slice (W5r in the plan) made for B7
(ADR-0020 gate G13) and for bounded reconstruction, its part of B3 (G15),
over the first slice's scope. They are decisions; the W5r rows of the
[acceptance matrix](#required-native-acceptance-matrix) are their evidence,
and the run recorded under
[Running the restore slice's cases](#running-the-restore-slices-cases) is
the only part of it that has run. They change the first slice's decisions
only where stated. The operator procedure is the
[restore runbook](../../../docs/operations/authz-host-grant-restore.md).

### Where the incarnation lives (G13)

A restore brings the incarnation back together with the grants it governs,
so the incarnation stored in PostgreSQL cannot by itself tell a restored
database from the live one. It is bound to what a restore of the database
does not bring back:

1. **The declared restore epoch.** `WADDLE_HOST_GRANT_RESTORE_EPOCH`, an
   unsigned integer in the deployment configuration (`0` when unset), read
   by the owner phase and by every replica. The operator raises it once for
   each restore; it is never lowered. `host_grant_authority.restore_epoch`
   records the epoch the current incarnation was established under.
2. **The database lineage.** `host_grant_authority.lineage_uuid` binds the
   incarnation to `_lineage`. A logical restore into another database is
   adopted, which rotates the lineage; the authority then no longer matches
   it. A physical copy (a base backup, a volume snapshot, a promoted
   replica) keeps the lineage row, the system identifier and the OIDs
   ([lineage runbook](../operations/db-lineage.md), "Physical-clone
   limitation"), so the lineage does not detect a physical restore, in
   place or into a new cluster.
3. **SpiceDB witnesses.** SpiceDB is not restored with PostgreSQL, so what it
   holds about the canonical store survives a restore of the database. Six
   kinds of record, all in the projection's definition and relation
   (`waddle_grant_projection#marker`), each under a resource ID no
   lifecycle identity can take (`permissions/host_grant/witness`):
   - the **projection marker** of each scope (`<lifecycle>`): the operation
     whose projection was last written;
   - the **journal head** of each scope (`head_<lifecycle>`): the scope's
     latest recorded operation, in the marker's spelling. It advances while
     the projection is stuck;
   - the **room bindings** of each projected scope
     (`room_<SHA-256 of the room's channel object>`, naming the lifecycle),
     written in the projection's own request;
   - the **PostgreSQL position** of the authority
     (`position_<authority_id>`): the declared epoch, the system identifier,
     the timeline (from `pg_walfile_name(pg_current_wal_flush_lsn())`) and
     the flushed WAL position (`pg_current_wal_flush_lsn()`). On the
     physically attested pool the system identifier is the attested
     physical identity's, which the attestor compares with the live one in
     the same transaction; the restricted runtime role may not call
     `pg_control_system()` (the attestor requires it not to). On any other
     handle it is read from `pg_control_system()`. `authority_id` is a fixed
     identity migration 2002 mints and nothing changes, so a restored copy
     finds the witness of the database it was copied from;
   - the **regression latch** of the authority (`latch_<authority_id>`):
     the position witness a regression was seen against, and whether it
     was on the witnessed timeline (`timeline`) or below the witness on
     another one (`branch`);
   - the **witnessed epoch** of the authority (`epoch_<authority_id>`): the
     declared epoch the position witness was last written under, written in
     the same request as every position witness. A position witness or
     latch that cannot be read carries no epoch of its own; this one tells
     whether a restore was declared since it was written.

No schema change is needed for the witnesses: they reuse the marker
definition and relation, so the built-in schema stays at version 2 and a
SpiceDB store that already holds it serves them. A synchronous witness
written with every revoke (in the revoke's commit path) was rejected: it
would put another store on revocation's path, which D5 keeps in PostgreSQL
alone. Every witness is written after the commit it describes, never inside
it; the commit never waits for SpiceDB.

#### What each event is caught by

"Pass" is a pass of the restore fence: when a replica attaches, when an
acceptance finds the binding changed, and about every five minutes
(300 s plus up to 30 s of jitter) while it is attached. "Witnessed" means
written to SpiceDB before the restore point. The tests named are in
`tests/host_grant_restore.rs` unless stated.

| Event | Caught by | Not caught when | Evidence |
| --- | --- | --- | --- |
| Declared restore (epoch raised) | The owner phase's transition: new incarnation, every scope that is not retired or exhausted closed as `restored` at its next generation, effective standing lowered to `none`, any position acknowledgement cleared. | — | `a_declared_restore_closes_every_scope_under_a_new_incarnation`, `a_transition_that_stopped_is_finished_by_the_next_owner_phase` |
| Replica configured with another epoch than the recorded one | `epoch_mismatch` at every pass. Configured below the recorded epoch, the replica is not ready while an unretired scope is enrolled (`configuration_predates_restore`); configured above it, it stays ready, refuses managed rooms and names `configuration_ahead_of_restore`. | — | `a_replica_verifies_a_changed_authority_in_place`, `live_spicedb_a_restore_to_before_a_transition_is_refused` |
| Logical restore into another database (adopted lineage) | `lineage_changed` at every pass and in the owner phase, while the database holds a scope or SpiceDB names one. The binding's database OID differs, so a running replica's next managed acceptance starts a pass. | The database holds no scope and SpiceDB names none: there is nothing to protect, and the owner phase binds the new lineage. | `an_adopted_database_is_not_served_until_its_restore_is_declared`, `a_database_without_a_scope_is_not_refused`, `a_refused_authority_keeps_the_replica_ready_and_refuses_managed_rooms` |
| Logical restore that recreates the relations in the same database, under running replicas | The binding's authority-relation OID differs: the next managed acceptance is refused and starts a pass, which closes the scopes the rows below catch. | As for those rows. Until a managed acceptance or a periodic pass, nothing is compared. | `a_replica_verifies_a_changed_authority_in_place` |
| Rows rewritten in place under running replicas, no restart and no new relation | The next periodic pass, on the scope evidence below. | As for those rows; the window is up to 330 s. | `live_spicedb_a_restore_under_a_running_replica_is_found_without_a_restart` |
| Undeclared restore that lost projected operations | A marker of the scope's incarnation the journal does not record: `projection_ahead`, the scope is `fenced` and marked as having lost operations. Reconciliation closes it on the same evidence (`AheadOfCanonical`, `UnknownMarker`). | SpiceDB went back to a point before that projection as well. | `live_spicedb_a_restore_cannot_reopen_a_projected_revoke`, `live_spicedb_reconciliation_closes_a_scope_whose_marker_is_ahead` |
| Undeclared restore that lost operations never projected | A journal witness the journal does not record: `journal_ahead`, `fenced`, marked as having lost operations. | The lost operation was not witnessed: its journal witness is written by a task after the commit (a mutation through `HostGrantAuthority`), and no later operation's witness or position witness passed it either. | `live_spicedb_the_journal_witness_finds_a_lost_revoke_the_projection_never_saw` |
| Declared restore of a scope that was never projected and whose journal witness the journal does not record | Its first projection (reconciliation) reads the journal witness, marks the scope as having lost operations under a `reconstruct` operation before the write, and moves the witness with the projection, which names that operation, in one request; a reopen of every scope leaves it closed. | As the row above. | `live_spicedb_a_restored_scope_never_projected_keeps_its_lost_operations` |
| A second restore during restore handling, to after the declaration and before the lost-operations mark | The mark is recorded with a `reconstruct` operation at the next generation, and the projection and journal witness then name that operation, which the restored journal does not hold. Declared, the reconstruction finds the marker unrecorded and marks the scope again; acknowledged instead (no new incarnation), the marker is ahead of the restored journal and the pass fences the scope, marked. A reopen of every scope leaves it closed either way. | The mark's projection was never written (the takeover after it failed) and SpiceDB still holds the earlier marker and witness: the next reconstruction reads the same evidence and marks the scope again. | `live_spicedb_a_second_declared_restore_keeps_the_lost_operations_mark`, `live_spicedb_a_second_acknowledged_restore_keeps_the_scope_closed` |
| Journal witness of a restored scope that cannot be read | Reconciliation marks the scope as having lost operations under a `reconstruct` operation and stops (`MalformedJournalWitness`), without moving the witness or taking the projection over; the scope stays `restored` until an operator removes the witness, and is then reconstructed with the mark. | — | `live_spicedb_an_unreadable_journal_witness_keeps_a_restored_scope_closed` |
| Undeclared restore that lost an owner-phase act (a retirement, an enrollment, a reopen, an operator revoke) | Its journal witness (`journal_ahead`). `waddle-server host-grant` writes it at once when it is given the servers' SpiceDB; otherwise the next pass that passes writes every journal witness that differs from its journal head, retired and exhausted scopes included. | It was not witnessed: no SpiceDB was given to the act and no pass passed before the restore point (up to 330 s with a replica attached; unbounded while every replica's fence refuses). | `live_spicedb_a_lost_retirement_is_found_by_its_journal_witness` |
| Undeclared restore that lost an enrollment | Its room witnesses name a lifecycle the store does not hold: the pass reads that set before it decides anything about the whole authority, and the replica refuses those rooms (`service-unavailable`) whether or not it is attached. Until a pass has read the set, the gate reads the room's witness from SpiceDB (500 ms bound; a witness is remembered, its absence for 60 s; after a failed read, no room is asked about for a minute). | The enrollment's projection was never written (no room witness); or SpiceDB does not answer while the set is unknown, and the room is served as unmanaged (accepted residual). A replica running when the database is restored keeps the set its last pass read until its next pass that reads SpiceDB: at most one periodic pass (330 s) for a restore made under it, sooner when an acceptance for a managed room finds the binding changed and starts a pass, or at the next retry while the authority is refused; a gate that stopped verifying (a runtime role that does not match the profile) reads the set once every interval as well; nothing reads it while SpiceDB does not answer. Meanwhile it serves the lost enrollment's rooms as unmanaged. | `live_spicedb_a_lost_enrollment_refuses_only_its_own_rooms` |
| Physical restore in place, or into another cluster, to an earlier point on the same timeline | The position witness is ahead of the server: `position_regressed`, latched (`timeline`) in SpiceDB and in the replica's memory, so it stays refused after the server writes past the witness again. Cleared by a declared restore, or by an acknowledgement bound to that witness. Every write that moves the position witness is conditioned on the latch it read, or on there being none. | No position witness was written after the restore point (it moves after a committed mutation and after every pass that passes); the restored server's flushed position passed the witnessed one before any pass compared them, so the regression was never seen; SpiceDB went back as well. A latch SpiceDB did not take (each way the write fails is counted) is the replica's alone, and every later pass of it writes it again; until one does, it is lost when the replica restarts, and stops counting once a writer that does not hold it (another replica's pass, a committed mutation, an operator's act) moves the witness, which it does as soon as the server has written past the witnessed position. | `live_spicedb_the_position_witness_refuses_a_rolled_back_database` |
| Recovery to a point in time, or promotion, onto a new timeline | Another timeline of the same system: `position_unverified` until an operator acknowledges that exact witness for the current, later, timeline; below the witnessed position, `position_regressed`, latched (`branch`), cleared only by a declared restore, and `acknowledge-position` refuses to record an acknowledgement for it. Another system: `position_unverified`, cleared only by a declared restore. | No position witness exists. An acknowledgement is the operator's assertion that the promotion lost nothing; operations whose witnesses were not written are not checked by it. | `live_spicedb_the_position_witness_refuses_a_rolled_back_database`, `the_position_witness_detects_regressions_and_unverified_timelines` |
| Restore to before the latest declared restore | The configured epoch (`epoch_mismatch`); with the configuration rolled back too, a position witness of the later epoch (`position_regressed`); without one, a marker or journal witness of the later incarnation (`unknown_incarnation`, `journal_ahead`). | The configuration was rolled back and no position witness, marker or journal witness of a later incarnation was written. | `live_spicedb_a_restore_to_before_a_transition_is_refused` |
| A journal witness of an incarnation the journal never held, on a reopened scope | `journal_ahead`: a reopen explains only a witness of an incarnation the journal held. | — | `live_spicedb_a_journal_witness_of_an_unknown_incarnation_closes_a_reopened_scope` |
| SpiceDB empty or another deployment's, PostgreSQL intact | Scopes admit and SpiceDB holds no marker or room witness of a lifecycle the store holds, and no position witness, latch or witnessed epoch of this authority (it may hold another deployment's markers): `projection_absent`. Nothing is closed; the refusal clears when the replica reaches the right SpiceDB. While no scope admits the pass passes, and writes no position witness into such a SpiceDB. Reconciliation writes no first projection while a scope admits or was ever projected and SpiceDB holds no position witness, latch or witnessed epoch of this authority and no marker of the first 16 such scopes, those that admit first (`restore::projection_absent`, which errs towards refusing and gives up as busy after 20 s), so the refusal is not cleared by a marker written into the wrong SpiceDB, and after a declared restore closed every scope a restored scope is not projected into a SpiceDB that holds none of the journal witnesses showing what the restore lost. Journal witnesses do not count: a committed mutation writes them wherever its replica points. | — | `live_spicedb_a_spicedb_without_the_projection_refuses_without_closing`, `live_spicedb_another_deployments_spicedb_gets_no_position_witness`, `live_spicedb_no_first_projection_is_written_into_a_spicedb_without_the_authority`, `live_spicedb_no_restored_scope_is_projected_into_a_spicedb_without_the_authority` |
| Position witness or latch that cannot be read, or witnessed epoch that cannot be read with neither of them readable beside it; latch without its position witness | `position_unverified`. Nothing removes them without a declared restore: the first pass under a declared epoch above the witnessed epoch (or above the epoch of the latch or witness that can be read) removes all three, each in a request conditioned on the resource that shows the earlier epoch, which goes last, and only when none that can be read shows the declared epoch or a later one; the pass that follows writes the first witness of the declared epoch. The removal request names one authority's position witness, latch and witnessed epoch only, and the permission backend refuses any other resource. A latch without its witness is cleared the same way once a restore was declared since its witness. Writers leave an unreadable witness or latch alone; an unreadable journal witness is counted and warned about like a failed write. An unreadable witnessed epoch beside a readable witness or latch, which carry their own epoch, is not refused, and the next move of the position witness replaces it (removing it first when it holds more than one relationship). | None of the three shows an epoch (the position witness unreadable, the witnessed epoch missing or unreadable, no readable latch): the refusal outlives the declared restore, and the operator removes the three resources by hand after declaring it (runbook). One that can be read shows the declared epoch: a further declared restore is needed. | `live_spicedb_a_latch_survives_a_failed_write_and_unreadable_witnesses_need_a_restore`, `live_spicedb_unreadable_witnesses_beside_one_of_the_declared_epoch_are_kept`, `live_spicedb_an_unreadable_epoch_beside_a_readable_witness_is_replaced` |
| SpiceDB lost markers, PostgreSQL intact | Beside this authority's position witness or other markers, an admitting scope without a marker is `fenced` (`marker_missing`); it is not marked as having lost operations. | — | `live_spicedb_a_scope_that_admits_without_a_marker_is_closed`, `live_spicedb_a_spicedb_without_the_projection_refuses_without_closing` |
| SpiceDB restored to an older point, PostgreSQL intact | Nothing to catch: a marker of an incarnation the journal holds, an older journal witness and an older position witness are SpiceDB being behind; the witnesses move forward. | — | `live_spicedb_a_projection_older_than_the_database_closes_nothing` |
| PostgreSQL and SpiceDB restored together | The witnesses that survive, as in the rows above. | Both stores went back to points that agree: nothing detects it. The operator procedure (declare, reconstruct, review, reopen) is the control. | untested |

The position read under the restricted runtime profile is
`the_runtime_role_reads_the_host_grant_position_on_its_attested_pool` in
`tests/restricted_runtime_startup.rs`.

A database that holds no scope, beside a projection that names none, has no
authority a restore could reopen. The fence and the owner phase do not
refuse it for its lineage or epoch, and the owner phase binds its new
lineage.

### Declaring a restore

The owner phase (`prepare_restricted_runtime_owner`, after the migrations and
before the runtime grants) calls `HostGrantStore::prepare_restore_incarnation`
with the configured epoch.

- Configured epoch below the recorded one: refused (`epoch_mismatch`),
  nothing changed.
- Equal: the lineage is bound if it is not yet (the first owner phase after
  migration 2002). A database that holds a scope and whose lineage differs
  from the bound one is refused (`lineage_changed`). A transition an earlier
  run did not finish is finished (`Resumed`).
- Above: a restore transition. One transaction takes the authority row
  `FOR UPDATE`, mints a new incarnation and records it with the epoch and
  the live lineage, and clears any position acknowledgement. From that
  commit no scope bound to the earlier incarnation admits, mutates or
  activates. Then transactions of at most `RESTORE_TRANSITION_BATCH` (256)
  scopes each take the scope state rows `FOR UPDATE` in lifecycle order and
  bind every scope that is not retired or exhausted, whatever else it was
  (`fenced` and `reconstructed` included), to the new incarnation at its
  next generation under a `restore` operation, closed as `restored`; one
  with no generation left is closed as exhausted. Retired and exhausted
  scopes keep their incarnation.

The transition keeps the recorded affiliations and lowers every effective
standing above `none` to `none`: no standing the restored rows hold
authorizes an initiator, a revoke included, until the scope is reopened.
Activation of the reconstructed projection makes the recorded grants
effective again. A transition is resumable rather than one transaction, so
its cost per transaction is bounded; while it is unfinished, the scopes it
has not reached are bound to an incarnation that is no longer current, which
admission, mutation and activation already refuse.

Locking the authority row needs `UPDATE` on it, which the runtime role does
not hold (B1's grant matrix is unchanged), so a runtime handle is refused
with `OwnerPhaseRequired` at the first statement.

### The fence a replica passes

`HostGrantAttachment::verify` checks the runtime role's privileges and then
runs `verify_restore_fence`; a replica's acceptances admit a managed room
only through the attachment it produces. The fence is bound to the
attachment: a pass reads the authority binding in its first snapshot, then
every marker and witness (one streamed, fully consistent read with its own
60 s deadline), then every scope the store holds (one statement), then the
PostgreSQL position and the evidence of each scope SpiceDB names, and
finally the binding again. The attachment is built only from a pass whose
two bindings are equal; otherwise the pass is repeated (three times, then
`authority_moved`). The binding is the incarnation, the epoch, the
PostgreSQL server's start (`pg_postmaster_start_time()`), the OID and name
of the current database, and the OID of the authority relation, which a
logical restore that recreates the relations changes without a restart. The
PostgreSQL reads and fence closures of a pass draw on one budget of 60 s
plus 100 ms for each scope it compares, which the SpiceDB read does not
draw on; a step that overruns it is `busy` and the pass is retried.

From the listing and the store's scopes the pass first takes the
lifecycles SpiceDB names that the store does not hold (lost enrollments)
and the rooms their room witnesses name, and publishes that set to the
replica's gate before it decides anything about the whole authority. An
acceptance for such a room, which the store reports as unmanaged, is
refused with `service-unavailable` whether or not the replica is attached.
Other rooms are unaffected. Until a replica's first pass has read the set,
its gate reads the witness of each unmanaged room an acceptance names
(500 ms; a witness is remembered, its absence for 60 s); after a read that
fails it serves such rooms as unmanaged without asking for a minute. Once a
pass has read the set, the replica refuses exactly that set until its next
pass that reads SpiceDB: a lost enrollment under a running replica is
refused from that pass on (see the table above).

What the fence refuses is the whole authority, and only when the evidence
concerns the whole authority: `lineage_changed`, `epoch_mismatch`,
`projection_absent`, `position_regressed`, `position_unverified`,
`position_unreadable` and `authority_moved`.

- `projection_absent`: scopes admit, so their markers were written, and
  SpiceDB holds no marker or room witness of a lifecycle the store holds
  and no position witness, latch or witnessed epoch of this authority (it
  may hold another deployment's markers). The replica is connected to a
  SpiceDB that is empty or another deployment's; nothing is closed, no
  position witness is written into it, and the refusal clears when it
  reaches the right one. A journal witness does not count: a committed
  mutation writes it into whatever SpiceDB its replica reaches.
- The position: see [Where the incarnation lives](#where-the-incarnation-lives-g13)
  and [Witness writers](#witness-writers). A regression the pass sees is
  latched in memory at once and written to SpiceDB beside the exact
  witness it names, in one conditioned request; the in-memory latch counts
  like a stored one naming the same witness, and every later pass writes
  it again until SpiceDB takes it; each way the write fails (an error, a
  failed condition, no projection backend) is counted. A latch without its
  position witness is `position_unverified`, kept, until a restore is
  declared since the witness it names. A position witness, latch or
  witnessed epoch that cannot be read is `position_unverified` until a
  restore is declared since; the first pass that finds the declared epoch
  above the witnessed one removes them (see the table above).

Evidence about one scope closes that scope and nothing else:

- a marker of the scope's own incarnation the journal does not record
  (`projection_ahead`), a marker of an incarnation the scope's journal never
  held (`unknown_incarnation`), or a journal witness the journal does not
  record (`journal_ahead`) closes it as `fenced` and marks it as one a
  restore lost operations in. A journal witness of another incarnation is
  explained while a restore is handled, and once a declared restore that
  superseded an incarnation the journal held was reopened under the
  current one;
- a scope that admitted when the pass began and has no marker
  (`marker_missing`), when SpiceDB holds a marker or room witness of a
  lifecycle the store holds or this authority's position witness, latch or
  witnessed epoch, is closed as `fenced`: a marker is written before an
  activation and only ever replaced.

A marker of an earlier incarnation the journal holds is SpiceDB being older
than PostgreSQL; it closes nothing, and reconciliation reports it. A scope
closed for a restore (`restored`, `reconstructed`) accepts a marker or
witness of another incarnation, which reconstruction and the reopen
replace. Malformed markers and witnesses are not evidence; reconciliation
reports a malformed marker, and a malformed position witness, latch or
witnessed epoch refuses the authority as `position_unverified` until a
declared restore removes it. The canonical state is
always read after what SpiceDB holds, so an operation another replica
recorded and projected meanwhile is found in the journal, and a scope is
closed only after the evidence is compared again under its guard.

A `fenced` closure is recorded like any mutation: the runtime role locks
the scope's state row, records a `fence` operation at the next generation,
closes the scope and lowers its effective standing to `none` at most. A
fenced scope is not projected again and takes no mutation but an
operator's revoke, so the evidence in SpiceDB stays as it was; it reopens
only through a declared restore. Reconciliation closes a scope on the same
evidence when it finds it: `AheadOfCanonical`, `UnknownMarker`, and a
`ForeignIncarnation` the journal never held. This changes the first
slice's rule that a contradiction never closes a scope, for these three.

The server starts whatever the fence finds. A failing scope stays closed
and its rooms refuse; a replica whose fence refuses the authority, or fails
on something a later attempt may pass (a statement, a bound, SpiceDB not
answering), holds no attachment, refuses every managed room, and keeps
trying in the background with a capped backoff (1 s to 60 s). Only a
runtime role that does not match the profile stops the attempts.

#### Re-verifying in place

Every acceptance for a managed room compares the binding it reads, under the
scope's guard, with its attachment's. On any difference it is refused
(`AdmissionDenial::AuthorityChanged`: `service-unavailable`, type `cancel`,
a committed denial) and the replica's `HostGrantGate` starts one
verification (single-flight): it runs the fence, which reads the binding
before and after, and swaps the attachment for the one the fence produced.
Until then every managed room is refused, and while it fails the gate keeps
retrying with the same backoff. A PostgreSQL restart or failover therefore
costs a refused acceptance per replica and one fence pass, not a replica
restart.

A restore that rewrites the rows in place, with neither a restart nor a new
relation, changes nothing a binding holds. An attached gate therefore also
runs the fence about every five minutes (`REVERIFY_INTERVAL`, plus up to a
tenth of it at random so that replicas do not pass together): scope
evidence closes the scope in PostgreSQL, where every replica's acceptances
read it; a refusal of the whole authority, or a runtime role that no longer
matches the profile, drops the attachment; a pass that cannot complete
keeps it, up to three in a row (`MAX_INCOMPLETE_PASSES`), after which it is
dropped and the gate keeps verifying. The window of such a restore is the
interval. A gate that holds no attachment and runs no verification (one
that stopped on a runtime role that does not match the profile, which no
backoff retries) verifies again at the same interval, so grants the owner
phase corrects attach the replica without a restart.

Readiness (`HostGrantGate::readiness`, rendered by the readiness handler)
is never coupled to SpiceDB or to a failure every replica shares: failing
it for such a failure would take every replica out of the endpoint set, and
the rooms no scope manages with them, while managed rooms are refused
either way. It returns HTTP 503 for one cause only, while an unretired
scope is enrolled (or that cannot be read): this replica's configured epoch
is below the recorded one (`configuration_predates_restore`), a
configuration of this replica alone that predates a declared restore and
that only a restart with the declared epoch corrects.

Every other failure keeps the replica ready, refuses managed rooms, logs an
error by kind at most once a minute and sets the
`waddle.host_grant.authority.unverified` gauge (1 while an unretired scope
is enrolled and the replica holds no verified attachment); the readiness
body names it in `host_grants`:

- a refusal of the whole authority (a failover the position witness cannot
  verify, a latched regression, an adopted lineage, `projection_absent`, an
  unreadable position witness): `unverified`;
- a configured epoch above the recorded one, which the owner phase that
  declares the restore clears, and which every replica of a rollout ahead
  of the owner phase shares (`configuration_ahead_of_restore`);
- a runtime role that does not match the profile, or a store that cannot be
  read as the profile requires (`runtime_profile_mismatch`): the owner
  phase's grant matrix, shared by every replica; the periodic pass verifies
  again;
- three verifications in a row (`MAX_INCOMPLETE_PASSES`) that could not
  complete, the first at least 30 s earlier (`INCOMPLETE_READINESS_GRACE`):
  `spicedb_unreachable` when the last one failed on SpiceDB, otherwise
  `fence_incomplete`. Each verification that does not complete is counted
  once; attached, the third periodic pass drops the attachment.

A restarted replica refuses managed rooms the same way until the cause is
resolved. A verification that completes, passed or refused for the whole
authority, clears the cause. A replica without a verified attachment in a
deployment with no unretired scope reports `"host_grants": "unmanaged"`; a
verified one reports no `host_grants` field. The alert is the gauge and the
error log, not readiness.

Re-verification in place is a precondition for enrolling the first scope.

### Witness writers

Each writer reads the witnesses first and the canonical state second,
writes conditioned on the exact witnesses it read, and on a failed
precondition reads both again (three attempts). They only move a witness
forward:

- the journal witness moves to the journal's latest operation when the
  journal records the witness at an older generation; a witness the journal
  does not record is kept, except that once a declared restore superseded
  its incarnation, which the journal held, and an operator reopened the
  scope under the new one, it is replaced. Reconstruction moves it with the
  takeover (below);
- the position witness moves when the position advanced on the witnessed
  system and timeline, when a declared restore raised the epoch since, or
  when an acknowledgement applies; such a move removes the latch it read
  in the same request, or, when it read none, is conditioned on there
  being none, and records the witnessed epoch. Every writer reads the
  latch: a stored latch that names the witness, or a latch without a
  witness, stops every move but the replacement a declared restore makes
  and an acknowledgement bound to an on-timeline latch, so `host-grant`
  never moves the witness past a latched or regressed position (it says
  so). A regression is latched, conditioned on the exact witness it names;
  otherwise a regressed or unverified position is kept as it is. Only a
  pass that passed and found a marker or room witness of a lifecycle the
  store holds, or a position witness, latch or witnessed epoch of this
  authority, writes the first position witness: a mutation's writer never
  does, so a SpiceDB that holds nothing of this deployment's is not given
  one (`projection_absent` stays decidable). A writer leaves a witness it
  cannot read alone; an unreadable journal or position witness is counted
  and warned about like a failed write.

`HostGrantAuthority` spawns both writers after every mutation it commits
(enrollment, affiliation change, retirement, reconstruction, fence
closure). After every pass that passes, the gate brings the position
witness up to date and every journal witness that differs from its
scope's journal head, retired and exhausted scopes included, one pass at a
time; a deployment whose witnesses are current costs one statement. The
owner phase holds no SpiceDB credential: its acts (a transition, an
enrollment, a retirement, a reopen, an operator revoke, an acknowledgement)
are witnessed by the next pass that passes, or at once by
`waddle-server host-grant` when the servers' SpiceDB is in its environment.
Writes that fail are counted (`waddle.host_grant.witness.write_failures`)
and logged by kind at most once a minute.

An acknowledgement (`host_grant_authority.position_acknowledgement`,
`<witness stamp>/<system>_<timeline>`) names the exact witness it
supersedes and the system and timeline the server wrote on when it was
recorded. It applies only while that witness is the current one and the
server writes on that system and timeline, and only for a later timeline of
the witnessed system whose position is not below the witness, or for a
regression on the witnessed timeline. It never applies to a regression
across timelines or to another system. That binding is what consumes it:
once applied the witness has moved, and the acknowledgement names one that
no longer exists, also when a restore of the database brings it back.
`host-grant` removes it once it applied it, and a declared restore clears
it. The one exception: an acknowledgement still in the row is honoured
again if SpiceDB once more holds exactly the witness it names (SpiceDB
restored to before it was applied, or rewritten by a holder of its key),
and then only for the same system and timeline.

The runtime role reads the position with `pg_current_wal_flush_lsn()` and
`pg_walfile_name()`, which PostgreSQL grants to every role and the
provisioning leaves in place, and on the attested pool takes the system
identifier from the attested identity. A deployment that revoked the two
WAL functions is refused `position_unreadable` and grants `EXECUTE` on them
(never on `pg_control_system()`, which the attestor requires the runtime
role not to hold). The timeline history, which would tell where a new
timeline branched, needs `pg_read_server_files` and is not read; a timeline
change is therefore unverified rather than decided. The flushed position is
read rather than the written one, so a crash that loses WAL never flushed
is not a regression; a failover to a standby that had not received WAL the
primary had flushed is a regression across timelines, which only a
declared restore clears.

### Invalidating old credentials and work

- **Commands.** The transition advances every restored scope's generation,
  so a command decided before it is refused as stale. The exact retry of an
  operation the restore lost carries its old expected generation and is
  refused too; it is not re-applied.
- **Initiators.** While a scope is `restored`, `reconstructed` or `fenced`,
  no initiator's standing is honoured, whatever the grant rows hold; the
  transition and the fence also lower the effective standings to `none`.
  Only an operator's revoke changes such a scope.
- **Activations and witnesses.** Activation compares the incarnation and
  the generation under the guard, so an activation prepared before the
  transition is superseded. An admission witness is valid only inside its
  transaction.
- **Projection passes.** A pass that read its plan before the transition
  writes only conditioned on the exact marker it saw, and its activation is
  superseded. Only a `RestoredPlan`, which the store alone produces and only
  for a `restored` scope, may take a projection over from another
  incarnation; the type is what the takeover message carries.
- **Replicas.** A replica attached before the transition, or to another
  server start or database, admits nothing until it has verified the fence
  again. A replica configured with an earlier epoch does not attach. A
  binary built before migration 2002 refuses the ledger.
- **Accepted children.** The ingress acceptance contract is unchanged:
  children accepted before the restore point are in the restored database
  and replay as accepted; those accepted after it are gone with it.
- **Credentials.** Not enforced by the server. A physical restore brings
  back the roles with their old passwords; the runbook stops every server
  before the restore, rotates the runtime login's password before any server
  starts, and rotates the SpiceDB key where a holder may survive.

### Verified reconstruction (G15, part)

A restored scope is reconstructed only by reconciliation
(`HostGrantAuthority::reconcile`, and `recover_pending`, which pages through
`restored` and `reconstructed` scopes like other pending ones). No
production caller runs either yet, as for every activation in the first
slice; a restored scope therefore stays closed until the slice that
integrates the authority runs them.

When the scope's marker was written under another incarnation, the pass:

1. refuses a projection family that holds a relationship the projection
   never writes, or more rows than a scope holds (`MalformedFamily`);
2. decides whether the restored database lost operations: it did when the
   journal does not record the marker exactly, or does not record the
   scope's journal witness;
3. records, as one operation of kind `reconstruct` at the next generation
   (`restore::reconstruction_targets`): every subject the projection holds
   that no canonical row names, at `outcast` if the projection bans it and
   at `none` otherwise; and, only when operations were lost, every canonical
   grant lowered to what the projection holds for its subject, `none` where
   it holds nothing. Nothing is raised, in the affiliation or in what
   admission honours: lowering an owner or admin grant of an account this
   deployment does not serve (honoured as none) to the projection's
   `member` is recorded at `none`. An affiliation issuance refuses for its
   subject is recorded no higher than `none`;
4. if operations were lost, the operation of step 3 also marks the scope as
   one a restore lost operations in (`host_grant_scopes.lost_operations`),
   and is recorded even when no grant changes, unless the scope holds the
   mark already (every mark is set by the operation that recorded it, at or
   before the current generation, so a database that holds the current
   operation holds the mark). The projection written next names that
   operation, which a database restored to before the mark cannot hold: a
   later pass over such a database finds the marker unrecorded or ahead of
   its journal and marks or fences the scope again. A journal witness that
   cannot be read (more than one relationship, or a stamp that is not a
   canonical one) is treated as showing a loss: the mark is recorded and
   the pass stops (`MalformedJournalWitness`), leaving the witness and the
   projection as they are, until an operator removes the witness. Then the
   pass takes the projection over: one `WriteRelationships` request that states
   the whole affiliation family of every canonical row, the room witnesses,
   deletes the old marker and touches the new one, conditioned on the exact
   old marker, and moves the journal witness from exactly what step 2 read
   to the current operation.
   What the witness showed lost is recorded by then; a witness of an
   incarnation the journal never held is not left to close the scope after
   it is reopened. A failed precondition stops the pass;
5. reads the projection back, compares it exactly with the canonical rows
   and activates the exact operation: the grants become effective and the
   scope is `reconstructed`, still closed.

A reconstructed scope admits again only after an operator reopens it
(`waddle-server host-grant reopen`): the operator's acknowledgement of the
restore, made after reviewing what the witnesses show it lost. A reopen
requires the scope's latest operation to be verified and activated; it
records a `reopen` operation at the next generation and opens the scope,
whose projection follows with the next pass. A scope marked as one the
restore lost operations in is reopened only by name, with the operator's
confirmation that what was lost has been applied again
(`--lost-operations-reapplied`); the reopen clears the mark. Mutations
recorded while it is `reconstructed` (an operator's revoke) are projected
and activated without opening it.

A marker missing after an activation (`MarkerMissing`) or malformed leaves
the scope closed: without a projection there is no witness, and
missing-marker initialization is allowed only in a fresh namespace. A
restored scope that was never projected is projected into its fresh
namespace as at enrollment, with what a takeover adds: the pass reads the
journal witness first, marks the scope as one the restore lost operations
in when the journal does not record it (nothing was projected to lower the
grants to, so the operator re-applies what was lost before the reopen by
name) under a `reconstruct` operation that the projection then names, as in
step 4, and moves the witness to the current operation in the same request
as the projection. A failed condition there is a contradiction
(`NamespaceNotFresh`) when the scope's rooms hold relationships, and
`Superseded` otherwise. No first projection, of a restored scope or of a
newly enrolled one, is written while a scope admits or was ever projected
and SpiceDB holds nothing of this authority (`projection_absent`).

Bounds. A scope holds at most 8 rooms and 200 grant rows. An observation is
at most 33 reads within 20 s; the `reconstruct` operation records at most
200 rows in one store transaction; the takeover is one request of at most
812 updates (802, 8 room witnesses and the journal witness's two). A
reconstruction whose rows would exceed 200 is refused before anything is
recorded (`ReconstructionTooLarge`) and the scope stays closed. Each scope
is reconstructed independently, and a scope that cannot progress is visited
once per recovery pass without holding back another. A fence pass reads
every marker and witness in one bounded stream (at most one marker, one
journal witness and eight room witnesses per enrolled scope, and one
position witness and one latch), every scope in one statement, and up to
four PostgreSQL snapshots per scope SpiceDB names; its budget grows with
the number of scopes it compares.

Declaring a restore is also how an operator has a scope whose projection
contradicts it with a well-formed marker rebuilt: the scope closes and is
reconstructed. Other repair of a contradiction is not decided.

### Operator acts

`waddle-server host-grant`, as the migrator login with the owner phase's
environment, records an operator's act in one or more short transactions
and can run while the servers run. With the servers' SpiceDB in its
environment (`WADDLE_SPICEDB_ENDPOINT`, `WADDLE_SPICEDB_PRESHARED_KEY`) it
writes the act's witnesses at once; without it, the next pass that passes
does.

- `reopen [--scope <lifecycle> [--lost-operations-reapplied]]` reopens
  reconstructed scopes whose latest operation is verified, and reports how
  many it left closed: not reconstructed, or (without the confirmation)
  marked as having lost operations;
- `acknowledge-position [--witnessed <stamp>]` records an acknowledgement
  of that position witness for the system and timeline the server writes
  on; without `--witnessed` it reads the witness from SpiceDB. One that
  could never apply (another system, a witness of a later timeline, or of
  an earlier timeline whose position the server's timeline is below) is
  refused. With SpiceDB it applies it at once and removes it, and says why
  when it did not;
- `revoke --scope --room --subject [--ban] --operation-id --account-domain`
  records an operator's revoke or ban, with no initiator, in any scope that
  is not retired or exhausted, closed ones included. It never raises (a
  banned account stays banned), keeps a recorded owner of a served account
  in the room (`LastOwner`), takes effect at its commit, and is replayed
  under its operation identity. It is how a revoke the restore lost, which
  no witness records the content of, is applied again.

### What stays open

- What a lost operation changed is not recorded outside PostgreSQL: the
  witnesses show that operations were lost, and reconstruction lowers what
  the projection shows; a lost revoke that was never projected is applied
  again by the operator (`revoke`) from what the operator knows, before the
  reopen. The audit trail of the lost operation is in the lost database.
- A witness write that had not completed at the restore point records
  nothing: the residual window is the lag of the asynchronous writers
  (after a mutation through `HostGrantAuthority`, a task; for an owner-phase
  act without SpiceDB, until the next pass that passes, up to 330 s with a
  replica attached and unbounded while every replica's fence refuses), plus
  the projection lag for what reconstruction can lower.
- A same-timeline regression is latched only once a pass sees it. A
  restored server whose flushed position passes the witnessed one before
  any pass compares them (a crash-consistent snapshot of a busy cluster,
  say) is not seen as a regression; the scope evidence still applies. A
  latch the replica holds only in memory, because SpiceDB did not take it,
  is lost when the replica restarts, and stops counting once a writer that
  does not hold it moves the witness after the server wrote past it.
- A lost enrollment whose projection was never written is not detected,
  and no tool retires a scope the database no longer knows; its rooms are
  refused while SpiceDB names them. While SpiceDB does not answer and no
  pass has read the refused rooms, they are served as unmanaged.
- The refused-room set a replica keeps is the one its last pass that read
  SpiceDB and the scope table produced; once it exists, single rooms are no
  longer read from SpiceDB. It is stale both ways until the next such pass
  (up to 330 s, unbounded while passes fail before that read): a lost
  enrollment that appears after it was read is served as unmanaged, and a
  room it lists stays refused after its scope is enrolled again or its
  witness removed.
- `projection_absent` is reversible; if the right SpiceDB never comes back
  the deployment declares a restore, and a scope whose activated projection
  has no marker stays closed (`MarkerMissing`) with no tool to reopen it.
  Nor is a first projection written into the replacement SpiceDB, of a
  restored scope or of a newly enrolled one, while the store holds a scope
  that was projected: no tool yet seeds a replacement SpiceDB with this
  authority's witnesses.
  PostgreSQL and SpiceDB restored together to agreeing points leave no
  witness; the operator procedure is the control.
- A promotion is never verified automatically; it needs an operator's
  acknowledgement or a declared restore. An acknowledgement is the
  operator's assertion that the promotion lost nothing.
- Witness writes after a commit are spawned per commit and not coalesced.
- Credential rotation is operational, not enforced.
- The attested runtime's pinned catalog profile does not cover the
  host-grant relations, as in the first slice.

### Restore rollout part (G24)

- Migration 2002 adds to `host_grant_authority` the restore epoch (default
  `0`), the lineage binding, the fixed `authority_id` and the position
  acknowledgement; to `host_grant_scopes` the `lost_operations` mark; the
  `restored`, `reconstructed` and `fenced` closed reasons; the `restore`,
  `reconstruct`, `fence` and `reopen` operation kinds; and an index on the
  journal by lifecycle and incarnation. It creates no relation, so B1's
  grant matrix is unchanged. The owner phase applies it; a server of the
  previous release refuses the ledger, so the owner phase runs before any
  server of the release starts. On SQLite it creates nothing.
- The first owner phase after the migration binds the authority to the
  database lineage and changes nothing else.
- `WADDLE_HOST_GRANT_RESTORE_EPOCH` is new and defaults to `0`, the value
  the migration records. The owner phase and every server must read the
  same value; with the chart, `config.extraEnv` renders it into the
  ConfigMap both the owner job and the Deployment read, so no template
  changes.
- `waddle-server host-grant` is a new operator subcommand. It reads
  `WADDLE_SPICEDB_*` when set.
- A server starts whatever the restore fence finds: the fence runs when the
  ingress authority attaches, and a refusal leaves managed rooms unavailable
  on that replica. Readiness fails, while an unretired scope is enrolled,
  only for a configured epoch below the recorded one; every other
  host-grant failure keeps the replica ready and raises the error log and
  the `waddle.host_grant.authority.unverified` gauge.
- The built-in SpiceDB schema is unchanged; witnesses live in the marker
  definition.
- Behavior with no scope enrolled: the fence reads the authority row, lists
  the projection definition (empty) and reads the scope table (empty); no
  witness is written; acceptance is unchanged except that, until the first
  pass has read the refused rooms, an acceptance for an unmanaged room may
  ask SpiceDB about its room witness. After a lineage adoption of a
  database without a scope, the owner phase binds the new lineage.

### Running the restore slice's cases

`tests/host_grant_restore.rs`, from `server/`, with the environment of the
[first slice's focused set](#running-the-first-slices-focused-set),
`WADDLE_TEST_REQUIRE_POSTGRES=1` included so that a case fails rather than
skips without PostgreSQL, and a SpiceDB endpoint of the run's own:

```sh
cargo nextest run -p waddle-server --features test-support --locked \
    --run-ignored all -E 'binary(host_grant_restore)'
```

The cases without a `live_spicedb_` prefix need PostgreSQL only; the
`live_spicedb_` cases need both stores, run in the `spicedb-live` group and
empty SpiceDB before each case, refusing a store that holds a position
witness no test suite marked (`tests/host_grant_support/live_spicedb.rs`).
A restore is simulated by copying the host-grant relations into a schema of
their own and rewriting them from it in one transaction. PostgreSQL is not
restarted and its WAL position does not go back: a changed server start is
stood in for by an attachment bound to another one, a regressed position
by a witness written ahead of the server, and a server that has written
past a latched regression by a witness below the server with its latch.
The position read under the provisioned restricted profile is in
`tests/restricted_runtime_startup.rs`.

## Blocking review ledger

Slice names refer to [the federation plan](../../../docs/planning/federation-2047.md),
whose gate table also names the verification step that discharges each entry's
evidence.

| ID | Unresolved decision/evidence | Work blocked | Under ADR-0020 |
| --- | --- | --- | --- |
| B1 | Exhaustive scope writer/reader exclusion, old writer retirement and concrete store capabilities | Enrollment and production authority integration. | D5 condition, gate G11; open. Decided by W5b (store capabilities, credential separation: [recorded](#b1-store-capabilities-and-credential-separation-g11)) and W5c (writer inventory and exclusion); old binaries and credentials are retired under the rollout runbook (G24) and the activation runbook (G22). |
| B2 | Complete action/negative/admin/global dependency policy, actor projection, XMPP request/result identity | Affected protected operations and handlers. | Gate G14; open. Decided per protected surface by W5b, W5c and W5e. Includes outcast precedence in every channel and message action, including an author's own edit and delete (W5o), and the D8 rules: privileged grants and room moderation for remote principals only from trusted homes and only on `pkix` streams, and a grant that is not honored contributing nothing. W5b's part is [recorded](#b2-policy-for-the-action-this-slice-protects-g14-part). |
| B3 | CAS/marker/schema protocol, bounded reconstruction, missing-marker and large-graph rules | Projection integration beyond independently proven bounded operations. | Gate G15; open. Off the first slice's safety path, which admits in SQL. W5b decides the marker protocol for the projection writes it makes ([recorded](#b3-marker-protocol-for-this-slices-projection-writes-g15-part)), W5c extends it to the writers it fences, W5o decides the fenced schema transition and W5r bounded reconstruction ([recorded](#verified-reconstruction-g15-part)). Any admission that reads the projection needs B3 decided first; no slice in the plan adds one. |
| B4 | Actual claim/authority/child lock order, atomic admission and cancellation/lost-commit ownership | First manifest integration. | D5 condition, gate G12; open. Decided by W5b ([recorded](#b4-lock-order-atomic-admission-cancellation-and-lost-commits-g12)) and extended by W5c for each surface it fences. |
| B5 | PubSub/DirectFrame admission identity; read/join/SM sink contracts | Those sinks and community activation. | Gate G16; open. Decided by W5c; W5d's #2056 control is its evidence. |
| B6 | XML-decoded pre-parse enforcement, canonical JID/account-binding agreement, isolated codec and authoritative reconciliation; ASCII test profile and full PRECIS/IDNA migration unresolved | External identities and enrolled authority keys. | D5 condition, gate G10; open. W1 covers only the codec part; W5i decides the rest, including the canonical domain form, and W5a applies it at S2S ingress. |
| B7 | Restore incarnation surviving rollback and excluding old deployment/work | Recovery capability and pilot activation. | D5 condition, gate G13; open. Decided by W5r ([recorded](#restore-incarnation-decisions), with the limits it leaves open); its evidence runs in verification step 4. |

The coarse fence's availability cost is a non-blocking tradeoff once bounded
resources and independent progress are proved. Fine-grained fences can follow
only after complete policy/mutation dependencies are demonstrated. Merging this
document, accepting D5 or passing an inventory scan discharges none of these
blockers.
