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

The restore slice (W5r in the plan, condition B7) is implemented as well
(`permissions/host_grant/restore` and `witness`, migration 2002, the
`waddle-server host-grant` operator subcommand). Its decisions are recorded
under [Restore incarnation decisions](#restore-incarnation-decisions), and
its operator procedure is the
[restore runbook](../../../docs/operations/authz-host-grant-restore.md). Its
suite has run against disposable stores (see
[Running the restore slice's cases](#running-the-restore-slices-cases));
verification step 4 has not.

The revocation slice (W5d in the plan) is implemented as well: the
acknowledged revocation fence (`permissions/host_grant/fence`, migration
2005), the sweep of seated occupants (`server/host_grant_revocation`), the
ledger of retired account JIDs (`auth/retirement`, migration 2006) and the
operator acts that go with them. Its decisions are recorded under
[Revocation fence decisions](#revocation-fence-decisions), and its focused
set has run against disposable stores (see
[Running the revocation slice's focused set](#running-the-revocation-slices-focused-set)).
With no scope enrolled it changes, beyond its migrations, what
[its rollout part](#rollout-part-of-the-revocation-fence-g24) lists.

The effect-boundary slice for remote principals (W5e in the plan, gate
G17) is implemented as well (`permissions/host_grant/principal.rs`, the
`[host_policy]` table of the S2S snapshot). Its decisions are recorded
under [Remote principals at the effect
boundary](#remote-principals-at-the-effect-boundary-g17); its suite has
run against disposable stores (see
[Evidence of the effect boundary](#evidence-of-the-effect-boundary)).

The inbound MUC slice (W6a in the plan) is the first producer of stream
evidence: remote principals reach a managed room's join, archive,
discovery and self-ping, its acceptance and its administration
(`muc#admin`, `muc#owner`, XEP-0425 moderation), on the evidence of their
streams, and a seated remote occupant is decided on the evidence its seat
was admitted under. A managed room is reached at all only while its
`federated` setting is on. What that changes here is recorded under
[Seated occupants, roles and media](#seated-occupants-roles-and-media-g14)
and [Where it is decided](#where-it-is-decided); the rest is in
[the routing design](s2s-routing-2047.md#inbound-muc-w6a). Its cases are
written and have not been run.

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
paths (the revocation slice, W5d; see
[Non-reuse of retired account JIDs](#non-reuse-of-retired-account-jids-d8)). Other homes cannot be required to. By
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
| Stored privileged grant for a remote principal: trusted home on `pkix` and on dialback-only streams; home removed from the list; home never listed, grant stored outside the canonical path | Honored only for a listed home on `pkix`; otherwise ignored, whatever is stored. | W5e (steps 4, 6 and 7) |
| Room moderation by a remote principal: trusted home on `pkix` and on dialback-only streams; home not on the list, on `pkix`; a message author's own edit and delete | Moderation honored only for a listed home on `pkix`; the author's own edit and delete are not moderation and are not restricted by it. | W5e (steps 4, 6 and 7) |
| Former admin of a home removed from the trusted-home list, holding no separate member-level grant | Cannot moderate or read through that grant: an unhonored grant contributes no permission. | W5e (steps 4, 6 and 7) |
| Work from a remote domain decided after it stopped being admitted (removed from the allowlist, a parent domain denylisted, `closed` mode, S2S off), including work queued before the change | Refused at the decision; its grants contribute nothing, member-level grants included. | W5e (steps 4, 6 and 7) |
| Remote member on a dialback stream, with host policy requiring `pkix` | Its member-level grant contributes nothing; on `pkix` it is honoured. | W5e (steps 4, 6 and 7) |
| Trusted home whose snapshot requires a private-CA profile, on a public-profile `pkix` stream and on a dialback stream whose certificate verified under the private CA | Privileged grant honoured only on the private-CA `pkix` stream. | W5e (steps 4, 6 and 7) |
| Remote initiator of an affiliation change: trusted home on `pkix` and on weaker streams, home removed from the list | Acts with its honoured standing only on the trusted home's `pkix` stream while the home is listed. | W5e (steps 4, 6 and 7) |
| Issuance, bootstrap and repair under the S2S snapshot the replica runs | Privileged grants issued to accounts of its trusted homes; none while S2S is off; a trusted home that is a service domain refused. | W5e (step 4) |
| Outcast on every action the SQL policy protects, including channel `moderate`, `manage` and `delete` and an outcast author's own message `edit` and `delete` | The SQL policy evaluates outcast for every action it protects, and outcast wins each at this slice's admission. | W5b |
| Acknowledged revoke across two replicas with queued work at joins, protected reads, PubSub and stream-management fanout and room administration | No new protected admission under the revoked grant after the fence, on any replica. | W5d |
| Retired account JID on a Waddle home | Never issued to a new holder through any provisioning path. | W5d |
| Groupchat planned by the room planner for a revoked member, a member of the parent Space without a room grant, and a seated account without a grant | The acceptance is reduced to the admitted audience: none of them is routed, projected, pushed or notified anything, nothing is recorded for a later replay to deliver, and the admitted members receive the message. | W5b |
| Plan that holds work a managed room does not account for; room address that is not canonical | The stanza is refused with a committed error of type `cancel`; a sender who is not admitted is told only that. | W5b |
| Stored owner or admin grant of a remote account at this slice's admission and as a mutation's initiator, on a replica with S2S off | Contributes nothing, member-level access included, and does not count as the owner a room keeps; with S2S off a member-level grant of a remote account contributes nothing either, since no remote domain is admitted (with S2S on, the W5e rows). | W5b |
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
the scope as exhausted. A serving replica learns of an enrollment from the
first load of its enrolled set that starts after the enrollment commits, and
the enrollment procedure waits until every serving replica reports such a
load before the scope is relied on ([enrolling a
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
9. the scope's queue lock, shared (an advisory lock; see
   [The queue in front of the guard](#the-queue-in-front-of-the-guard-g12)),
   then **the scope guard, shared**, and under it the room binding, the
   authority incarnation and the room's grant rows, read without row locks;
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
refuses the write); the scope's queue lock and then the scope guard, both
exclusive; then host-grant rows only.
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

Known limit of the row lock: PostgreSQL does not queue share lockers behind a
waiting exclusive locker of a row. A continuous overlap of acceptance
transactions in one scope could therefore make a mutation of that scope time
out repeatedly: never an acknowledged revoke that did not commit, but a
delayed one. The revocation slice (W5d) puts a queue in front of the guard
([The queue in front of the guard](#the-queue-in-front-of-the-guard-g12)),
which removes the limit.

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

Remote principals at this boundary (ADR-0020 D1 and D8). Whose grants
admission honors is decided by the principal's admission witness, as
recorded under [Remote principals at the effect
boundary](#remote-principals-at-the-effect-boundary-g17): an account of a
served domain is honored as stored; a remote principal's grants contribute
nothing unless its domain is admitted under the S2S snapshot the replica
runs, and its `owner` or `admin` grant, the only privileged or moderator
affiliations this slice holds, only for a trusted home on a `pkix` stream.
A grant that is not honored yields neither `moderate` nor the `read` and
`send_message` the table would derive from it. The sender is decided with
the evidence of how its request arrived, and every named principal without
stream evidence. The served account domains and the S2S snapshot are the
replica's principal policy: admission takes it from its attachment (B1) and
a mutation from its issuance policy, both built from one value, so issuance
records an owner or admin grant as `local` exactly for the accounts
admission honors as served. A replica that serves no account domain honors
no privileged grant of a local account.

A mutation's initiator is an account whose authority is evaluated from the
state under the guard, in the transaction that records the change: only an
effective owner grants or removes owner and admin standing, an effective
owner or admin changes other standing, and a room always keeps an owner who
can administer it: one whose standing is recorded, effective and honored. A
standing that was revoked, demoted or not yet activated authorizes nothing,
nor does a standing the initiator's witness does not honor (above), and a
command decided under an earlier generation is refused as stale first. The
owner a room keeps is an owner of a served account domain: a remote
owner's standing is honored only on some streams and only while the
snapshot lists its home, so it is never counted. An enrollment is refused
unless every room starts with an owner of a served account domain.

Issuance to remote accounts (ADR-0020 D8). `IssuancePolicy` takes the domains
the deployment serves and the trusted-home set as inputs. An owner or admin
affiliation for an account whose domain is neither served here nor an exact
member of that set is refused before anything is recorded; member-level and
negative affiliations are issued to any account; an identity of a local
service is refused as a subject. The accepted basis (`local`, `trusted_home`,
`unprivileged`) is recorded with the grant. A replica's issuance policy
takes the trusted-home set from the S2S snapshot it runs (none while S2S is
off). A grant recorded for a trusted home is honored as described above: on
that home's `pkix` stream, while the home stays admitted and listed.

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
- The migrations after the baseline are numbered 2001 to 2013 without a
  gap. The runner refuses a database that recorded a later version but not
  an earlier one, so a skipped number could never be used once a later
  migration ships; the catalog's own test requires every version to follow
  the one before. Before the first release that carries them, the account
  traffic, Spaces and session-ending migrations were renumbered from 2010
  to 2013 and 2015 to 2009 to 2013. A database that a build from before
  that renumbering migrated (a development or test database; none shipped)
  records versions that now name other migrations, with other checksums,
  and every later startup refuses it: recreate it.
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
2026-11-15 at the writer fence's commit `ef5205cf2` (below), against the
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
recorded and effective affiliation, outcast winning, and each grant
honoured as its subject's admission witness says ([Remote principals at the
effect boundary](#remote-principals-at-the-effect-boundary-g17)).
`HostGrantGate` (`permissions/host_grant/gate.rs`) is that decision
outside room-manifest acceptance. For a room it answers:

- *unmanaged*: the replica's enrolled set (below) does not name the room,
  and the surface keeps its existing checks. Nothing is read from
  PostgreSQL for it;
- *managed*: the room's canonical grants, read in one `REPEATABLE READ`
  snapshot that takes no row lock (`HostGrantStore::room_grants`), bounded
  by 500 ms for a lock, 1 s for a statement and 1.5 s for the whole read,
  together with the replica's principal policy (the account domains it
  serves and the S2S snapshot it runs);
- *unavailable*: the enrolled set has not loaded, the room's address
  reaches an enrolled room in a form that is not the identity profile's
  canonical form, the grants could not be read within the bounds, or the
  replica holds no verified attachment (B1). Every surface fails closed on
  it. A room address that is not canonical and reaches no enrolled room is
  unmanaged.

The attachment is the one room-manifest acceptance uses: `HostGrantGate`
reads it, at each decision, from the replica's `HostGrantAttachmentGate`
(`permissions/host_grant/runtime_attach.rs`), and only while that gate is
attached (`HostGrantAttachmentGate::verified_attachment`, which reads the
state and the attachment under one lock): the runtime role's privileges and
the restore fence were verified and nothing has found the authority changed
since (see [Restore incarnation decisions](#restore-incarnation-decisions)).
In every other state every surface answers *unavailable* for managed rooms
until the gate attaches again: before the first verification passes; when
the gate drops the attachment (a refusal of the whole authority, a role
that no longer matches the profile, passes that cannot complete); and while
it verifies a changed authority again after an acceptance found the binding
changed, and after that verification failed. In those last two states the
gate still holds the earlier attachment, because acceptance compares its
binding with the one it reads and refuses on the difference; the surfaces
compare no binding, so they do not read it. The XEP-0045 administration
writers stop with them: an affiliation change is not recorded, and the
background reconciliation of a recorded one runs no pass, while the gate is
not attached. A replica binds the surfaces' gate to its attachment gate
once at startup; a second, different attachment gate offered to it leaves
every managed room *unavailable*, and the replica then does not verify its
own gate either. The projection recovery pass a replica runs at startup
waits for the first attachment, so nothing is projected while the fence
refuses the authority. A closed scope (restored, reconstructed, fenced)
admits nothing on these surfaces either (`RoomGrants::scope_admits`).

Two differences from acceptance remain, and closing both is a precondition
for enrolling the first scope:

- *Per-decision binding comparison.* The surfaces trust the attached state
  rather than comparing the attachment's binding with the authority each
  decision reads. A PostgreSQL restart, failover or relation recreation
  under the running replica is therefore noticed by these surfaces only
  once an acceptance or a periodic fence pass has noticed it; until then
  they decide from the changed authority. Each decision has to read the
  binding in its snapshot and refuse on a difference, as acceptance does.
- *Rooms of an enrollment only SpiceDB holds.* The surfaces serve a room no
  enrolled scope manages without asking the attachment gate whether it
  belongs to an enrollment the restored database lost
  (`HostGrantAttachmentGate::refuses_unmanaged`); acceptance refuses such a
  room. Each surface has to consult it for an unmanaged room.

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
  retired scope's objects stay enrolled. A load takes its start time before
  it reads, and reads one snapshot taken after that start, so the set it
  installs holds every scope whose enrollment committed before the start.
  A replica decides a newly enrolled scope's rooms as unmanaged until a
  load that started after the enrollment committed has succeeded there:
  one refresh interval plus one reload while reloads succeed, and no bound
  while they fail. A reload is one snapshot of three statements under the
  read bounds above, so it ends or fails within them. Room-manifest
  acceptance reads the bindings in its own transaction (first slice) and is
  not delayed. A load that finishes after one that started later installs
  nothing; the set and its start are installed together.
- A reload that holds fewer objects than the set it replaces is applied
  and logged at warn with the number of rooms, channels and Space nodes it
  no longer holds, and nothing that names them.
- Each successful load records when it started as the gauge
  `waddle.host_grant.enrolled_set.load_started` (unix seconds, no
  attributes). The load itself records it, so its last value keeps being
  exported while later reloads fail or hang, and `time()` minus it is the
  set's age whatever the refresh task is doing. That holds under
  cumulative temporality, the OTLP exporter's default; a deployment that
  sets `OTEL_EXPORTER_OTLP_METRICS_TEMPORALITY_PREFERENCE` to `delta` gets
  the value only from the collection after each load. Each refresh tick that
  completes also records that age as `waddle.host_grant.enrolled_set.age`
  (seconds, no attributes) and, once a set that holds the host-grant
  relations is older than 30 s (`ENROLLED_SET_STALE_AFTER`, six refresh
  intervals), logs a rate-limited warning. A reload that finds the
  relations gone after a load keeps the set and its start: it does not
  count as a fresh load. Operators alert per replica when the age
  computed from `load_started` exceeds that bound or the replica reports
  no `load_started` series.
- The set's age does not affect readiness. While reloads fail, an
  unmanaged room keeps its legacy checks and a managed room reads its
  grants from PostgreSQL per decision and fails closed when it cannot; a
  replica taken out of service for an old set would refuse the unmanaged
  rooms too. What an old set can do is decide a scope enrolled since as
  unmanaged, and the enrollment procedure (below) does not rely on a scope
  until every serving replica has loaded it.

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
| XEP-0045 role change and moderator role | Authority from the store (`moderate`); the moderator role is granted only to an occupant whose witness honours room moderation, decided without stream evidence, so to served accounts. Roles are occupancy, not grants. |
| XEP-0045 owner configuration (`muc_owner_config.rs` channel owner tuple) | Store authority (`manage`); writes no owner relationship; members-only is kept. |
| XEP-0045 destroy | Refused: no room affiliation confers `delete`; a managed room ends when its scope is retired. |
| Mediated invitation (`message/muc_invite.rs` member grant, its rollback and rebuild) | Refused when planned; an invitation accepted before enrollment is delivered without granting membership. Its rollback writes neither the legacy relation nor the room actor's list of a managed room, or of a room whose management cannot be told. |
| Group-DM invitation rollback (`message/group_dm_invite.rs`) | Refused for an identifier that is a managed room's channel, or whose enrollment cannot be told: it writes nothing there. |
| Room actor durable affiliations (`muc_durable`, `ChangeAffiliation`, `ApplyAdminItems`, resolver sync) | Out of scope as grants: for a managed room the actor's list is a mirror written only from canonical decisions (join resolution, administration after its canonical commit, and the repair below). Admission never reads it. |
| Mirror repair (`iq/muc_admin_host_grant.rs` `MirrorRepair`) | Store-derived: armed before a request records its first change, and kept unless the room actor applied every change or the request was refused before the store could hold any of them (so a batch refused part-way and a commit whose outcome could not be read back are repaired too). Each changed subject's entry is then set to its recorded grant in the background, after 0.25, 1, 4, 16 and 60 s until an attempt succeeds. An attempt applies one generation of the grants and reads them again, applying the newer values while the generation moves (at most five reads: the first and up to four after applying), so it never leaves an older value over a change applied since. |
| Claim-move re-seating (`clustering/route_bridge/mailbox.rs`) | Gate: a recovered occupant is seated again only while its grants allow `read`, with the standing they honour and the moderator role only with `moderate`; the room is not seated when the grants cannot be read. |
| Room actor configuration (every actor the room registry spawns) | Members-only binding: an actor spawned for a room the enrolled set names, or while it is unknown, keeps `members_only` through its creation spec, durable restores and configuration changes (`RoomActor::with_members_only_binding`, asked through the durable membership source). |
| Admin channel commands (`admin/channels.rs`: update, delete, set-affiliation, kick, occupants, affiliations, creation in an enrolled Space) | Refused. |
| Admin Space commands (`admin/spaces.rs`: update, delete and its cascade, members, set-role, create on an enrolled node) | Refused; a Space deletion whose cascade would reach a managed room is refused whole. |
| Group-DM member mirror and creation (`admin/channels.rs` direct SQL on `permission_tuples` and its actor pair; `run_group_dm_create`) | Refused before either statement for an identifier that is a managed room's channel, or whose enrollment cannot be told; a creation is refused before its room actor and catalog row are created. |
| Spaces publish, retract and bookmark cleanup (`iq/spaces_publish.rs`, `iq/spaces_retract.rs`, `iq/spaces_bookmark_cleanup.rs`) and channel-space links | Refused for a managed room and for an enrolled Space node. Placing any other channel follows the placement rule (B2), and a placement removes every other Space as the channel's parent, including one that listed no bookmark of it. |
| PubSub on the Spaces service (`iq/pubsub_dispatch.rs`, `iq/pubsub_admin.rs`: create, configure, delete, purge, affiliations, subscriptions, publish, retract, items, subscribe) | Refused for an enrolled Space node; leaving a subscription is kept. Request kinds the build does not classify are refused on the Spaces service. |
| Owner reseed (`spaces_pubsub_seed.rs`, from topology, node creation and admin Space creation) | Retired as an overwriting writer for every node: an existing outcast is kept, in one conditional statement. Enrolled Space nodes are never seeded. |
| Topology bootstrap (`server/topology.rs`: channel rows, General Space, bookmarks, parent tuples, room actor configuration) | Refused for managed rooms and an enrolled General Space; managed rooms' actors are opened members-only. |
| Join parent repair (`presence/muc/access.rs`) | Refused: a managed room is resolved from the store before any repair. For any other channel a join fills a parent only when the channel has none, its link names the Space and that Space lists its bookmark; it never adds a second parent or removes one. |
| Spaces items backfill (`iq/spaces_items.rs`) | Refused for an enrolled Space node (PubSub refusal). In any other Space a linked channel is skipped unless the enrolled set decides it unmanaged: a managed room's bookmark is never published there, nor while its enrollment cannot be told. For any other channel a read backfills a bookmark and a parent only when no Space other than the read one is the channel's parent; it never removes a parent or another Space's bookmark. |
| Extension route reads (`iq/extension_route_items.rs` member affiliation on room-bound nodes) | Refused for a managed room: it serves no extension state. |
| Extension PubSub publication (`extension_commands/pubsub.rs`) | Refused for a managed room's channel, and for one whose enrollment cannot be told: such a room serves no extension state. |
| Extension bot seating (`interpret/bot.rs`) | Refused: no grant is an extension bot's, so a bot is never seated in a managed room. |
| Legacy permission reads (`CheckPermission`, `ListRelations`, `ListSubjects`, `LookupSubjects`, `LookupResources`) | Answer nothing for an enrolled object, decided from the enrolled set; fail before it has loaded. |
| Extension host tools (`extension_host_adapter`: room send, archive query, member and channel lists, Space list, PubSub item reads) | Gate, with no deployment-owner fallback; an enrolled Space node is left out of the Space list, and an item read of a node bound to a managed room, or to a room whose management cannot be told, is refused. |
| XEP-0425 moderation, call setup, Muji call presence, channel-wide mentions, pin list, room system messages | Gate (B2). |
| Media grant re-assertion and eviction of a live call participant (`sfu_voice_reconcile.rs`, `websocket/muc_call_sfu.rs`, `livekit_webhook.rs`) | They push the media grants a seated occupant's mirrored voice derives, evict a participant who is no longer an occupant, and mint no token. Since the revocation slice the join webhook and its relayed form decide a participant from the grants first, and the revocation sweep ends a removed occupant's media ([Seated occupants, roles and media](#seated-occupants-roles-and-media-g14)); the periodic reconciler follows occupancy. |
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
| Service disco#items, Spaces disco#items | `view`, as the room's `disco#info` decides it: the MUC service lists a managed room whose catalog row is public only to a subject its grants allow `view`, read by the channel binding the enrolled set named, and a Space node's `disco#items` lists a managed room's bookmark only to such a subject. For the MUC service's listing, and a Space node's listing for a session, each grant that allowed `view` is confirmed with `still_allows` just before the answer is built, and a room not decided and confirmed within the listing's time is not listed: a session is then answered with `internal-server-error` (type `wait`), and a remote asker with the rest and no count. A remote principal's Space node listing decides each room once, through the gate's `allows_as`. A room whose grants cannot be read is not listed, and the listing tells no count. An enrolled Space node is never listed, and its own `disco#items` is not found ([MUC fallback](s2s-routing-2047.md#muc-fallback-w7b)). |
| Affiliation change | The store's initiator rule (first slice): an owner grants and removes owner and admin standing, an owner or admin changes other standing, the room keeps an owner who can administer it, and issuance refuses an owner or admin grant to an account outside the served domains and the trusted homes. One change per request. |
| Affiliation lists | The member list to anyone with `read`, every other list to a subject with `moderate`; the lists are the recorded grants. |
| Role change | `moderate` |
| Owner configuration | `manage` |
| Destroy | `delete`, which no affiliation confers |
| XEP-0425 moderation | `moderate`; the occupant role the room actor holds decides nothing. |
| Call setup (LiveKit token) | `send_message`, at every decision that leads to a minted token: the setup on each replica of a relayed initiate and the re-check before the token is released. The media capabilities still follow the voice the room actor mirrors, which can only narrow them. A live participant's media grants are decided from the grants as well, since the revocation slice ([Seated occupants, roles and media](#seated-occupants-roles-and-media-g14)). |
| Muji call presence (starting, joining or preparing a call, and the call-thread anchor it starts) | `send_message`, on the local path and the relayed ordered delivery alike; unreadable grants refuse it with `internal-server-error`. Leaving a call is always accepted. |
| Channel-wide mention (XEP-0513 `#channel` push) | `moderate`, decided before dispatch for a message that carries a channel mention and passed to the room chain; the role the room actor mirrors decides nothing, and unreadable grants broadcast nothing. A message without one reads no grants. |
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

Remote principals (D1 and D8). Every surface above honours a grant as its
subject's admission witness says, as the first slice's acceptance does
([Remote principals at the effect
boundary](#remote-principals-at-the-effect-boundary-g17)). A surface that
decides a local session's request, or a subject it names (an occupant, a
member, a mention), decides without stream evidence, so a remote subject
holds no privileged authority or room moderation there and the moderator
role is granted to served accounts only. Bootstrap and repair writers take
the trusted-home set from the replica's principal policy: the owner reseed
refuses Owner for an account outside the served domain and the trusted
homes of the S2S snapshot the replica runs, and account bootstrap
provisions deployment ownership only for an account of the served domain,
since owners are configured by localpart.

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
  that commits after the decision does not by itself eject an occupant
  seated by it; the revocation slice removes the seat and defines what an
  occupant still receives until then
  ([Revocation fence decisions](#revocation-fence-decisions)).
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
  is its evidence for unenrolled Spaces
  ([Sink contracts under the fence](#sink-contracts-under-the-fence-g16)).
- **Queued work.** The groupchat audience is the first slice's manifest
  acceptance. Invitations, which would queue a membership grant, are refused.
  Room effects of an administration request are produced by the room actor
  after the canonical commit, from the change that was recorded.
- **Room system messages.** The room's own message is archived like any
  groupchat; its audience is the room actor's seated occupants reduced to
  those whose grants allow `read`, decided after the actor's snapshot and
  before the route intents are captured, so a recovered delivery replays the
  reduced audience. When the grants cannot be read before the archive write
  nothing is archived or sent, and the caller's own retry applies (a
  call-thread ending is retried from its persisted fence). After the archive
  write the audience is decided again; a read that cannot decide is made once
  more, and when neither decides the message stays archived and is delivered
  to no occupant, with a warning that names only the kind of the failure,
  logged at most once a minute. A system message planned inside a
  room-manifest acceptance is refused by the first slice's plan reduction.
- **Call tokens.** A call is verified on the replica that receives it, on
  the room's owner when relayed, and again before the token is released;
  each verification asks the gate for `send_message`, so a revoke that
  commits before the last one withholds the token. A Muji presence that
  starts, joins or prepares a call asks the gate for `send_message` before
  the room actor records it, on either path, so no call-thread anchor is
  built from a refused one. A token already released stays valid until the
  media server ends it; the revocation slice ends a removed occupant's
  media and decides a participant that joins from the grants
  ([Seated occupants, roles and media](#seated-occupants-roles-and-media-g14)).
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
  parent and the enrolled set decides the channel unmanaged, and no longer
  retracts the channel's bookmarks from other Spaces or removes their
  parent relationships. A join fills a missing parent only
  from the channel's link, never from a single bookmark without a link, and
  never adds a second parent. A placement removes every other Space as the
  channel's parent.
- A replica's enrolled set is refreshed in the background and exported
  as the start of its last successful load; readiness does not depend on
  it.
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
unmanaged until a load of the enrolled set that started after the enrollment
committed has succeeded there; only room-manifest acceptance fences them at
once. The enrollment procedure therefore enrolls with every serving replica
running this slice's binary, records the enrollment transaction's commit
time, and waits until every serving replica's
`waddle.host_grant.enrolled_set.load_started` is later than that commit time
by more than the clock skew allowed between the replicas and the database
(the gauge has a resolution of one second). A replica that reports no value,
or a value at or before that point, counts as not fresh, whatever its
readiness says, and the procedure keeps waiting or takes that replica out of
service. Only then does it rely on the scope: announce its rooms, make grants
or retire the legacy state of its rooms.

Follow-up for the activation slice (optional): each replica records the start
of the load that installed its set against its node lease, and activation
waits until every live lease records a load that started after the
enrollment committed. That replaces the operator's reading of the gauge with
a check the activation transaction can make, and a replica without a live
lease cannot serve.

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
  service discovery, Spaces retract, a Spaces items read of a Space no
  scope enrolls that backfills an unmanaged room's bookmark and leaves none
  of a managed room (the end state only: the suite's in-memory PubSub
  storage keeps no trace of a bookmark published and then retracted, so
  the case also passes without the enrollment check), a managed room
  without a channel row,
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
  set's age growing while refreshes fail and resetting on a reload, the
  load's start taken before its read and kept by a failed refresh and by
  vanished relations, a schema without the relations (never older than any
  bound), non-canonical addresses, the gate without an attachment, and
  legacy reads before the first load.
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
commit `ef5205cf2`, against PostgreSQL 17.11 and SpiceDB 1.56.1 (memory
datastore) with nextest's default parallelism, and passed 64 of 64 cases.
Only this document has changed since that commit.

The recovery pass a replica runs when it attaches is covered by the first
slice's suites (`xep0045_host_grant_admission.rs`, `host_grant_projection.rs`).
Not covered by a test: the call-token gate and the call-thread anchor (the
fenced state configures no media server; the Muji refusal precedes the
update the anchor is built from), the relayed ordered delivery of a Muji
presence (only the local path is driven; both call the same function),
the media grant re-assertion paths, the
mirror repair and its generation check, claim-move re-seating and the
invitation rollbacks (they need actor-failure injection or clustering), the
reconciliation retries, the host tool's item-read refusal (it needs a
loaded extension), the channel-mention decision end to end in a managed
room and its skip for a message without a channel mention, the refusal of a
group-DM creation under a managed room's identifier (a minted identifier
cannot be made to collide), the exported gauges and the stale-set warning,
the rule that a load finishing after a later-started one installs nothing,
the transient publish the Spaces backfill no longer makes (above), and the
warning of a reload that holds fewer objects. The library tests that cover the same
code (the pin and unpin cases whose interpreter holds a WebSocket state in
`ingress/room_pin_tests.rs`, and the join's parent repair from a link in
`server/routes/websocket/tests/muc.rs`) are neither compiled nor run in
this lane.

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

- Occupant ejection and presence after a revoke: decided by the revocation
  slice ([Seated occupants, roles and media](#seated-occupants-roles-and-media-g14)).
- A PubSub admission identity for serving an enrolled Space (needed before a
  scope's Space is served), and with it what `view` of a scope's Space
  means: until then its node is described and listed to nobody. The Spaces
  slice (W7a) did not build it: over S2S an enrolled scope's Space answers
  as a node that does not exist, whatever its `federated` setting says
  ([Spaces](s2s-routing-2047.md#spaces-w7a)).
- Remote principals reaching these surfaces over S2S: the join, the
  archive read, discovery, self-ping, the room's acceptance and room
  administration (the `muc#admin` lists and changes, the `muc#owner`
  configuration and XEP-0425 moderation) are reached through
  [inbound MUC](s2s-routing-2047.md#inbound-muc-w6a), and the decision
  they reach is recorded under [Remote principals at the effect
  boundary](#remote-principals-at-the-effect-boundary-g17). The other
  surfaces of this section (calls, pins, hats, voice requests,
  invitations, the Space's nodes and commands) are not reachable over
  S2S.
- Recreation of a scope, a room or a node under a new lifecycle.
- A deployment-wide policy guard; the first slice's community generation
  still does not fence a changed deployment override.

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
| Replica configured with another epoch than the recorded one | `epoch_mismatch` at every pass. Configured below the recorded epoch, the replica is not ready while an unretired scope is enrolled (`configuration_predates_restore`); configured above it, it stays ready, refuses managed rooms and names `configuration_ahead_of_restore`. Verifications that cannot complete do not replace `configuration_predates_restore`. | — | `a_replica_verifies_a_changed_authority_in_place`, `live_spicedb_a_restore_to_before_a_transition_is_refused`, `incomplete_verifications_keep_a_configuration_that_predates_the_restore` |
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
a committed denial) and the replica's `HostGrantAttachmentGate` starts one
verification (single-flight): it runs the fence, which reads the binding
before and after, and swaps the attachment for the one the fence produced.
Until then every managed room is refused, by acceptance and by every other
surface, which reads the attachment only while the gate is attached, and
while it fails the gate keeps retrying with the same backoff. A PostgreSQL restart or failover therefore
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

Readiness (`HostGrantAttachmentGate::readiness`, rendered by the readiness handler)
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
  read as the profile requires (`runtime_profile_mismatch`): usually the
  owner phase's grant matrix, shared by every replica; the periodic pass
  verifies again;
- three verifications in a row (`MAX_INCOMPLETE_PASSES`) that could not
  complete, the first at least 30 s earlier (`INCOMPLETE_READINESS_GRACE`):
  `spicedb_unreachable` when the last one failed on SpiceDB, otherwise
  `fence_incomplete`. Each verification that does not complete is counted
  once; attached, the third periodic pass drops the attachment. A replica
  whose configuration predates the declared restore stays not ready through
  them: only a verification that reaches the epoch shows it corrected.

A failure that is one replica's alone (its runtime role, or its own
network path to SpiceDB or PostgreSQL) keeps that replica ready as well:
it stays in the endpoint set and refuses every managed room while its
peers serve them. The gauge, which is 1 on that replica only, and its error
log are the signal.

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
   admission honours on any decision: lowering an owner or admin grant of
   an account this deployment does not serve, which every decision but a
   trusted home's `pkix` request honours as none (`policy::honoured_floor`),
   to the projection's `member` is recorded at `none`. An affiliation
   issuance refuses for its subject is recorded no higher than `none`;
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

## Revocation fence decisions

These are the decisions of the revocation slice (W5d in the plan): what a
revoke's acknowledgement promises and by when, the queue in front of a
scope's guard (G12), what happens to occupants a revoke leaves seated (G14),
how each sink contract of the writer fence stands once a revoke is
acknowledged (G16), and non-reuse of retired account JIDs (ADR-0020 D8). The
acceptance-matrix rows marked W5d are their evidence; the run is recorded
under [Running the revocation slice's focused set](#running-the-revocation-slices-focused-set).
They restate the bounds of the earlier slices they rely on and change those
slices' decisions only where stated.

Gate G18 (remote traffic to local accounts) is not decided here. The plan's
gate table gives it to W6c and to `s2s-account-traffic-2047.md`, and nothing
in this section admits or refuses a stanza between a remote sender and a
local account: the fence covers managed rooms and Spaces, and the ledger of
retired JIDs only keeps a JID from being issued again. What G18 can rely on
from here is that no account is provisioned for a retired JID from the
retirement's commit on (streams the account had open are not ended, see
[What stays open](#what-stays-open-1)), and that a revoked room grant admits
nothing new once its revoke is acknowledged.

### What an acknowledgement means

The linearisation point of a revoke, a ban, a demotion or the closing of a
scope is still the PostgreSQL commit of its operation. Two things follow
from the commit alone, as in the earlier slices:

- no room-manifest acceptance admits under the revoked grant: the
  acceptance takes the scope's guard shared and reads the policy state under
  it (first slice);
- no decision that reads the grants after the commit admits under it: every
  other surface reads the canonical grants from PostgreSQL when it decides
  (writer fence).

What the commit does not cover is a decision that was read before it and
whose effect has not happened yet, on any replica. ADR-0020 D5 does not
accept a prior permission check as admission, so the acknowledgement covers
exactly that (`permissions/host_grant/fence.rs`, migration 2005):

- **Lease.** A replica decides for a managed room only while it holds a
  lease: a row of `host_grant_fence_replicas` whose `lease_expires_at` it
  moves 5 s ahead (`FENCE_LEASE_TTL`) every 250 ms (`FENCE_TICK`). The
  replica measures the lease on its boot-time clock from an instant taken
  before the renewal was sent, and stops deciding 1 s
  (`FENCE_LEASE_MARGIN`) before the recorded end. The boot-time clock is
  `CLOCK_BOOTTIME` on Linux (`permissions/host_grant/boot_clock.rs`), which
  keeps counting while the host is suspended, so a host that sleeps through
  the end of its lease finds the lease ended when it wakes. Each of a
  tick's transactions is abandoned after 1.5 s (`FENCE_TICK_DEADLINE`),
  0.5 s (`FENCE_TICK_SLACK`) short of the margin and the grace together. A
  renewal whose answer arrives after the lease it renews ran out here does
  not revive that lease: the replica counts a lapse, releases the lease and
  registers again. It registers while its attachment gate is attached, the
  condition under which the surfaces decide managed rooms, and its enrolled
  set names a scope; it holds no lease otherwise, and releases its lease
  while the gate verifies a changed authority again.
- **Install, then record.** Each tick reads every scope's generation. A
  generation that moved is installed in memory under the lock every
  validity check takes, and only afterwards recorded in
  `host_grant_fence_acks`. A recorded generation never goes back.
- **Ticket.** Every decision the gate makes for a managed room carries a
  ticket: the registration it was read under and the scope's generation.
  The registration is taken before the grants are read. A ticket is current
  while the replica holds the lease of that registration and has installed
  no newer generation of the scope. `ManagedRoomGrants::allows_as` and
  `standing_as`, which every surface calls with the evidence of the
  request it decides, answer nothing for a decision whose ticket is not
  current, whatever that evidence and however long its caller held it, and the room actor asks the ticket again
  in the turn that seats an occupant or applies a role change (below). No
  check waits. A surface that holds a decision across an await (a read of
  the archive or the room actor, the room's configuration lock) asks it
  again after the await, and reads the grants again when its ticket is no
  longer current (`HostGrantGate::still_allows`, with the same evidence),
  before the effect.
  `FenceTicket::admit` runs a synchronous effect under the lock an
  installation takes, or not at all; the surfaces fenced so far have no
  such effect (theirs are replies, actor asks and durable writes) and use
  the checks.
- **Acknowledgement.** `HostGrantStore::await_revocation_fence(lifecycle,
  generation, limit)` reads, after the operation committed, every replica
  whose lease has not been expired for 1 s (`FENCE_EXPIRY_GRACE`), and
  answers `Acknowledged` once each of them has recorded the generation.

After the acknowledgement, no replica passes a validity check with a
decision read before the revoke. A replica that recorded the generation
installed it first. For one that did not, take the last renewal of its lease
that committed: the replica sent it at an instant `s` and stops deciding at
`s` plus 4 s on its own clock. PostgreSQL recorded the lease's end as its
own clock at the start of the renewal's transaction, which is not earlier
than `s`, plus 5 s, and the acknowledgement counts the replica 1 s beyond
that. So the replica stopped deciding at least 2 s before the
acknowledgement could stop waiting for it, provided the two clocks agree in
rate over a lease (below). A later renewal cannot make a decision read
before the revoke current again:

- A renewal that commits while the acknowledgement still counts the lease
  extends a lease the acknowledgement keeps counting: the replica records
  the revoke's generation, read in that renewal or the next, or its lease
  runs out.
- A renewal sent before the replica's own deadline commits within 1.5 s of
  being sent or is abandoned, so it commits at least 0.5 s before the
  acknowledgement can stop counting the lease it renews: that lease ends at
  least 1 s after the replica's deadline, and is counted 1 s beyond.
- An answer that arrives after the replica's own deadline, however the
  renewal fared in PostgreSQL, lapses the lease instead of renewing it, and
  the replica registers again.

A replica that registers later reads the grants after its registration
committed, which is after the revoke did. A replica whose lease ran out
registers again under a new identity; nothing it decided before is current
under the new registration, and it sweeps every scope before it is of use.

The acknowledgement depends on PostgreSQL alone: no step reads or writes
SpiceDB, waits for a room actor or calls the media server. It is given
within one tick while every replica runs, and within the lease and the
grace (6 s after the last renewal) when one stopped without releasing its
lease. A replica releases its lease when its fence stops; a process that exits
before that write is waited for like one that stopped. A caller waits at
most 10 s (`FENCE_ACKNOWLEDGEMENT_DEADLINE`); an acknowledgement that has
not arrived or cannot be read leaves the operation committed and is
reported as such (below).

What acknowledges through the fence: an XEP-0045 affiliation change of a
managed room replies after the acknowledgement, increases included; the
operator's `host-grant revoke` and `revoke-account` wait for it and report
it, `revoke-account` for every room the account holds a row in, including
rooms an earlier run revoked. A reply that is not acknowledged in time is `resource-constraint` with
the change recorded; the mirror repair is left armed. A retirement, a
restore transition, a fence closure and a reopen are owner-phase or
background acts that return at their commit; replicas install their
generations like any other, and `host-grant fence` reports a scope's
standing.

#### Already admitted and in-flight work

After the acknowledgement these may still complete. None of them takes a
new decision.

| Work | Why it may complete | Bound |
| --- | --- | --- |
| Children of a manifest accepted before the commit: routes, inbox rows, pushes, notifications and recovery items, their replay, and the resend of stanzas already in a session's stream-management queue | The first slice's accepted-child contract: frozen target and payload, no new recipient. A revoke does not erase them. | The existing replay and custody bounds; unchanged. |
| An effect whose validity check passed before its replica installed the generation: a response frame already built for the connection, an event already handed to a queue or a detached stream-management buffer, a token already released, a seat or a role change the room actor had begun to record | It was admitted before that replica recorded the generation, and so before the acknowledgement. | The request that carries it. A seat that completes after the installation is swept (below). |
| Presence the room actor broadcasts to a revoked occupant that is still seated, and that occupant's own presence without a call | Seating is not a protected effect (writer fence, B5). Everything else the occupant could do or receive is decided from the grants. | Until the sweep of the replica that hosts the room removes the seat: started when that replica installs the generation, retried every second. |
| A media session of a revoked participant | The media server holds it. | Ended by the same sweep; a participant that joins or reconnects is decided from the grants; the reconciler removes a participant that is no occupant. |
| Seats held by a replica whose lease ran out | That replica decides nothing, and no occupant of its rooms is admitted to anything new. | Until it registers again, when it sweeps every scope. |

Residual risks of the fence, accepted here:

- A validity check and the effect it guards are separate steps. Where the
  effect follows in the same poll of the task (a decision turned into a
  reply) nothing can come between them but the scheduler. Where an await
  lies between them (the room actor's durable commit of a seat or a role
  change it has checked, a frame handed to the connection) the replica can
  install a generation meanwhile; that effect is the in-flight work of the
  table above, and a seat is swept. A replica that is stopped for more than
  2 s between a check and its effect, and whose lease ran out meanwhile,
  can complete that one effect after the acknowledgement; it then holds no
  lease and decides nothing more.
- The margin and the slack assume the replica's boot-time clock and
  PostgreSQL's clock differ in rate by much less than a fifth over one
  lease. PostgreSQL's clock is the same clock for every replica only while
  the database stays on one host: a failover to a host whose clock is
  ahead, or a host clock that steps forward, ends every lease early by the
  difference, and a revoke can then stop waiting for a replica that still
  decides. PostgreSQL hosts are therefore kept in sync by NTP with an
  offset well under 1 s. Each registration and renewal compares the
  replica's wall clock with PostgreSQL's `clock_timestamp()`: the gauge
  `waddle.host_grant.fence.clock_skew` records the difference in
  milliseconds, and a warning, at most one a minute, is logged when it
  exceeds 500 ms (`FENCE_CLOCK_SKEW_WARNING`) by more than half the round
  trip. The comparison assumes the replica's own wall clock is in sync; it
  shows a database host that is not, and takes no part in the fence.
- On a platform without `CLOCK_BOOTTIME` the lease is measured on a clock
  that stops while the host is suspended, and the fence warns once when it
  starts there. Production replicas run on Linux.
- A replica that renews its lease and cannot record a generation (its
  acknowledgement write fails while its renewal succeeds) holds the
  acknowledgement back until the write succeeds; the caller is told after
  10 s that the operation is recorded and not acknowledged.
- The tick uses the main connection pool. A pool that is exhausted for more
  than 4 s costs the replica its lease, and its managed rooms answer
  unavailable until the next tick registers it again.

### The queue in front of the guard (G12)

The first slice left open whether the guard needs a fair primitive:
PostgreSQL lets a share locker join the share lockers of a row whatever
waits for the row exclusively, so overlapping acceptances of one scope could
keep a mutation of that scope waiting without bound. It does. Whoever takes
a scope's guard first takes a transaction-scoped advisory lock keyed by the
scope (`model::guard_queue_key`, the first eight bytes of a hash of the
lifecycle), in the same mode: an acceptance
`pg_advisory_xact_lock_shared`, a store transaction
`pg_advisory_xact_lock`. Advisory locks go through the lock manager, which
grants in arrival order: a shared request that arrives after a waiting
exclusive one waits behind it.

- Lock order: the queue lock is taken immediately before the guard, in
  both kinds of transaction, so the orders of the first slice hold with
  "the queue lock, then the guard" in place of "the guard". A transaction
  that takes several guards takes each pair in ascending lifecycle, as
  before. The batches of a restore transition take scope rows without the
  queue lock; they wait for the row locks as before and are not part of a
  revoke's path.
- Bounds are unchanged: an acceptance waits at most its 100 ms for the
  queue lock or the guard, a store transaction 3 s.
- Behavior change: an acceptance that arrives while a mutation of its scope
  waits no longer overtakes it. It waits behind the mutation and is refused
  with the `Timeout` class, nothing accepted, if the mutation has not
  committed within 100 ms. Before, it was admitted beside the acceptances
  that kept the mutation waiting.
- Two scopes whose keys collide wait for each other and nothing else.

### Seated occupants, roles and media (G14)

The room actor of a managed room mirrors the grants: who is seated, with
which affiliation and role. `server/host_grant_revocation.rs` puts that
back in line. The fence asks for a sweep of a scope whenever the replica
installs a new generation of it, and of every scope when the replica
registers. For each room of the scope that this process holds an actor for,
the sweep reads the grants as stored and applies, per account the actor
seats or lists:

| The grants say | The actor holds | The sweep |
| --- | --- | --- |
| Banned (recorded or effective outcast) | A seat | Sets the entry to outcast and removes every seat with status 301 (XEP-0045 §9.1). |
| No membership recorded | A seat | Sets the entry to none and removes every seat with status 321 (§9.4). |
| Membership recorded, and reading not admitted: the scope admits nobody (retired, restored, fenced), the grant is not honoured for the account (below), or the membership is a recorded increase that is not yet effective | A seat | Removes every seat with status 321 and leaves the entry. |
| No membership recorded, or banned | An entry above that, no seat | Sets the entry, which also refuses a join of that account the actor has not applied yet. |
| An affiliation below the mirrored one | Any | Lowers the entry to the recorded one. The role is derived again (an admin that became a member is no moderator), with the affiliation presence. |

The sweep reads the grants through `HostGrantGate::room_as_stored`, which
returns `StoredRoomGrants`: the recorded and effective affiliations and
what a principal would be decided as, with no ticket of the revocation
fence. It is a type of its own, so nothing that admits (a join, a read, an
administration request, the room's acceptance) can be given it; those take
`ManagedRoomGrants`, whose ticket is always the fence's.

Whether a seated account may stay is decided as admission decides it: from
the effective affiliation, for the principal the account is. A recorded
increase that is not yet effective admits nothing, so an occupant seated
while one is pending is removed although its recorded affiliation is a
membership. The sweep holds no request of the account, so which evidence
it decides with depends on what the replica knows of the seat:

- An account of a served domain is decided without stream evidence, as its
  own requests are.
- A remote occupant this replica seated over S2S is decided on the
  evidence of the stream each of its seats was admitted under, which the
  router records with the seat (`SeatEvidence`, read through
  `HostGrantGate::seat_evidence`; see
  [inbound MUC](s2s-routing-2047.md#inbound-muc-w6a)). It stays only if
  every one of its seats' evidence admits it to read under the snapshot the
  replica runs now. A trusted home's admin seated on a `pkix` stream
  therefore keeps its seat, and loses it when its home leaves the
  trusted-home list; with `require_pkix`, a member seated on a dialback
  stream is removed.
- A remote occupant for which the replica holds no seat evidence (one
  seated through the room actor from a record, or by a replica that has
  since stopped) is decided without stream evidence first, and kept if
  that admits it. Deciding without a stream honours less than a request on
  the account's own stream would: no owner or admin standing, and with
  `require_pkix` nothing at all. So where that decision denies, the
  occupant is removed only when the grants deny it whatever stream its
  requests arrive on: it is banned, the scope admits nobody, its effective
  affiliation is below membership, or its domain is not admitted. A domain
  is not admitted when S2S is off or when the snapshot the replica runs
  refuses the domain or a parent of it (ADR-0020 D1); no stream of such a
  domain admits anything, so the denial holds on every stream. What is left
  is the occupant whose only obstacle is the missing evidence: a trusted
  home's admin, or under `require_pkix` any remote member. It keeps its
  seat, and each of its own uses of the room is decided with the evidence
  of the request that makes it.

Lowering a mirrored entry to the recorded one (the last row) applies to
every account.

A room's own administration request applies its change to the actor
itself, with the administrator and the reason it names. The sweep does not
look at a room while such a request is under way on the same process: it
takes the room's configuration lock, which the request holds from before it
records the change until the actor applied it, and then finds the room in
line. It waits at most 1 s for that lock in one pass and comes back to the
room in the next, so a request that waits for its acknowledgement does not
hold up the other rooms. While a pass looks at a room it holds the lock
itself, so a request that administers or configures that room waits for the
pass to finish with it; every ask of the pass is bounded at 5 s. The lock is
held by one process: a request and a sweep on different processes are not
ordered by it. A sweep that reaches the actor first, on another
process or for a change that was not made through the room, removes without
naming an actor or a reason.

The removal presence goes to the removed occupant (with status 110) and to
everyone who stays. The removed sessions' media ends, and a changed voice is
pushed to the media server. The effects travel as the actor's other
administration effects do: through the room-effect outbox where the room is
durable, directly otherwise. A removal that keeps the affiliation
(`RemoveUnadmittedOccupant` in `waddle-xmpp`) exists because a list that was
corrected without the seat (the mirror repair) would otherwise leave the
occupant seated. The recorded owners are written into the actor's list
first, so the actor's own last-owner rule does not refuse a removal the
store decided.

- The sweep acts only on a read that shows the grants. While they cannot be
  read it changes nothing and tries again every second: a database that does
  not answer never empties a room.
- It never seats anyone and never raises an entry above the recorded
  affiliation. The first slice's and the writer fence's rule stands: the
  actor's list of a managed room is written only from canonical decisions.
- Only the process that holds a room's actor sweeps it. A replica that only
  relayed a join keeps its record of the remote membership; nothing reads it
  for a managed room (self-ping below).
- A closed scope empties its rooms. The occupants keep their recorded
  standing and enter again once the scope admits.

**Stale role between check and apply.** A request that changes roles only
records nothing in the store, so nothing revalidates its sender after the
grants were read. The room actor asks the decision's ticket in the turn that
applies the request (`ApplyAdminItemsIfAdmitted`) and applies nothing once a
newer generation is installed; the request is answered `resource-constraint`
and decided again when it is repeated. A join carries its ticket the same
way (`JoinWithAffiliationIfAdmitted`): the actor asks it before the join
changes anything, and a join that was overtaken is decided again like one
its own admission revision refused. A seat recorded while the generation
moved asks for another sweep of its scope. An affiliation change needs
neither: the store revalidates the initiator under the guard, and its own
commit is what moves the generation.

**Self-ping (XEP-0410).** The service advertises the self-ping
optimization, so its answer is authoritative. For a managed room it is
decided from the grants before occupancy, on whichever replica is asked,
and before the record a relaying replica keeps of a remote membership: an
account the grants do not admit to read is answered `not-acceptable`, "not
joined", whether or not its seat has been removed yet. While the grants
cannot be read the answer is `remote-server-timeout`, the one error
XEP-0410 §4 has a client take no decision from; any other error would have
every occupant leave and rejoin.

**Media.** A live call participant of a managed room is decided from the
grants (`send_message`) wherever its media grants are derived: the webhook
on a participant's join, on the replica that receives it or the room's
owner it is relayed to. An account the grants do not admit is removed from
the call, so a token released before a revoke does not regain a media
session. This changes the writer fence's rule that these paths follow
occupancy only; grants that cannot be read change nothing there.

### Sink contracts under the fence (G16)

The writer fence's contracts (B5) stand. Under the fence each reads as
follows; "decided" means from the grants, with a ticket.

| Surface | After a revoke is acknowledged |
| --- | --- |
| Join | Decided; the actor asks the ticket before it changes anything. An occupant seated before is removed by the sweep. |
| Archive pages, room disco#info, pin list | The decision after the read answers nothing once its ticket is not current, so a page read before the revoke is not sent on a decision made before it. A response already built is in-flight work. |
| Fan-out and stream-management fan-out from an enrolled Space node | Nothing is delivered (writer fence). |
| Fan-out from a Space node no scope enrolls | Not covered by the acknowledgement. The node's deliverable subscriptions are read again once per publication, in one statement, before its first hand-over (`PubSubStorage::list_deliverable_subscribers`), and the event is handed only to captured subscribers still among them, for a live queue and for the detached stream-management buffer alike. A list captured before a ban or an unsubscription hands nothing to the subscriber it removed: the #2056 schedule. The fence of such a change is its storage commit; a hand-over whose read preceded the commit can follow it. |
| Stream-management resend | Replays what a session's queue already holds: accepted children and events handed over before. It takes no decision and is not filtered. |
| Queued invitations | Refused when planned (writer fence); nothing is queued that could grant after a revoke. An invitation accepted before enrollment is delivered without granting membership, as before. |
| Extension delivery | A managed room serves no extension state and seats no extension bot (writer fence); an observer is dropped from each acceptance. Nothing is queued for an extension. |
| Room administration | An affiliation change is revalidated under the guard. A role change is refused by the actor once its sender's decision is overtaken. An owner's configuration change reads the grants again once it holds the room's configuration lock, which an administration request holds from before it records a change until the actor applied it, and asks that decision again just before the actor applies the change: a demotion acknowledged while the change waited refuses it (`forbidden`). A request waits at most 15 s for the lock (`ROOM_CONFIG_LOCK_WAIT`) and is answered `resource-constraint` after that. The role list is decided again after the actor's occupants were read; the affiliation lists are answered from the decision itself. |
| Call setup, Muji presence that starts, joins or prepares a call | Decided at each verification; a decision that is not current allows nothing. A live participant is decided from the grants. |
| Room system messages | The audience of a room's own message is decided before its archive write and again after it, and each occupant is asked again just before its copy is routed; a decision whose ticket is no longer current is read again for the occupants that remain. Grants that cannot be read after the archive write, in two reads, deliver nothing; the message stays archived and a rate-limited warning names only the kind of the failure. |
| An extension's archive query (`query_mam`), the reserved-nick node (XEP-0045 §7.12) | Decided before the read and again after it, as an archive page is. |
| Channel-wide mentions, moderation, search and discovery | Decided when sent. Some are decided before an await and acted on after it (see What stays open). |
| Claim-move re-seating | Decided once per claim epoch (writer fence). The re-seating asks for a sweep of the room once the seats are restored, and also when the ask that seats them failed or timed out, since it may have seated some; the sweep removes a seat the grants no longer admit. |

### Operator acts

`waddle-server host-grant` gains, beside the restore acts:

- `revoke`: unchanged in what it records; it now waits `--fence-wait`
  seconds (15 by default) for the acknowledgement and prints it. Exit code 1
  with `acknowledged: false` means recorded and not yet acknowledged; the
  same command with the same operation ID replays the revoke and waits
  again.
- `grants --subject`: every grant row an account holds.
- `revoke-account --subject [--ban]`: an operator's revoke or ban in every
  room where the account holds a row that differs, each under an operation
  identity derived from the act's, so the act is replayed as a whole. Every
  room the account holds a row in is reported on its own line with the
  fence's answer: at the generation of the revoke recorded now, or, where
  the row already holds what is asked for (`"unchanged": true`), at the
  scope's current generation, which covers the revoke an earlier run
  recorded. A repeated act therefore confirms every room. One room's error
  does not stop the others. The command exits as failed (1) when a room
  could not be revoked or its acknowledgement could not be read, then as
  refused (4) when a room's last owner of a served domain was refused
  there (`last_owner`), then as failed (1) when a revoke is recorded and not
  yet acknowledged. It is the act after a reported reassignment of a JID
  ([runbook](../../../docs/operations/authz-host-grant-reassignment.md)).
- `fence --scope`: whether the scope's current generation is acknowledged.

The subcommand is no replica: it holds no lease and waits for those that do.

### Non-reuse of retired account JIDs (D8)

A home never issues the JID of a retired account to a new holder
(`auth/retirement.rs`, migration 2006, both engines).

- **The ledger.** `retired_account_jids` holds the canonical bare JID of
  every retired account, the key `native_users.jid_key` uses. A row is never
  updated or deleted. Under the restricted runtime the runtime role may only
  read it.
- **Retirement** (`retire_account`, `waddle-server account retire`) is one
  transaction: it records the JID and removes the account's native
  credentials and its account row, with the identity-provider links and
  sessions that hang on it. A JID that holds no account is recorded as well.
  No account was retired or deleted by the server before this slice; the
  fixed-account bootstrap of test deployments resets credentials and is not
  a retirement.
- **Every provisioning path** reads the ledger where it would create the
  row:

  | Path | With a retired JID |
  | --- | --- |
  | Native registration (`NativeUserStore::register`) | The insert carries the condition; `JidRetired`. |
  | Provisioning at an identity-provider login (`IdentityService::create_user`) | The insert carries the condition; the name is passed over like a taken one, so the account gets the next free name. The same provider subject returning after its account was retired is provisioned as a new account under another JID. |
  | Session creation (`CreateAuthSession`), which inserts an account row for a JID that has none | The ledger is read in the transaction, after the insert; neither the row nor the session is created. This covers a login whose credentials were checked before the retirement. |
  | Fixed-account bootstrap | Goes through native registration and fails. |

  The owner localparts of the deployment configuration name JIDs, not
  accounts, and create no account; a retired JID named there holds what it
  is given and nobody can authenticate as it.
- **Exclusion.** On PostgreSQL the retirement takes `SHARE ROW EXCLUSIVE`
  on both account tables before it writes. An insert that has not committed
  finishes first and is removed with the account; one that arrives later
  waits for the commit and then runs against the ledger that holds the JID.
  SQLite runs one writing transaction at a time. Every writer of the
  account tables (provisioning, session creation at every login, changes of
  native credentials) that arrives while the retirement waits for the lock
  or holds it is held up until the retirement commits, rolls back or gives
  up. The wait for the lock is at most 2 s; one that cannot get the lock in
  time fails without having changed anything, the writers queued behind it
  proceed, and it is run again. Once it holds the lock the transaction has
  no deadline of its own: each of its statements runs at most 5 s. They are
  the ledger insert, the deletes of the account's rows (the delete of its
  `users` row cascades to its identity-provider links and its sessions; one
  delete for each row of the account, usually one native row and one
  account row), two reads of the account rows that carry no lookup key,
  the delete of the account's offline store (`pending_delivery`), taken
  after the advisory lock every insert into that store takes for the
  account's canonical JID, a read of the store's recipients that start
  with the account's localpart, compared without letter case, and a
  delete for each other spelling of the account's JID among them (a
  U-label domain, a row written before recipients were prepared), and the
  three deletes of what remote senders left for it: eleven statements for
  a usual account, so a login can be held up for 2 s of waiting plus up to
  55 s, and is in practice for milliseconds. The read of the recipients
  scans the store. From the commit on the store refuses every row for the
  account (`InsertOutcome::RecipientRetired`), whichever producer offers
  it: an insert that held the lock first commits before the delete reads
  the store, and one that waits for it reads the ledger the retirement
  wrote. The message is discarded and no sender is answered. An account
  whose delete does not finish within the statement bound is not retired;
  nothing splits that delete.
- **Ending the account's open sessions** (`auth/session_endings.rs`,
  `server/account_session_endings.rs`,
  `server/routes/websocket/account_retirement.rs`, migration 2013, both
  engines). `retire_account` leaves them; the operator command takes the
  second step once the retirement committed. It records a request naming
  the canonical JID. Every serving replica holds a lease row it registers
  when it starts serving and renews every 5 s for 15 s, on a task of its
  own. Every second a replica reads, in a read-only transaction, the
  requests of the last day whose JID the ledger holds and that it has not
  completed; it holds every requested account in its in-memory set of
  retired accounts first, then for each request records that it observed
  it, ends what it holds of the account and records the request as
  completed with what it ended. While nobody is retired that is one
  single-row update every 5 s and one select a second per serving process;
  the lease's expiry carries no index, so on PostgreSQL the update is
  heap-only:
  - every live stream it hosts is force-detached for the account's
    retirement (`ForceDetachOrigin::AccountRetirement`): the connection
    ends its binding as terminally as a replacing bind does (the session
    is closed, not detached), with `<not-authorized/>` instead of
    `<conflict/>`. No binding takes the full JID over, so the end is no
    transfer: a connection whose pumps are not seen to stop within the
    transfer join deadline does not take the node's terminal failure
    path, as a transfer's does. The origin is local to the replica that
    hosts the stream and is never relayed. The connection's teardown
    broadcasts the unavailable presence to local contacts, leaves local
    rooms (which fan the departure out to remote occupants) and remote
    rooms, and retires the binding. A stream counts as closed once the
    registry no longer holds its binding;
  - every stream-management session of the account that waits for a
    resume is ended: those held in memory through the invalidation and
    the displaced promotion a replacing bind uses, those of the durable
    authority under their claim. Both run the terminal path of an
    invalidated detached session (room leaves, unavailable presence). The
    promotion settles the session's acknowledgement windows, reads the
    ledger before any sink, so that what the session held for the client
    is discarded rather than redelivered or stored, ends the ingress
    custody of what it held as `Tombstoned` (the recovery of orphaned
    custody reads the ledger the same way and ends it so too), and
    deletes its durable rows and releases its claim under the captured
    fence. A session whose promotion fails is kept for a retry, and one
    whose claim another replica holds is left to it; the request stays
    incomplete until none is left. A live stream whose connection already
    has another detach queued ends by that detach; if that one keeps the
    session for a resume, the session is ended as a detached one in the
    same pass or the next poll, and a resume of it is refused meanwhile;
  - once no stream and no detached session is left, the pass checks that
    the account's offline store is empty;
  - from the moment the account is in the set, a binding or resume of it is
    refused before it is published (`<not-authorized/>`). For a minute after
    taking a request up the replica also closes any stream of the account
    it finds again, which covers a binding that checked the set just before
    the account was added.

  The command waits (default 30 s, at most 600 s) until every replica
  whose lease counts has completed or has stopped renewing its lease,
  judged by whether it saw the lease renewed within the lease's lifetime
  and a 2 s grace (17 s), on its own clock; the default wait is checked
  at compile time to exceed that. It reports each replica as
  `completed`, `lapsed` or `pending` and exits 3 when one is `pending`;
  a lapsed replica no longer serves the account's sessions and does not
  keep the command from exiting 0. A
  replica that starts within the day takes the request up too: it ends the
  detached sessions an earlier process left and refuses the account's
  bindings. The runtime role inserts and updates the lease and
  acknowledgement rows and reads requests; the command, as the owner,
  removes leases ended more than a day ago and requests older than two
  days. A holder of the runtime credential can write an acknowledgement
  with its own SQL and so report sessions ended that were not; the ledger
  and the account rows are unaffected.
- **Not done by retirement:** it does not revoke the account's grants in
  any room, here or on another deployment, and the account's contacts on
  other deployments are sent no final unavailable presence: the S2S router
  refuses everything a retired account sends. The
  [retirement runbook](../../../docs/operations/account-retirement.md)
  orders those steps. Other homes cannot be required to keep such a ledger;
  ADR-0020 D8 states what a reassigned JID may inherit there.

### Store capabilities of the fence (G11)

The first slice's grant matrix gains two relations, both `SELECT`,
`INSERT`, `UPDATE` for the runtime role: `host_grant_fence_replicas` and
`host_grant_fence_acks`. The runtime still deletes no host-grant row: an
expired lease is a row that no longer counts, and the owner phase removes
leases that ran out more than a day ago. The store's privilege check and the
attachment's relation list cover both. `retired_account_jids` is `SELECT`
only. A holder of the runtime credential can write a lease or an
acknowledgement with its own SQL, and so make a revoke's acknowledgement
wait or report a generation it did not install; as in the first slice, the
runtime is the authority's writer, and the commit of a revoke is unaffected.

### Rollout part of the revocation fence (G24)

- Migration 2005 creates the two fence relations on PostgreSQL and nothing
  on SQLite; migration 2006 creates the ledger of retired JIDs on both.
  Version 2004 is not in this catalog. The owner phase applies them and
  grants from the matrix before any server of the release starts, as for the
  earlier migrations.
- With no scope enrolled a replica holds no lease, runs no tick and no
  sweep, and takes no queue lock. Unmanaged rooms never reach the fence.
- With a scope enrolled, each serving replica runs four ticks a second on
  its main connection pool, whether or not anything changes: one
  transaction that renews its lease and reads every scope's generation
  (two statements, plus the one that bounds them), and one more that
  records the generations that moved.
- For every deployment: native registration and identity-provider
  provisioning insert under the ledger's condition, and session creation
  reads the ledger once; fan-out from a node of the Spaces service lists
  the node's deliverable subscriptions once more per publication before it
  hands over; a self-ping and the media grants of a call participant ask
  the gate, which answers from the enrolled set in memory for an unmanaged
  room. A client's room administration and owner configuration requests
  wait at most 15 s for the room's configuration lock and are answered
  `resource-constraint` after that.
- For enrolled scopes: every decision needs the replica's lease; an
  affiliation change replies after the acknowledgement; occupants are
  removed as above; an acceptance behind a waiting mutation waits for it.
- Clocks, before the first enrollment: serving replicas run on Linux, where
  the lease is measured on `CLOCK_BOOTTIME`; PostgreSQL hosts, the standby a
  failover promotes included, are kept in sync by NTP with an offset well
  under 1 s; and the `waddle.host_grant.fence.clock_skew` gauge of every
  replica stays below 500 ms
  ([metrics](../../../docs/operations/server-metrics.md)).
- Mixed versions: a binary built before this slice holds no lease, so a
  revoke does not wait for it, and it admits from decisions it does not
  check. Enrollment therefore requires every serving replica of the
  deployment on this slice's binary, as it already requires the writer
  fence's: W13's rollout runbook enrolls no scope until every replica runs
  it, and no replica of an older binary joins while a scope is enrolled.

### What stays open

- A PubSub admission identity for a scope's Space, and with it an
  acknowledged fence for PubSub: unenrolled Spaces have the hand-over read
  only. A notification for a subscriber at another server is decided at
  the same hand-over, on the subscription's recorded evidence
  ([Spaces](s2s-routing-2047.md#spaces-w7a)); a revoke's acknowledgement
  does not cover it, as it does not cover a local subscriber's.
- Ending a revoked account's open streams. A retired account's are ended by
  the retirement command ([D8](#non-reuse-of-retired-account-jids-d8)).
- A replica whose lease lapsed keeps its seats until it registers again.
  Removing them on the lapse would need the grants it cannot read.
- The relayed record of a remote membership (`RemoteMucMemberships`) is not
  cleared when the room's owner removes an occupant; self-ping no longer
  reads it for a managed room, and other readers of it are not audited here.
- A plain presence update of a seated, revoked occupant is still broadcast
  until the sweep removes the seat.
- Remote principals at these surfaces are decided under
  [Remote principals at the effect boundary](#remote-principals-at-the-effect-boundary-g17);
  the remaining items of the writer fence's list stay open.
- Surfaces that decide before an await and act after it without deciding
  again, beyond those this section re-decides: XEP-0425 message moderation,
  channel-wide mention delivery, Muji call start and the release of a media
  token, the owner configuration form (`muc#owner` get), the extension bot
  list, and the room and Space listings. A revoke acknowledged during that
  await can be followed by that one effect, as for in-flight work. Each
  needs its decision taken again, or its ticket asked, after the await, as
  the archive pages, the role list and the room's own messages now do.
- An owner's configuration change asks its decision just before it asks
  the room actor, not in the actor's turn. An `UpdateConfig` that asks the
  ticket in that turn, as `ApplyAdminItemsIfAdmitted` does, would close the
  step between them.
- The admin commands (`admin/channels.rs`) and the configuration recovery
  path still wait for a room's configuration lock without bound.

### Evidence of the revocation fence

The suites, under `crates/waddle-server/tests/`:

- `host_grant_revocation_fence.rs` (PostgreSQL): two replicas' gates on one
  database. A revoke is acknowledged once both installed it, and a decision
  read before it then allows nothing; a replica that stops renewing is
  waited for until its lease and the grace ran out, decides nothing by then
  and returns under a new registration; a renewal whose answer is held
  until after the lease it renews ran out here does not make a decision
  read under that lease current again; a replica that stopped in order, or
  holds no attachment, is not waited for; an acceptance that arrives behind
  a waiting revoke does not overtake it (the case fails without the queue
  lock); the operator subcommands list, revoke and replay through the fence,
  a repeated `revoke-account` reports every room acknowledged at its scope's
  current generation, and both report a revoke a replica has not installed
  as not acknowledged.
- `xep0045_host_grant_revocation.rs` (PostgreSQL), on one or two replicas
  served over loopback sockets: removal with status 321 and 301 through the
  room's administration and through a revoke recorded outside the room, and
  of everyone when the scope is retired; a revoke made on one replica is
  acknowledged by both before its reply, the other removes the seat it
  hosts, and a join decided before the revoke seats nobody when it reaches
  the actor afterwards; a demotion lowers the mirrored affiliation and role
  and voids a role change decided before it; the sweep changes nothing while
  the grants cannot be read; four requests held after their decision while
  a revoke is recorded and acknowledged (`DecisionCheckpoint`): an owner's
  configuration change that waited for the configuration lock an
  administration request held across the acknowledgement is refused, a
  role list read before a demotion is not sent, an extension's archive page
  read before a revoke is not returned, and the room's own message archived
  before a revoke is not delivered to the revoked member; and
  `two_processes_…`, where the second replica
  is another operating-system process that is stopped with `SIGSTOP` in the
  middle of a revoke: the reply waits for its lease, and when it continues
  it never tells the revoked member that it is joined and removes the seat
  once it has registered again.
- `xep0410_host_grant_self_ping.rs` (PostgreSQL): `not-acceptable` for a
  revoked occupant on another replica, `remote-server-timeout` while the
  replica holds no lease.
- `xep0060_space_fanout_fence.rs` (PostgreSQL): the #2056 schedule, for a
  ban and for an unsubscription.
- `xep0045_host_grant_fence.rs`, its join and disco case extended: the
  reserved-nick node of a managed room is answered `item-not-found` to an
  account the grants do not admit to view the room.
- `account_jid_retirement.rs` (SQLite and PostgreSQL): every provisioning
  path after a retirement, the exclusion of retirement and registration in
  both orders, a retirement that fails with nothing changed when it cannot
  lock the account tables within its bound, and the operator subcommand on
  both engines.
- `host_grant_projection.rs`, one case added
  (`live_spicedb_a_revoke_is_acknowledged_while_the_projection_still_holds_the_grant`,
  PostgreSQL and SpiceDB): a revoke is acknowledged while SpiceDB still
  answers for the projection before it, the replica admits nothing under
  it, and the projection follows on the next pass.

The acceptance-matrix rows marked W5d map to them as follows. "#2056
captured Space fanout after a revoke ACK" is `xep0060_space_fanout_fence`,
with the limit stated above for unenrolled Spaces. "Acknowledged revoke
across two replicas with queued work" is `host_grant_revocation_fence`
(decisions held across the revoke, the queue at room-manifest acceptance)
and `xep0045_host_grant_revocation` (joins, protected reads, room
administration and the audience of a later message, on two replicas and two
processes); PubSub and stream-management fan-out from an enrolled Space
deliver nothing, which the writer fence's suite covers. "Retired account JID
on a Waddle home" is `account_jid_retirement`.

Not covered by a test: the media paths (the fenced state configures no media
server), a seat that completes after an installation and the sweep it asks
for, the detached stream-management leg of the Space fan-out, the sweep's
delivery through the room-effect outbox and under clustering, the pruning of
old leases by the owner phase, the restricted runtime role's privileges on
the new relations beyond the store's own check, the two counters, the order
of a room's administration and a sweep on different processes, the operator
subcommand's exit code when a retirement fails, `revoke-account`'s line and
exit code for a room whose revoke or acknowledgement fails to be read, the
configuration lock's 15 s bound, the re-seating's sweep after a failed ask,
the boot-time clock across an actual suspend of the host, the clock-skew
gauge and its warning, a renewal that is slow in PostgreSQL rather than in
its answer, and a second replica that is a `waddle-server` process rather
than a process of the test binary: a server process attaches only under the
restricted runtime, which is verification step 4.

#### Running the revocation slice's focused set

With the environment of the
[first slice's focused set](#running-the-first-slices-focused-set):

```sh
cargo nextest run -p waddle-server --features test-support --locked \
    --run-ignored all \
    -E 'binary(host_grant_revocation_fence) | binary(xep0045_host_grant_revocation) | binary(xep0410_host_grant_self_ping) | binary(xep0060_space_fanout_fence) | binary(account_jid_retirement)'
```

Each case creates and drops its own database. None of these binaries needs
SpiceDB; the case added to `host_grant_projection` does, and runs with the
first slice's set. Three cases run the server binary's operator subcommands,
and one starts a second process of the test binary.

The set was executed on 2026-11-16 at commit `e60eea4a1`, against
PostgreSQL 17.11 and SpiceDB 1.56.1, with nextest's default parallelism and
together with the first slice's, the writer fence's and the restore slice's
sets, whose code it changes: two consecutive runs passed 239 of 239 cases in
19 binaries, the 27 cases of the five binaries above among them. That SpiceDB
ran on a PostgreSQL datastore, so the `live_spicedb_` cases of those runs,
the one added here included, are evidence against a PostgreSQL-backed
SpiceDB, unlike the earlier runs recorded above. The second replica of the
two-process case is a process of the test binary, not a `waddle-server`
process.

After the attached-only gate was merged in and the late renewal, the
repeated `revoke-account`, the surfaces decided again after an await and
the batched Space fan-out read were fixed, the same 19 binaries ran once
more on 2026-11-16 at commit `658db7c4e`, with the same versions, the same
PostgreSQL-backed SpiceDB and nextest's default parallelism: 245 of 245
cases passed, 32 of them in the five binaries above. No run recorded here
shows that a new case fails without its fix.

Merged with the effect boundary for remote principals, the five binaries
above, the effect boundary's suite and every other host-grant, schema and
codec binary (21 binaries) ran on 2026-11-16 at commit `c415ea213`, with
the same versions and a PostgreSQL-backed SpiceDB of its own: 277 of 278
cases passed. The one failure was the new case for a seated remote
occupant, which waited for an arrival presence a member seated through its
room actor is never sent; the case was corrected at `f05fed7ac` and its
binary then passed 12 of 12. That case fails with the sweep's rule for
remote occupants reverted, and the case for a dropped increase fails with
reconciliation's drop disabled. The 21 binaries ran again at `ff1dbf666`,
with a fresh PostgreSQL-backed SpiceDB: 278 of 278 cases passed. The runs
are recorded in `docs/evidence/2047/unrun-tests.md`.

## Remote principals at the effect boundary (G17)

These are the decisions of the effect-boundary slice (W5e in the plan) for
gate G17 and its part of G14. Like those of the slices before it, they are
decisions: the acceptance-matrix rows marked W5e and the suite named under
[Evidence of the effect boundary](#evidence-of-the-effect-boundary) are
their evidence; the suite's run is recorded with them.

### The principal policy

- One value per replica decides whose host grants are honoured:
  `PrincipalPolicy` (`permissions/host_grant/principal.rs`), the served
  account domains (the canonical `WADDLE_XMPP_DOMAIN`) and the S2S snapshot
  the replica runs (`FederationSnapshot`: its generation, its admission
  policy with the trusted-home list and `require_private_ca`, and the
  host-policy `pkix` switch). It holds no snapshot while S2S is off.
- The replica builds it from `AppState::grant_principals`, and every
  host-grant path takes it from there: the attachment, and the verifier
  that rebuilds the attachment each time the fence runs again, so
  room-manifest acceptance and every `HostGrantGate` surface; the issuance
  policy of room administration and of projection recovery
  (`IssuancePolicy::for_principals`); the topology bootstrap; and the
  Spaces owner seeds of Space and node creation. The served account
  domains and the trusted homes therefore each come from one configuration
  value.
- Startup refuses an S2S snapshot whose trusted-home list names a domain of
  one of the deployment's services (upload, push, the community and
  extension services, and the call mixer of the account domain). The S2S loader refuses the local domains it knows;
  issuance cannot grant to a service domain.
- The snapshot is immutable for the life of the process: a replica applies
  a changed one when it restarts with it, as the connection-manager design
  has no reload. Every decision reads the snapshot the replica runs at that
  moment, and a remote principal's witness names that snapshot's
  generation. A change is complete only when every replica runs it
  (ADR-0020 D1); until then a replica that still runs the previous snapshot
  decides by it. This is what makes the snapshot safe as a deployment-wide
  policy input without a guard of its own: within a process no decision
  can read any other snapshot.

### The witness

`PrincipalPolicy::witness` decides what the principal of a decision is,
from its bare JID and the evidence of how its request arrived:

- *Evidence.* `PrincipalEvidence::Stream` carries the authentication
  strength and the trust-profile kind of the inbound S2S stream that
  carried the request. Outside test support it is built only from a
  `RemotePrincipal`. A replica builds one from a stanza on an inbound
  stream it verified, or, for a room it holds, from the facts of a stream
  another replica of the deployment verified and carried to it over the
  cluster relay with the stanza (`RemotePrincipal::carried`; see
  [inbound MUC](s2s-routing-2047.md#inbound-muc-w6a)). The carried facts
  are the address, the strength, the trust-profile kind and the snapshot
  generation, and nothing decided from them: the receiving replica
  evaluates admission and trusted-home status itself. It takes the facts
  on the relay's word, as it takes every relay message; only replicas of
  the deployment reach the relay, and no peer server's input builds a
  principal on any path but a verified stream.
  `PrincipalEvidence::NoStream` is a local session's own request, or a
  principal the decision names without its request (a recipient, a seated
  occupant, a listed subject).
- A subject of a served account domain without stream evidence is
  `Served`, and its grants are honoured as stored. The local path is
  unchanged.
- Every other subject is remote. Its witness (`RemoteWitness`) binds its
  home domain; the strength and trust-profile kind of its stream (none
  without one); the snapshot's generation; the D1 admission decision for
  that domain (`AdmissionPolicy::decide`, a denylisted parent domain
  included); whether the home is on the trusted-home list; whether it is a
  trusted home for that stream (`AdmissionPolicy::is_trusted_home` with the
  stream's profile, so a home whose `require_private_ca` entry the profile
  does not meet is not); and whether host policy requires `pkix`. Beside
  D1's refusals, a remote principal is refused when S2S is off, when its
  domain is not in canonical form, and when a stream claims an account of
  a served domain, which is never decided as local.
- Admission and trusted-home status are never carried with work: a
  `RemotePrincipal` carries only the facts of its stream, and the rest is
  evaluated again at each decision. Work queued before its domain stopped
  being admitted, or before its home left the list, is refused when it is
  decided.

### The rule

What a remote principal's effective affiliation is honoured as
(`PrincipalWitness::honour`, used by `policy::grant_allows`):

| Witness | Honoured as |
| --- | --- |
| Effective `outcast` | `outcast`, always; a recorded `outcast` denies every action whatever the effective column holds |
| Domain not admitted (S2S off, a D1 refusal, not canonical, a served domain on a stream) | none |
| Host policy requires `pkix` and no `pkix` stream carried the request | none |
| `owner` or `admin`, unless a trusted home's `pkix` stream with a verified certificate carried the request | none |
| Anything else | as stored |

None allows nothing, so a grant that is not honoured contributes nothing,
the member-level `read` and `send_message` the table would derive from it
included (D8). A member-level grant is honoured on any stream while the
domain is admitted. A message author's own correction and retraction need
what `send_message` needs, so the moderation conditions do not restrict
them.

### Where it is decided

- Room-manifest acceptance (`admit_room_acceptance`): the sender with the
  evidence of its request, every named recipient without stream evidence.
  The acceptance's witness (`AdmissionWitness::sender`) records what the
  sender was decided as. A local session, an extension and a room's own
  result submit without a stream, so the acceptance passes `NoStream` for
  them. A remote occupant's message is submitted with a principal of its
  own kind (`IngressPrincipal::Remote`), which carries the stream evidence
  of its seat, and the acceptance decides the sender on it
  (`RoomAcceptance::sender_evidence`). A named recipient that is a remote
  occupant is decided on the evidence each of its seats on this replica
  was admitted under (`RoomAcceptance::seats`), and is in the audience
  only if all of them admit it to read; a recipient without a seat here is
  decided without stream evidence.
- The gate's surfaces: each names the evidence it decides with, through
  `ManagedRoomGrants::allows_as` and `standing_as`, `HostGrantGate::allows_as`
  and `HostGrantGate::still_allows`. A surface that serves a local
  session's request or a subject it names passes `NoStream`. The surfaces a
  remote principal reaches (the join, the archive read before and after the
  page is read, `disco#info` of the room, the MUC service's and a Space
  node's `disco#items`, the self-ping, and the administration requests:
  the `muc#admin` lists and changes, the
  `muc#owner` configuration and XEP-0425 moderation; see
  [inbound MUC](s2s-routing-2047.md#inbound-muc-w6a)) pass the evidence of
  the stream that carried the request (`PrincipalEvidence::of`). The
  administration handlers take the evidence as a parameter: a session's
  dispatch and the cluster's forward of a session's list request pass
  `NoStream`, and the room host passes the stream's.
  `ManagedRoomGrants::admitted` lists subjects without stream evidence.
  Each of these asks the revocation fence's ticket of the decision too, so
  a decision made with a stream's evidence is fenced like any other. The
  stream-less forms `allows` and `standing` exist for suites only (test
  support), so a surface that starts serving remote requests cannot decide
  them as local ones by omission.
- A mutation's initiator: `HostGrantStore::set_affiliation_as` (and
  `HostGrantAuthority::set_affiliation_as`) takes the initiator's evidence
  and decides its standing under the issuance policy's principal policy,
  evaluated when the command is submitted. Room administration passes
  `NoStream` for a local session and the evidence of the request's stream
  for a remote principal, so a remote owner's or admin's change is recorded
  only while its home is a trusted home and a `pkix` stream carried the
  request, and is acknowledged by the revocation fence like any other; the
  stream-less `set_affiliation` exists for suites only. A `PrincipalPolicy` is built from served account
  domains alone (S2S off) by name, `PrincipalPolicy::local`; the
  conversion from `ServedAccountDomains` exists for suites only, so a
  replica cannot lose its snapshot through a conversion. The evidence is not part of the operation's
  fingerprint: an exact retry returns the recorded outcome and authorizes
  nothing again, and a refused initiator records nothing.
- The moderator role (an XEP-0045 role change): granted only to an occupant
  whose witness honours room moderation. The occupant is not the
  requester: an account of a served domain is decided without stream
  evidence, and a remote occupant on the evidence its seats were admitted
  under, so the role goes to a trusted home's occupant only while every
  one of its seats is on a `pkix` stream. The same holds where a seat is
  taken: a join that would seat a remote principal as owner, admin or
  moderator without its witness honouring that is refused, and one the
  room seated all the same is undone without anyone being told that an
  occupancy ended, since nobody was told that it began.
- An owner or admin affiliation (an XEP-0045 affiliation change) derives
  the moderator role for every occupant of the account, in the room actor,
  for the account and not for each seat. So the affiliation is not
  recorded, in a managed room or any other, while one of the account's
  seats on the replica that holds the room would not honour it
  (`seats_honour_privilege`): the requester is answered `not-allowed`, and
  grants it once that occupant has left or has entered over a stream that
  honours it. An account that holds no seat there is decided by the
  issuance policy alone, and each seat it takes later where the seat is
  taken.
- A room's own message (a system message) in a managed room: each
  occupant is named without a request, so a remote occupant is decided on
  the evidence each of its seats on this replica was admitted under and
  receives its copy only if all of them admit it to read, when the
  audience is chosen and again as each copy is routed.
- Re-seating after a claim move: a recorded remote occupant is seated
  again only when this replica takes over its seat, which it can once the
  seat's owner is no longer alive, and is then decided on the evidence
  that seat records. One whose seat cannot be taken over (none exists, or
  a live replica holds it) is not seated again, in a managed room or any
  other: a room's stanzas leave for a remote JID only through a seat.

### Decided without stream evidence

These rules are decided by the served account domains alone, whatever the
snapshot and the stream, because what they protect outlives any one
request:

- The owner a room keeps (`LastOwner`) is an owner of a served account
  domain. A remote owner's standing is honoured only on some streams and
  only while the snapshot lists its home, so a room only it owned could be
  left with nobody who can administer it.
- Enrollment: every room starts with an owner of a served account domain.
- An operator's revoke: its recorded basis and the owners it keeps.
- Reconstruction compares a lowering under the floor
  (`policy::honoured_floor`): what a grant is honoured as on every decision
  that honours it at all. Lowering a remote admin to member, which some
  decisions would honour above the admin grant, is recorded at `none`.

### An increase its initiator's home no longer backs

An increase is recorded at its commit and effective only once its
projection is verified and activated. The initiator's standing is decided
when the command is submitted, under the snapshot the replica runs then. A
remote initiator's standing to make an increase (owner or admin) rests on
its home being admitted and listed as a trusted home, and the snapshot can
change before the increase is activated: the home delisted, no longer
trusted, or no longer admitted.

Reconciliation therefore checks again, before it reads the scope's plan
(`HostGrantStore::drop_unhonoured_increases`, called by
`HostGrantAuthority::reconcile` under the replica's issuance policy). Under
the scope's guard it finds every recorded increase that is not yet
effective whose recording operation names an initiator of a domain the
deployment does not serve, and evaluates that initiator's witness under the
snapshot the reconciling replica runs. The stream that carried the request
is gone, so only what does not depend on it is evaluated: the home is
admitted, and it is on the trusted-home list. An increase whose initiator
fails either is dropped: one operation at the next generation, recorded
without an initiator, lowers each such row's recorded affiliation to its
effective one and journals the change from the increase to it. The increase
is never projected and never activated; the projection that follows holds
the scope without it, and the scope converges as after any other operation.
An increase by an account of a served domain, or recorded without an
initiator (an owner act), is kept.

Dropping fails closed. The row returns to its effective affiliation, which
is what every decision already honoured, and never to some value between
the two: the store keeps one recorded and one effective affiliation for a
row and the operation that last recorded it, not the history of what was
recorded in between. Two consequences follow. An increase that another
initiator recorded on the same row earlier, and that had not been
activated when the dropped one replaced it, is lost with it and has to be
issued again; the journal keeps both operations. And an initiator's
increase that a later operation of an honoured initiator replaced is not
looked at: the row names the later operation, whose initiator is the one
checked. Nothing is ever raised by a drop, and a decrease is never
dropped, since a decrease is effective at its commit. A scope that admits no mutation (retired,
exhausted, fenced, bound to another incarnation, or closed for a restore) is
not checked. A retired, exhausted or fenced scope activates nothing; the
reconstruction of a scope closed for a restore activates what its restored
rows record once its projection is rebuilt, so an increase recorded there
before the restore is not checked again on that path, and the operator's
review before the reopen is what stands in for it. Each drop is counted
(`waddle.host_grant.increases_dropped`) and logged with its kind, never with
a JID.

The journal records a drop as a `set_affiliation` operation without an
initiator whose change lowers a recorded increase to the effective
affiliation; it has no operation kind of its own, which would need a
migration of the kind constraint. The snapshot is fixed for the life of a
process, so a pass that finds nothing to drop is not overtaken by a change
of its own replica's policy. A replica that still runs the earlier snapshot
decides by it until it restarts, and can activate such an increase in that
window; the change of snapshot is complete only when every replica runs it
(ADR-0020 D1).

### Host policy requiring `pkix`

`require_pkix` in the `[host_policy]` table of the S2S snapshot (off by
default) makes every grant of a remote principal contribute nothing unless
a `pkix` stream carried its request, member-level grants included. It is
deployment-wide, and stays so for this slice: ADR-0020 D2 allows host policy
to require `pkix` for a community or a room, and a per-scope requirement is
a later change. It would be a
policy input of the scope, recorded and fenced by the scope's generation
like its grants, which needs a migration and an operation of its own.

### What stays open

- A principal named without its request and without a seat on the
  deciding replica has no stream evidence. A trusted home's admin that
  holds no seat there is therefore not counted as a reader: not in an
  acceptance's audience and not listed as a reader; with `require_pkix`,
  no such remote member is. A remote occupant seated over S2S is decided
  on its seat's evidence in the acceptance's audience, by the revocation
  sweep and when it is re-seated
  ([Seated occupants, roles and media](#seated-occupants-roles-and-media-g14)).
  The evidence is held by the replica that seats the occupant; a decision
  taken on another replica names it without one.
- Rooms and Spaces no scope manages keep their legacy checks. Where a
  remote principal reaches one (the MUC service and the Spaces service),
  each effect is decided with `PrincipalPolicy::witness`: D1 at every
  request, and D8 on whatever standing the legacy checks give the
  principal ([inbound MUC](s2s-routing-2047.md#inbound-muc-w6a),
  [Spaces](s2s-routing-2047.md#spaces-w7a)). D8 issuance is applied by the
  `muc#admin` writer of such a room, by the admin command that sets a room
  affiliation (`channels:set-affiliation`), by the writers of a Space's
  PubSub affiliations (`spaces:set-role` and the `<affiliations/>` change
  on the Spaces service) and by the permission actor's write messages,
  which refuse the channel and Space `owner` and `admin` relations and the
  relations no writer names (`channel.manager`, `channel.moderator`,
  `space.moderator` and `role.manager`) for an account whose home is
  neither served nor trusted. A standing
  stored for a remote account before that, or under an earlier
  trusted-home list, is not honoured at the effect unless its home is
  trusted and its stream is `pkix`.
- The channel and Space `owner` and `admin` relations are checked by their
  writers and again by the permission actor, so a writer added later that
  names one of them for an account it does not check is refused at the
  actor. A replica whose account domain is not canonical attaches a
  policy that admits no account, and then writes those relations for
  nobody.
- A remote principal's administration of a managed room has no fence check
  beyond the ones a session's has: a moderation request (XEP-0425) and the
  role list read are decided once, on the grants as read, and an
  affiliation list is read from one generation of the grants. An
  acknowledged revoke between such a read and its answer is not seen by
  that answer, for a remote principal as for a session.
- Account bootstrap provisions deployment ownership for served accounts
  only, which is stricter than D8 requires.
- A per-scope `pkix` requirement (above).

### Evidence of the effect boundary

`tests/host_grant_remote_principals.rs` decides each case under snapshots
loaded through the production S2S loader from files, with a stream's
evidence given in place of a live stream. The acceptance-matrix rows marked
W5e map onto its cases; over live streams, which reach a managed room
through inbound MUC, they are the cases listed at the end of this section
and verification steps 6 and 7. It needs
PostgreSQL only, except
`live_spicedb_an_increase_whose_initiator_is_no_longer_trusted_is_dropped`,
which reconciles through a disposable SpiceDB like the other `live_spicedb_`
suites. `a_remote_ban_is_honoured_by_the_gate_and_acceptance_on_every_stream`
decides a ban under a snapshot that admits and trusts the banned homes, on
the strongest and every weaker stream, a `pkix` strength without a trust
profile included. The suite ran on 2026-11-16 at
`c415ea213`, with the revocation fence's focused set (above): 14 of 14
cases passed, the `live_spicedb_` case against a PostgreSQL-backed SpiceDB. `host_grant_authority.rs` and
`xep0045_host_grant_admission.rs` cover a replica with S2S off.

Written since that run and not executed:

- in `host_grant_remote_principals.rs`,
  `live_spicedb_a_remote_initiated_increase_activates_while_its_home_is_trusted`,
  the control for the dropped increase, and
  `live_spicedb_an_increase_whose_initiators_home_is_no_longer_admitted_is_dropped`;
- in `xep0045_host_grant_revocation.rs`, the sweep's rule for a seated
  remote occupant, which replaces
  `the_sweep_removes_a_remote_occupant_only_when_no_stream_would_admit_it`:
  `the_sweep_removes_a_remote_occupant_whose_domain_is_not_admitted` (S2S
  off), `the_sweep_decides_an_admitted_remote_occupant_on_its_effective_standing`
  (a pending increase admits nothing) and
  `the_sweep_removes_an_admitted_remote_occupant_when_its_scope_closes`;
- over live streams, in `xep0045_s2s_inbound_muc.rs` and
  `xep0313_s2s_inbound_mam.rs`: a remote member entering a managed room on
  its grant, a stranger refused, messages through the room's acceptance
  both ways, a revoked member removed by the sweep on its seat's evidence
  and told over S2S, the archive read decided on the grant, a managed room
  whose `federated` setting is off answered as a room that does not exist
  to its own remote member, a trusted home's admin recording a membership
  through the store on its stream's evidence
  (`a_remote_admin_of_a_managed_room_changes_its_affiliations_through_its_grants`),
  and a remote member refused every administration request
  (`a_remote_member_of_a_managed_room_administers_nothing`).

### Rollout part of the effect boundary (G24)

- With S2S off, a remote account's member-level grant in a managed room no
  longer admits anything, because no remote domain is admitted. No remote
  principal reaches a managed room while S2S is off; a remote recipient
  planned in a groupchat is frozen no child, where it was frozen one that
  could not be delivered.
- `[host_policy]` is a new table of the S2S snapshot. The loader refuses
  unknown keys, so a binary that predates the table refuses a snapshot that
  holds it: replicas run the new image before the snapshot uses it, and a
  rollback to such a binary removes the table from the snapshot first, or
  the rolled-back replica does not start.
- A replica whose snapshot no longer admits a home, or no longer lists it
  as a trusted home, drops that home's initiators' recorded increases that
  are not yet effective at its next reconciliation (above).
- A snapshot whose trusted-home list names a service domain stops startup.
- With S2S on, a trusted home's privileged grants and room moderation are
  honoured on its `pkix` streams and member-level grants of admitted
  domains on any stream. The MUC service serves remote principals on a
  replica that runs S2S ([inbound MUC](s2s-routing-2047.md#inbound-muc-w6a)),
  so an enrolled scope's room admits the remote accounts its grants name
  once their domain is admitted and the room's `federated` setting is on.
  The setting is off for every room that exists, so enrolling a scope or
  enabling S2S opens no managed room to another server by itself. A trusted
  home's owner or admin then administers the room over S2S: its
  affiliation changes are recorded through the store and acknowledged by
  the revocation fence as a session's are.
- The revocation sweep removes a seated occupant of a domain that is not
  admitted, which with S2S off is every domain the deployment does not
  serve. Before, such an occupant kept its seat while its recorded
  affiliation was a membership. It also removes an occupant whose
  membership is a recorded increase that is not yet effective, where it
  read the recorded affiliation before.

## Blocking review ledger

Slice names refer to [the federation plan](../../../docs/planning/federation-2047.md),
whose gate table also names the verification step that discharges each entry's
evidence.

| ID | Unresolved decision/evidence | Work blocked | Under ADR-0020 |
| --- | --- | --- | --- |
| B1 | Exhaustive scope writer/reader exclusion, old writer retirement and concrete store capabilities | Enrollment and production authority integration. | D5 condition, gate G11; open. Decided by W5b (store capabilities, credential separation: [recorded](#b1-store-capabilities-and-credential-separation-g11)) and W5c (writer inventory and exclusion: [recorded](#b1-writer-and-reader-exclusion-g11)); old binaries and credentials are retired under the rollout runbook (G24) and the activation runbook (G22). |
| B2 | Complete action/negative/admin/global dependency policy, actor projection, XMPP request/result identity | Affected protected operations and handlers. | Gate G14; open. Decided per protected surface by W5b, W5c and W5e; W5d decides the actor's seats, roles and media after a revoke ([recorded](#seated-occupants-roles-and-media-g14)). Includes outcast precedence in every channel and message action, including an author's own edit and delete (W5o), and the D8 rules: privileged grants and room moderation for remote principals only from trusted homes and only on `pkix` streams, and a grant that is not honored contributing nothing. W5b's part is [recorded](#b2-policy-for-the-action-this-slice-protects-g14-part), W5c's [as well](#b2-policy-for-the-surfaces-this-slice-protects-g14-part), and W5e's for remote principals [too](#remote-principals-at-the-effect-boundary-g17). |
| B3 | CAS/marker/schema protocol, bounded reconstruction, missing-marker and large-graph rules | Projection integration beyond independently proven bounded operations. | Gate G15; open. Off the first slice's safety path, which admits in SQL. W5b decides the marker protocol for the projection writes it makes ([recorded](#b3-marker-protocol-for-this-slices-projection-writes-g15-part)), W5c extends it to the writers it fences ([recorded](#b3-projection-of-the-writers-this-slice-fences-g15-part)), W5o decides the fenced schema transition and W5r bounded reconstruction ([recorded](#verified-reconstruction-g15-part)). Any admission that reads the projection needs B3 decided first; no slice in the plan adds one. |
| B4 | Actual claim/authority/child lock order, atomic admission and cancellation/lost-commit ownership | First manifest integration. | D5 condition, gate G12; open. Decided by W5b ([recorded](#b4-lock-order-atomic-admission-cancellation-and-lost-commits-g12)) and extended by W5c for each surface it fences ([recorded](#b4-lock-order-of-the-surfaces-g12)) and by W5d with the queue in front of the guard ([recorded](#the-queue-in-front-of-the-guard-g12)). |
| B5 | PubSub/DirectFrame admission identity; read/join/SM sink contracts | Those sinks and community activation. | Gate G16; open. Decided by W5c ([recorded](#b5-sink-contracts-g16)); W5d defines what a revoke's acknowledgement means at each sink ([recorded](#sink-contracts-under-the-fence-g16)), and its focused set with the #2056 control is the evidence ([run](#running-the-revocation-slices-focused-set)). A PubSub admission identity for a scope's Space is still not built: the Spaces slice (W7a) serves an enrolled scope's Space to nobody over S2S. |
| B6 | XML-decoded pre-parse enforcement, canonical JID/account-binding agreement, isolated codec and authoritative reconciliation; ASCII test profile and full PRECIS/IDNA migration unresolved | External identities and enrolled authority keys. | D5 condition, gate G10; open. W1 covers only the codec part; W5i decides the rest, including the canonical domain form, and W5a applies it at S2S ingress. |
| B7 | Restore incarnation surviving rollback and excluding old deployment/work | Recovery capability and pilot activation. | D5 condition, gate G13; open. Decided by W5r ([recorded](#restore-incarnation-decisions), with the limits it leaves open); its evidence runs in verification step 4. |

The coarse fence's availability cost is a non-blocking tradeoff once bounded
resources and independent progress are proved. Fine-grained fences can follow
only after complete policy/mutation dependencies are demonstrated. Merging this
document, accepting D5 or passing an inventory scan discharges none of these
blockers.
