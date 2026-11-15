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
The writer-fencing slice is implemented as well: its decisions are recorded
under [Writer fence and sink contracts](#writer-fence-and-sink-contracts),
and [its rollout part](#rollout-part-of-the-writer-fence-g24) lists what it
changes while no scope is enrolled. No scope is enrolled by any release
configuration, and nothing enrolls one at
runtime, so the first slice changes no behavior of an existing deployment beyond
its migration, the built-in authorization schema's version, one indexed
lookup on legacy channel and Space permission writes, a privilege check of
the runtime role when a PostgreSQL replica starts, and the refusal of a room
address that is not in canonical form (see
[Rollout part](#rollout-part-g24)).

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
insufficient. The mechanism surviving rollback and excluding the previous
deployment is unresolved (B7); the restore-incarnation slice (W5r in the plan)
owns it. Graph loss also requires closed admission before reconstruction;
silent active corruption is not covered by an old marker.

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
| Restored older PostgreSQL state | Restore cannot reopen old authority; old credentials and work are invalid; admission stays closed until verified reconstruction. | W5r (step 4) |
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
| `host_grant_authority` | The deployment's authority incarnation, one row. Every scope and every projection marker is bound to it; a scope bound to another incarnation admits nothing and accepts no grant change, and its projection is neither written nor activated. The restore slice owns how it changes. |
| `host_grant_scope_lifecycles` | One row per enrollment: lifecycle identity and scope key, both unique. Never deleted, and updated once at most: retirement stamps `retired_at_ms`. Only the schema owner writes it. |
| `host_grant_scopes` | The latest state of a scope: incarnation, policy version, generation, the operation that recorded it, the latest generation whose projection was verified and activated, and `closed` or `active` with the reason for `closed` (`enrolled`, `retired`, `exhausted`). Its row lock is the scope's authority guard. |
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
the scope as exhausted. Serving replicas learn of an enrollment within one
reload of their enrolled set, and the enrollment procedure waits for that
before the scope is relied on ([enrolling a
scope](#rollout-part-of-the-writer-fence-g24)).

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
scope can share an identifier space with group DMs. The writer fence refuses
both for a managed room's identifier
([B1 of the writer fence](#b1-writer-and-reader-exclusion-g11)).

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

A contradiction or an unknown outcome never closes the scope and never
changes canonical state: the recorded increase stays ineffective, effective
grants keep admitting, and a revoke still takes effect at its commit. Nothing
in the projection can make a revoked grant effective, because admission does
not read it.

Not decided here: repair of a contradiction and reconstruction of a
projection (the restore slice), projections larger than one request, and any
admission that reads the projection. A retired or exhausted scope has no
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
  writer-fencing slice (W5c), which records them under
  [B2 of the writer fence](#b2-policy-for-the-surfaces-this-slice-protects-g14-part).

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
PostgreSQL-backed SpiceDB datastore. The same set was executed again on
2026-11-15 at the writer fence's commit `eb73991da` (below), against the
same versions, and passed 110 of 110 cases.

## Writer fence and sink contracts

These are the decisions of the writer-fencing slice (W5c in the plan) for
gates G11, G12, G14 and G16, and its part of G15. Like the first slice's,
they are decisions: the acceptance-matrix rows marked W5c and the suites
listed under [Evidence](#evidence-of-the-writer-fence) are their evidence,
and the last run of those suites is recorded there. They restate every bound
of the first slice they rely on.

### One decision for every surface

A room an enrolled scope manages (a *managed room* below, as in the first
slice) is decided on every surface from its canonical grants, with the
admission policy of the first slice (`policy::grant_allows`): the
recorded and effective affiliation, outcast winning, and an owner or admin
grant of an account of a domain the replica does not serve honoured as no
grant. `HostGrantGate` (`permissions/host_grant/gate.rs`) is that decision
outside room-manifest acceptance. For a room it answers:

- *unmanaged*: the replica's enrolled set (below) does not name the room,
  and the surface keeps its existing checks. Nothing is read from
  PostgreSQL for it;
- *managed*: the room's canonical grants, read in one `REPEATABLE READ`
  snapshot that takes no row lock (`HostGrantStore::room_grants`), bounded
  by 500 ms for a lock, 1 s for a statement and 1.5 s for the whole read,
  together with the account domains the replica serves;
- *unavailable*: the enrolled set has not loaded, the room's address
  reaches an enrolled room in a form that is not the identity profile's
  canonical form, the grants could not be read within the bounds, or the
  replica holds no verified attachment (B1). Every surface fails closed on
  it. A room address that is not canonical and reaches no enrolled room is
  unmanaged.

Enrolled set. Whether a channel, a room address or a Space node belongs to
an enrolled scope is answered from the replica's in-memory copy of the
enrolled set (`EnrolledScopes`, `permissions/host_grant/enrolled.rs`): the
room bindings and Space nodes of every scope, retired ones included, each
with its scope lifecycle. It is read in one background snapshot under the
read bounds of the store (5 s for a lock, 10 s for a statement), never on a
request path:

- A PostgreSQL replica loads it before it serves: startup retries the load
  every second for up to 30 s and exits if it cannot load it, like a replica
  whose database is unreachable at startup. A schema without the host-grant
  relations holds no scope, and every object is unmanaged there.
- It is reloaded every 5 seconds (`ENROLLED_SET_REFRESH_INTERVAL`). A reload
  that fails keeps the set loaded before, so a room outside the set keeps
  its legacy checks while PostgreSQL is unreadable; only managed rooms,
  whose grants are read per decision, fail closed then. A reload that finds
  the host-grant relations gone after a load is a failed reload too, never
  a schema without scopes: migrations only add them.
- Until the first load succeeds, every channel, room address and Space node
  is undecided and fails closed, on every surface and in the permission
  actor's legacy reads; a reader waits up to 2 s for that load before it
  answers.
- Enrollment only adds to the set: a room binding is immutable and a
  retired scope's objects stay enrolled. A scope enrolled on any replica is
  therefore managed on each replica within one refresh interval plus one
  reload after the enrollment commits, and decided as unmanaged there until
  then. A reload is one snapshot of three statements under the read bounds
  above, so it ends or fails within them. Room-manifest acceptance reads
  the bindings in its own transaction (first slice) and is not delayed.
- A reload that holds fewer objects than the set it replaces is applied
  and logged at warn with the number of rooms, channels and Space nodes it
  no longer holds, and nothing that names them.
- While reloads fail the set ages. Its age since the last successful load is
  recorded on every refresh tick as the gauge
  `waddle.host_grant.enrolled_set.age` (seconds, no attributes). Once a set
  that holds the host-grant relations is older than 30 s
  (`ENROLLED_SET_STALE_AFTER`, six refresh intervals) the replica's
  readiness check answers not ready (`host_grant_enrolled_set: stale`) and
  a rate-limited warning says so, until a reload succeeds; the replica keeps
  serving the connections it holds with the set it has. A schema without
  the relations and a deployment without a canonical store are never
  stale.

The gate holds the same attachment as the ingress unit of work: a replica that
attaches to the store attaches both, from one check, and the served account
domains both use come from that attachment. The gate and the permission actor
share one enrolled set. On SQLite, and on a PostgreSQL schema without the
host-grant relations, every room is unmanaged. Legacy tuples, durable room
affiliations, PubSub affiliations and subscriptions, Space membership,
deployment roles and the room actor's own affiliation list admit nothing in a
managed room.

### B1: writer and reader exclusion (G11)

Reader exclusion. The permission actor answers no legacy read of a managed
room's channel or of an enrolled scope's Space: `CheckPermission` is not
allowed, and `ListRelations`, `ListSubjects` and `LookupSubjects` are empty;
`LookupResources` leaves such objects out. The answer is decided from the
replica's enrolled set, in memory, so no legacy read waits on PostgreSQL
inside the permission actor's mailbox; until the set has loaded, a read of a
channel or a Space object fails (other objects are unaffected). Legacy writes
are still refused from PostgreSQL on every write (first slice). So a
projection that holds a managed room's affiliations, a relationship an older
binary or an operator tool wrote, and relation-name checks (`owner`, `member`,
`outcast`, which subtract no ban) decide nothing there, whichever surface
asks. Surfaces that serve managed rooms ask the gate instead; a surface this
slice did not convert therefore denies a managed room rather than deciding it
from the graph.

Writer exclusion. Every writer the fresh sweep found that can write channel,
Space, room or PubSub grant state is listed here with its disposition for an
enrolled scope, together with the readers that decide a protected effect or
disclose a managed room. *Store* means it goes through the host-grant store;
*refused* means it changes nothing for an enrolled scope and answers an
error or skips; *gate* means it decides from the canonical grants (B2);
*out of scope* names why the writer cannot reach grant state of an enrolled
scope.

| Writer or reader | Disposition for an enrolled scope |
| --- | --- |
| Permission actor `WriteTuple`, `DeleteTuple`, `ReplaceExclusiveRelation`, `SwapExclusiveRelation` (every legacy caller below) | Refused (first slice): object, Space subject or channel subject of an enrolled scope. |
| XEP-0045 affiliation change (`iq/muc_admin.rs`, graph-first `persist_managed_channel_affiliation` and its `SwapExclusiveRelation` rollback) | Store: the change is recorded in the host-grant store before the room actor applies it; no relationship is written and nothing is rolled back. The graph-first write and its rollback remain for unmanaged channels only. |
| XEP-0045 role change and moderator role | Authority from the store (`moderate`); the moderator role is granted to served accounts only. Roles are occupancy, not grants. |
| XEP-0045 owner configuration (`muc_owner_config.rs` channel owner tuple) | Store authority (`manage`); writes no owner relationship; members-only is kept. |
| XEP-0045 destroy | Refused: no room affiliation confers `delete`; a managed room ends when its scope is retired. |
| Mediated invitation (`message/muc_invite.rs` member grant, its rollback and rebuild) | Refused when planned; an invitation accepted before enrollment is delivered without granting membership. Its rollback writes neither the legacy relation nor the room actor's list of a managed room, or of a room whose management cannot be told. |
| Group-DM invitation rollback (`message/group_dm_invite.rs`) | Refused for an identifier that is a managed room's channel, or whose enrollment cannot be told: it writes nothing there. |
| Room actor durable affiliations (`muc_durable`, `ChangeAffiliation`, `ApplyAdminItems`, resolver sync) | Out of scope as grants: for a managed room the actor's list is a mirror written only from canonical decisions (join resolution, administration after its canonical commit, and the repair below). Admission never reads it. |
| Mirror repair (`iq/muc_admin_host_grant.rs` `MirrorRepair`) | Store-derived: armed before a request records its first change, and kept unless the room actor applied every change or the request was refused before the store could hold any of them (so a batch refused part-way and a commit whose outcome could not be read back are repaired too). Each changed subject's entry is then set to its recorded grant in the background, after 0.25, 1, 4, 16 and 60 s until an attempt succeeds. An attempt applies one generation of the grants and reads them again, applying the newer values while the generation moves (at most four reads), so it never leaves an older value over a change applied since. |
| Claim-move re-seating (`clustering/route_bridge/mailbox.rs`) | Gate: a recovered occupant is seated again only while its grants allow `read`, with the standing they honour and the moderator role only with `moderate`; the room is not seated when the grants cannot be read. |
| Room actor configuration (every actor the room registry spawns) | Members-only binding: an actor spawned for a room the enrolled set names, or while it is unknown, keeps `members_only` through its creation spec, durable restores and configuration changes (`RoomActor::with_members_only_binding`, asked through the durable membership source). |
| Admin channel commands (`admin/channels.rs`: update, delete, set-affiliation, kick, occupants, affiliations, creation in an enrolled Space) | Refused. |
| Admin Space commands (`admin/spaces.rs`: update, delete and its cascade, members, set-role, create on an enrolled node) | Refused; a Space deletion whose cascade would reach a managed room is refused whole. |
| Group-DM member mirror (`admin/channels.rs` direct SQL on `permission_tuples` and its actor pair) | Refused before either statement for an identifier that is a managed room's channel. |
| Spaces publish, retract and bookmark cleanup (`iq/spaces_publish.rs`, `iq/spaces_retract.rs`, `iq/spaces_bookmark_cleanup.rs`) and channel-space links | Refused for a managed room and for an enrolled Space node. Placing any other channel follows the placement rule (B2), and a placement removes every other Space as the channel's parent, including one that listed no bookmark of it. |
| PubSub on the Spaces service (`iq/pubsub_dispatch.rs`, `iq/pubsub_admin.rs`: create, configure, delete, purge, affiliations, subscriptions, publish, retract, items, subscribe) | Refused for an enrolled Space node; leaving a subscription is kept. Request kinds the build does not classify are refused on the Spaces service. |
| Owner reseed (`spaces_pubsub_seed.rs`, from topology, node creation and admin Space creation) | Retired as an overwriting writer for every node: an existing outcast is kept, in one conditional statement. Enrolled Space nodes are never seeded. |
| Topology bootstrap (`server/topology.rs`: channel rows, General Space, bookmarks, parent tuples, room actor configuration) | Refused for managed rooms and an enrolled General Space; managed rooms' actors are opened members-only. |
| Join parent repair (`presence/muc/access.rs`) | Refused: a managed room is resolved from the store before any repair. For any other channel a join fills a parent only when the channel has none, its link names the Space and that Space lists its bookmark; it never adds a second parent or removes one. |
| Spaces items backfill (`iq/spaces_items.rs`) | Refused for an enrolled Space node (PubSub refusal); the parent writes it would make for a managed room are refused by the actor. For any other channel a read backfills a bookmark and a parent only when no Space other than the read one is the channel's parent; it never removes a parent or another Space's bookmark. |
| Extension route reads (`iq/extension_route_items.rs` member affiliation on room-bound nodes) | Refused for a managed room: it serves no extension state. |
| Extension PubSub publication (`extension_commands/pubsub.rs`) | Refused for a managed room's channel, and for one whose enrollment cannot be told: such a room serves no extension state. |
| Extension bot seating (`interpret/bot.rs`) | Refused: no grant is an extension bot's, so a bot is never seated in a managed room. |
| Legacy permission reads (`CheckPermission`, `ListRelations`, `ListSubjects`, `LookupSubjects`, `LookupResources`) | Answer nothing for an enrolled object, decided from the enrolled set; fail before it has loaded. |
| Extension host tools (`extension_host_adapter`: room send, archive query, member and channel lists, Space list, PubSub item reads) | Gate, with no deployment-owner fallback; an enrolled Space node is left out of the Space list, and an item read of a node bound to a managed room, or to a room whose management cannot be told, is refused. |
| XEP-0425 moderation, call setup, Muji call presence, channel-wide mentions, pin list, room system messages | Gate (B2). |
| Media grant re-assertion and eviction of a live call participant (`sfu_voice_reconcile.rs`, `websocket/muc_call_sfu.rs`, `livekit_webhook.rs`) | Occupancy only: they push the media grants a seated occupant's mirrored voice derives and evict a participant who is no longer an occupant. They do not consult the grants and mint no token; ending a revoked participant's media is the revocation slice's (W5d). |
| Host-grant enrollment (`HostGrantStore::enroll_scope`) | Refused for a channel whose catalog row is a group direct-message room, read in the enrollment transaction: a group DM's members are its conversation's participants, not grant subjects. |
| Channel search, Spaces node disco#info, admin channel and Space listings | Gate (B2): a managed room is disclosed only to a subject with `view`, and an enrolled Space node to nobody. Search and the admin listing read the grants by the channel binding the enrolled set named, and withhold the room whatever else the gate answers. |
| `set_room_affiliation` facade (`xmpp_permission_state.rs`, `xmpp_state.rs`, `waddle-xmpp` `AppState`) | Retired: removed. |
| Account bootstrap and reconciliation (`server/bootstrap_membership.rs`) | Out of scope as a scope writer: it writes deployment relations only. Deployment ownership is provisioned only for an account of the served domain (D8). |
| Host-grant projection (`WriteHostGrantProjection`) | Store: the only writer of an enrolled scope's affiliation family (first slice). |

What this does not exclude, and is not claimed: a holder of the runtime
database credential or the SpiceDB key writing with its own client, an older
binary, extension modules and other crates outside the two scan roots. Those
remain the rollout and activation runbooks' old-writer retirement (G24, G22).
The sweep is lexical and the table is its review, not a call graph.

### B2: policy for the surfaces this slice protects (G14, part)

| Surface | Action under the first slice's table |
| --- | --- |
| Join | The standing the grants honour: outcast is refused as banned, no grant as membership required; owner, admin and member enter with that affiliation. The room is members-only. A managed room without a channel row is not served. |
| Archive (XEP-0313), mention audience, durable membership | `read` |
| Room disco#info | `view`; anyone else is told the room does not exist. The room is advertised members-only. |
| Service disco#items, Spaces disco#items | A managed room and an enrolled Space node are never listed. |
| Affiliation change | The store's initiator rule (first slice): an owner grants and removes owner and admin standing, an owner or admin changes other standing, the room keeps an owner who can administer it, and issuance refuses an owner or admin grant to an account outside the served domains and the trusted homes. One change per request. |
| Affiliation lists | The member list to anyone with `read`, every other list to a subject with `moderate`; the lists are the recorded grants. |
| Role change | `moderate` |
| Owner configuration | `manage` |
| Destroy | `delete`, which no affiliation confers |
| XEP-0425 moderation | `moderate`; the occupant role the room actor holds decides nothing. |
| Call setup (LiveKit token) | `send_message`, at every decision that leads to a minted token: the setup on each replica of a relayed initiate and the re-check before the token is released. The media capabilities still follow the voice the room actor mirrors, which can only narrow them. Re-assertion and eviction of a live participant's media grants follow occupancy only (B5). |
| Muji call presence (starting, joining or preparing a call, and the call-thread anchor it starts) | `send_message`, on the local path and the relayed ordered delivery alike; unreadable grants refuse it with `internal-server-error`. Leaving a call is always accepted. |
| Channel-wide mention (XEP-0513 `#channel` push) | `moderate`, decided before dispatch and passed to the room chain; the role the room actor mirrors decides nothing, and unreadable grants broadcast nothing. |
| Pin list | `read` |
| Room system messages (pin and unpin notices, call-thread anchors and endings) | Archived as the room's own; delivered to the seated occupants whose grants allow `read`. An interpreter without the WebSocket state, whose gate alone tells whether a scope manages the room, delivers none. |
| Channel search | `view`, read by the channel binding; nobody else is shown the room. |
| Admin channel listing | `view`; the community-owner role the command needs is not a grant. A listed room shows members-only and its recorded grants as counts. |
| Spaces node disco#info, admin Space listing, extension Space list | An enrolled Space node is described to nobody: no grant of the scope is a Space's, so no `view` of it can be decided, and its name, owners and counts are PubSub state the scope does not consult. |
| Channel permission checks (extension host tools, extension route access) | `view`, `read`, `send_message`, `moderate`, `manage`, `delete` map to the table; any relation name or other permission is refused. Neither the default channels' deployment-membership fallback nor the host adapter's deployment-owner fallback applies, so a ban denies everything. |
| Extension state (room-bound extension nodes, extension PubSub publication, the host tool's item reads) | Not served. |
| Extension bots | Never seated. |

Extension observers in a managed room observe nothing: the acceptance drops
them like any other unadmitted reader (first slice), its room-bound
extension nodes are not served, and extension bots hold no grant.

Channel placement in a Space (every room no scope manages). Publishing a
room's bookmark to a Space node moves the channel's link to the node and
writes the Space as its parent, so it is decided in code from explicit
relationships, alike on every permission backend
(`iq/spaces_publish.rs`):

- A publish that moves no authority needs only the Space node's own
  rights: the channel's link or a parent relationship names the node, and
  neither names another Space. When the link and a parent disagree, the
  publish would move one of them, and the rule below applies.
- Otherwise the publisher holds an owner, admin or manager relationship on
  the channel, or an owner or admin relationship on a Space that is the
  channel's parent now (the authority the channel inherits from its current
  Space), and is not the channel's outcast; or the publisher is the
  deployment owner, who can place any channel through the channel
  administration commands.
- A placement makes the node the channel's only parent: every other Space
  stops being its parent, whether or not it listed a bookmark of it.
- Nothing else moves a channel. A Spaces items read and a join only fill a
  parent the channel lacks, from a link that names the Space (and, for a
  join, a bookmark that Space lists); a link that disagrees with the
  channel's parent changes nothing until a placement by someone with the
  authority above.

Remote privilege (D8). Every surface above honours a privileged or moderator
grant only for an account of a domain the replica serves, as the first
slice's acceptance does; the moderator role likewise. W5e replaces the rule
with the trusted-home and `pkix` evaluation. Bootstrap and repair writers are
given the trusted-home set they use, and are given none today: the owner
reseed refuses Owner for an account outside the served domain and that set,
and account bootstrap provisions deployment ownership only for an account of
the served domain.

Members-only binding. A managed room's binding is members-only. The binding
is applied wherever a room actor's configuration is built for such a room
(join, topology) or changed (owner configuration), and every decision this
slice makes reads the room as members-only whatever an actor that ran before
enrollment holds. The room actor's flag is therefore not an input of any
decision for a managed room.

Planner audience. The planner's sources for a managed room are its grants:
durable membership is the subjects with `read`, the mention audience admits
only them, and seated occupants entered only through the join decision
above. The acceptance still reduces the plan (first slice); the planner's
filtering spares the work it would drop.

XMPP request identity (B2). An affiliation change is recorded under a fresh
operation identity, decided at the generation the handler read; a stale
generation is decided again, at most three times, then refused with
`resource-constraint`. An IQ identifier is not a durable idempotency key, so
a retransmitted request is a new operation; setting the affiliation a
subject holds records a new generation and changes nothing it admits. An
unacknowledged commit is read back by its operation identity: committed is
success, absent and unreadable are `internal-server-error`. The room actor
applies the change after the commit; if it fails, the reply says so, the
change stays recorded, and it is what every admission reads.

### B5: sink contracts (G16)

- **Join.** The decision is taken before the room actor seats the occupant,
  from one snapshot, with no lock held across the actor ask. Seating is not a
  protected effect: what reaches an occupant afterwards (groupchat, inbox,
  notifications, recovery) is admitted again by the acceptance. A revoke
  that commits after the decision does not eject an occupant seated by it;
  ejection and the presence an occupant still sees until then are the
  revocation fence's (W5d), which defines what a revoke's acknowledgement
  promises.
- **Protected reads.** An archive page and a room description are admitted by
  a decision taken after they were read: the gate is asked before the read (to
  refuse early) and again after, and the response is sent only if the second
  decision admits it. A pin list is decided once, after it was read. Its
  snapshot follows every read the response holds, so nothing read after a
  revoke that committed before the decision is sent; a response admitted
  before a revoke may still be sent after the revoke's acknowledgement, as
  already accepted work. Neither decision holds a lock, so no read delays a
  mutation.
- **PubSub and stream-management fan-out.** No PubSub admission identity is
  built: the Space node of an enrolled scope and the room-bound extension
  nodes of managed rooms accept no subscription, publication or items request,
  and fan-out from an enrolled Space node delivers nothing to a live queue or
  a detached stream-management buffer. Fan-out recognises the Spaces and
  extensions services by the node's owner, not by the request's PEP
  classification, which a request addressed to a service also carries. When
  whether a Spaces node is enrolled cannot be told, fan-out asks once more and
  then drops the event, logging only that a Space node's event was dropped. A
  subscriber list captured for those nodes is therefore never acceptance,
  because nothing is delivered from it. Serving a scope's Space through PubSub
  needs that identity and is not built; the revocation slice's #2056 control
  is its evidence for unenrolled Spaces.
- **Queued work.** The groupchat audience is the first slice's manifest
  acceptance. Invitations, which would queue a membership grant, are refused.
  Room effects of an administration request are produced by the room actor
  after the canonical commit, from the change that was recorded.
- **Room system messages.** The room's own message is archived like any
  groupchat; its audience is the room actor's seated occupants reduced to
  those whose grants allow `read`, decided after the actor's snapshot and
  before the route intents are captured, so a recovered delivery replays the
  reduced audience. When the grants cannot be read nothing is archived or
  sent, and the caller's own retry applies (a call-thread ending is
  retried from its persisted fence). A system message planned inside a
  room-manifest acceptance is refused by the first slice's plan reduction.
- **Call tokens.** A call is verified on the replica that receives it, on
  the room's owner when relayed, and again before the token is released;
  each verification asks the gate for `send_message`, so a revoke that
  commits before the last one withholds the token. A Muji presence that
  starts, joins or prepares a call asks the gate for `send_message` before
  the room actor records it, on either path, so no call-thread anchor is
  built from a refused one. A token already released stays valid until the
  media server ends it, and the media grant re-assertion and eviction paths
  (the webhook on a participant's join and the voice reconciler) decide
  from occupancy and the mirrored voice only; both are the revocation
  slice's (W5d).
- **Mirror of the room actor.** The actor's affiliation list follows the
  canonical grants, never the other way: administration applies a change
  only after its commit, a change the actor did not apply (or that a
  request stopped after, or whose commit could not be read back) is
  repaired from the recorded grants in the background without overwriting a
  newer generation, invitation rollbacks do not write
  it, and a room re-seated after a claim move seats only occupants whose
  grants allow `read`, with their canonical standing.
- **Room-bound extension nodes of unmanaged rooms** are decided by the room
  as it stands: subscribe and event delivery need the channel's `view` and
  no ban, publication its `send_message`, whatever PubSub affiliation an
  earlier read left. A fan-out resolves the node's room and its enrollment
  once per event and asks each subscriber's right in that room.

### B4: lock order of the surfaces (G12)

- The gate's reads, the enrolled set's background reload and the actor's
  writer refusal take no row lock; they read the scope's guard row without
  `FOR SHARE`. A mutation never waits for them and they never wait for one.
  The actor's reader refusal reads no database at all.
- An affiliation change is a store transaction with the first slice's order
  (protocol epoch row shared, the scope guard exclusive, host-grant rows) and
  its bounds (3 s lock, 5 s statement, `Busy` after). It takes no ownership
  claim, ingress row or room actor ask; the handler asks the room actor only
  after the transaction resolved. The process-local room configuration lock
  the handler holds is taken before the store transaction and is not a
  PostgreSQL lock.
- The projection reconciliation of a recorded change runs on a task of its
  own after the commit, outside every lock, as do its retries, the mirror
  repair and the recovery pass a replica runs when it attaches.
- Fresh-manifest acceptance keeps the first slice's order; no surface of this
  slice takes the guard shared.

No surface holds a host-grant lock across an actor ask, an RPC, a queue wait
or a socket write.

### B3: projection of the writers this slice fences (G15, part)

The only fenced writer that records grants is the XEP-0045 affiliation
change. Each recorded change starts one reconciliation pass of its scope
(`HostGrantAuthority::reconcile`) without waiting for it; the reply reports
the recorded change. A pass that does not finish (the projection did not
answer, another operation got ahead, the store failed) is repeated after
1, 2, 4, 8, 16 and 32 s; a contradiction, and a scope that was retired,
exhausted or bound to another incarnation or policy version, is not
retried. A replica that attaches to the store runs one recovery pass
(`HostGrantAuthority::recover_pending`) and logs its outcome by kind. A
change whose passes all fail stays recorded and not effective until the
scope's next reconciliation; reductions are effective at their commit
regardless. No other writer writes
the projection, and no admission reads it.

### Rollout part of the writer fence (G24)

With no scope enrolled, these changes alter behaviour for every room:

- A PostgreSQL replica loads the enrolled set before it serves: startup
  retries the load every second for up to 30 s and exits if it cannot load
  it. The set is then reloaded every 5 s in the background, one read-only
  snapshot of two small relations.
- Every permission check, relation and subject listing of a channel or Space
  object is answered from that set in memory; it fails only while the set
  has never loaded. Legacy writes still read PostgreSQL once per object
  (first slice).
- Every surface the gate decides (joins, archive and pin reads, room
  disco#info, service and Spaces listings, channel search, owner and
  administration IQs, moderation, call setup, room system messages,
  invitations, Spaces publish and retract, Spaces PubSub and its fan-out,
  extension host tools, extension bots and state, admin channel and Space
  commands) decides an unmanaged room in memory, with no PostgreSQL read,
  and keeps its existing checks; it fails closed only while the set has
  never loaded. A room address that is not canonical and reaches no
  enrolled room keeps its existing checks too.
- A Spaces publish of an existing channel follows the placement rule (B2):
  a publish that names a Space the channel's link and parents already name,
  and no other, needs no more than before; otherwise the publisher is an
  owner, admin or manager of the channel, an owner or admin of one of its
  current parent Spaces, or the deployment owner, read from explicit
  relationships whatever the permission backend computes for `manage`.
- A Spaces items read no longer moves a channel: it backfills a linked
  channel's bookmark and parent only when no other Space is the channel's
  parent, and no longer retracts the channel's bookmarks from other Spaces
  or removes their parent relationships. A join fills a missing parent only
  from the channel's link, never from a single bookmark without a link, and
  never adds a second parent. A placement removes every other Space as the
  channel's parent.
- A replica whose enrolled set has not reloaded for 30 s answers not ready.
- Subscribing to, receiving events from and publishing to a room-bound
  extension node follow the room's current right (all rooms).
- The owner reseed no longer lifts a ban on any node; Owner seeds and
  deployment ownership are written only for accounts of the served domain.
  A Spaces node creation whose account domain yields no seed issuance
  policy seeds no owner, and an admin Space creation is refused before
  anything is written.
- `AppState::set_room_affiliation` is removed from `waddle-xmpp`.

A binary built before this slice decides managed rooms from the legacy
stores on these surfaces. Retiring such binaries before any scope is
enrolled is part of the rollout and activation runbooks (G24, G22).

Enrolling a scope (owner phase). A replica decides the scope's rooms as
unmanaged until its next reload of the enrolled set, one
`ENROLLED_SET_REFRESH_INTERVAL` (5 s) plus that reload after the enrollment
commits; only room-manifest acceptance fences them at once. The enrollment
procedure therefore enrolls with every serving replica running this slice's
binary, waits at least one refresh interval and one reload after the
enrollment transaction commits, and longer while any replica's
`waddle.host_grant.enrolled_set.age` gauge exceeds the time since the
commit or any replica logs that the enrolled set could not be refreshed;
a replica whose set is stale is already out of the serving set. Only then
does it rely on the scope: announce its rooms, make grants or retire the
legacy state of its rooms.

### Evidence of the writer fence

The suites, all under `crates/waddle-server/tests/`:

- `xep0045_host_grant_fence.rs` (PostgreSQL), on a WebSocket state wired as
  a PostgreSQL deployment (a fenced permission actor, one enrolled set
  shared with the gate, room actors with the members-only binding): joins,
  archive reads and disco follow the grants; administration is recorded in
  the store and writes no relationship, with D8 refusal, one change per
  request, member and mirror authority refused, owner configuration and
  destroy; invitations and Space PubSub grant nothing; legacy reads of an
  enrolled channel or Space answer nothing; durable membership is the
  admitted grants; the owner reseed keeps bans, skips enrolled Space nodes
  and refuses remote privileged seeds. Its `sinks` cases cover XEP-0425
  moderation by a stale mirror moderator, the pin list after a ban, a Muji
  presence refused to a seated occupant whose grant was revoked (and not
  reflected to another occupant) while a member's is, a room system
  message reaching only readers, channel search, Spaces and
  service discovery, Spaces retract, a managed room without a channel row,
  an unattached replica, extension bots, the extension host (a ban and the
  deployment owner), admin refusals including a Space-deletion cascade,
  admin listings, the group-DM writers, the read re-decision after a ban,
  a lost commit reply read back, a change overtaken by another recorded
  at the current generation, fan-out from an enrolled Space node delivering
  nothing whatever the request's PEP classification, the moderator role
  refused to a seated remote occupant, the topology bootstrap leaving
  an enrolled default channel and General Space alone, and the enrollment
  of a group direct-message room refused.
- `host_grant_enrolled_set.rs` (PostgreSQL): loading, refresh after an
  enrollment, a failed refresh and vanished relations keeping the set, the
  set's age growing while refreshes fail and resetting on a reload, a
  schema without the relations (never stale), non-canonical addresses, the
  gate without an attachment, and legacy reads before the first load.
- `spaces_pubsub_owner_seed.rs`: the owner seed's writes, idempotence, kept
  bans and refused subjects.
- `xep0503_channel_placement.rs`: the placement rule, including authority
  inherited from the channel's current Space, a destination Space owner
  without it, a move that leaves the destination the only parent even when
  the Space left listed no bookmark, and a link that disagrees with the
  channel's parent, which neither a Spaces items read of the linked Space
  nor a join with a bookmark left there turns into a parent.
- `xep0060_extension_route_room_access.rs`: room-bound extension nodes
  after a ban.

In `waddle-xmpp`, `tests/xep0513_mentions.rs` and the room chain's unit
tests (`protocol/room/inbox.rs`) cover a pre-dispatch channel-mention
decision replacing the role check; they are outside the focused set below
and were not run with it.

They run like the first slice's focused set (above), with this filter:

```sh
cargo nextest run -p waddle-server --features test-support --locked \
    --run-ignored all \
    -E 'binary(xep0045_host_grant_fence) | binary(xep0503_channel_placement) | binary(xep0060_extension_route_room_access) | binary(host_grant_projection) | binary(host_grant_enrolled_set) | binary(spaces_pubsub_owner_seed)'
```

`xep0045_host_grant_fence` and `host_grant_enrolled_set` need PostgreSQL
only, and each case creates and drops its own database;
`host_grant_projection` needs both stores; `xep0503_channel_placement`,
`xep0060_extension_route_room_access` and `spaces_pubsub_owner_seed` run
on the in-memory test state. The set was last executed on 2026-11-15 at
commit `eb73991da`, against PostgreSQL 17.11 and SpiceDB 1.56.1 (memory
datastore) with nextest's default parallelism, and passed 62 of 62 cases.
Only this document has changed since that commit.

The recovery pass a replica runs when it attaches is covered by the first
slice's suites (`xep0045_host_grant_admission.rs`, `host_grant_projection.rs`).
Not covered by a test: the call-token gate and the call-thread anchor (the
fenced state configures no media server; the Muji refusal precedes the
update the anchor is built from), the media grant re-assertion paths, the
mirror repair and its generation check, claim-move re-seating and the
invitation rollbacks (they need actor-failure injection or clustering), the
reconciliation retries, the host tool's item-read refusal (it needs a
loaded extension), the channel-mention decision end to end in a managed
room, the readiness answer and the gauge of a stale set, and the warning of
a reload that holds fewer objects. The library's pin case that now gives
its interpreter a WebSocket state (`ingress/room_pin_tests.rs`) is a
library test, which this lane neither compiles nor runs.

The acceptance-matrix rows marked W5c map to them as follows: "Delayed
repair or compensation by any other writer" to the administration and
reseed cases (the rollback is gone for managed rooms, and every other
compensation path is a refused legacy write); "Bootstrap, owner reseed and
actor restoration after revoke/Outcast" to the reseed and topology cases
and the banned member's join with a stale mirror; "Space, room or node
deletion and recreation" to the destroy, Space-deletion cascade and Space
PubSub cases (recreation of a scope is not built); "Privileged or moderator
grant ... written by bootstrap or repair" to the reseed case. None of them
yet uses two replicas; that is verification step 4.

### Not decided here

- Occupant ejection and presence after a revoke (W5d).
- A PubSub admission identity for serving an enrolled Space (needed before a
  scope's Space is served; W7a depends on it), and with it what `view` of a
  scope's Space means: until then its node is described and listed to
  nobody.
- Remote principals at these surfaces (W5e, W6a).
- Recreation of a scope, a room or a node under a new lifecycle.
- A deployment-wide policy guard; the first slice's community generation
  still does not fence a changed deployment override.

## Blocking review ledger

Slice names refer to [the federation plan](../../../docs/planning/federation-2047.md),
whose gate table also names the verification step that discharges each entry's
evidence.

| ID | Unresolved decision/evidence | Work blocked | Under ADR-0020 |
| --- | --- | --- | --- |
| B1 | Exhaustive scope writer/reader exclusion, old writer retirement and concrete store capabilities | Enrollment and production authority integration. | D5 condition, gate G11; open. Decided by W5b (store capabilities, credential separation: [recorded](#b1-store-capabilities-and-credential-separation-g11)) and W5c (writer inventory and exclusion: [recorded](#b1-writer-and-reader-exclusion-g11)); old binaries and credentials are retired under the rollout runbook (G24) and the activation runbook (G22). |
| B2 | Complete action/negative/admin/global dependency policy, actor projection, XMPP request/result identity | Affected protected operations and handlers. | Gate G14; open. Decided per protected surface by W5b, W5c and W5e. Includes outcast precedence in every channel and message action, including an author's own edit and delete (W5o), and the D8 rules: privileged grants and room moderation for remote principals only from trusted homes and only on `pkix` streams, and a grant that is not honored contributing nothing. W5b's part is [recorded](#b2-policy-for-the-action-this-slice-protects-g14-part), and W5c's [as well](#b2-policy-for-the-surfaces-this-slice-protects-g14-part). |
| B3 | CAS/marker/schema protocol, bounded reconstruction, missing-marker and large-graph rules | Projection integration beyond independently proven bounded operations. | Gate G15; open. Off the first slice's safety path, which admits in SQL. W5b decides the marker protocol for the projection writes it makes ([recorded](#b3-marker-protocol-for-this-slices-projection-writes-g15-part)), W5c extends it to the writers it fences ([recorded](#b3-projection-of-the-writers-this-slice-fences-g15-part)), W5o decides the fenced schema transition and W5r bounded reconstruction. Any admission that reads the projection needs B3 decided first; no slice in the plan adds one. |
| B4 | Actual claim/authority/child lock order, atomic admission and cancellation/lost-commit ownership | First manifest integration. | D5 condition, gate G12; open. Decided by W5b ([recorded](#b4-lock-order-atomic-admission-cancellation-and-lost-commits-g12)) and extended by W5c for each surface it fences ([recorded](#b4-lock-order-of-the-surfaces-g12)). |
| B5 | PubSub/DirectFrame admission identity; read/join/SM sink contracts | Those sinks and community activation. | Gate G16; open. Decided by W5c ([recorded](#b5-sink-contracts-g16)); W5d's #2056 control is its evidence. |
| B6 | XML-decoded pre-parse enforcement, canonical JID/account-binding agreement, isolated codec and authoritative reconciliation; ASCII test profile and full PRECIS/IDNA migration unresolved | External identities and enrolled authority keys. | D5 condition, gate G10; open. W1 covers only the codec part; W5i decides the rest, including the canonical domain form, and W5a applies it at S2S ingress. |
| B7 | Restore incarnation surviving rollback and excluding old deployment/work | Recovery capability and pilot activation. | D5 condition, gate G13; open. Decided by W5r. |

The coarse fence's availability cost is a non-blocking tradeoff once bounded
resources and independent progress are proved. Fine-grained fences can follow
only after complete policy/mutation dependencies are demonstrated. Merging this
document, accepting D5 or passing an inventory scan discharges none of these
blockers.
