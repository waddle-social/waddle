# P2.1 known coverage gap gate

This is a test-support-only prerequisite for #2116 and #2046, not the complete
Gate 1 profile. `inspect_coverage_gap_gate_v1` always refuses: it either names a
configuration shape outside the deliberately narrow diagnostic target or
returns the fixed V1 list of known family gaps and conflicts. It has no success
variant, does not create a pool or transaction, and is not wired into startup.
It does not attest that caller-supplied configuration or owner OIDs describe a
live connection, authorize serving, or claim the list is exhaustive.

The described target is PostgreSQL 17, a build with `clustering` compiled but
runtime clustering disabled, homeserver mode, XMPP and native auth enabled,
SpiceDB configured, no external auth provider, extensions disabled with no
configured modules, and no APNs or FCM provider. The global URL must be
nonempty. All seven MAM,
inbox, PubSub, spaces metadata, channel-space links, SM and pending-delivery
locations must be explicitly represented as inherited from that same global
URL; an explicitly configured identical URL is refused. These checks cover
only the named switches. Unchecked settings and a syntactically matching URL
are not proof that the server or backend has this configuration.

The diagnostic also refuses a zero or equal schema-owner and database-owner
OID. This narrows the proposed joint target because Push, notification activity,
notification outbox and call-teardown attachment checks require distinct
owners. The existing `PinnedRuntimeAuthority` deliberately permits equality for
its narrower physical/ledger/epoch proof and remains unchanged. The diagnostic
compares the supplied integers only; the independent operator handoff and any
live verification remain separate work.

The [enabled caller-to-object map](./2116-enabled-caller-object-map.md) tracks
the candidate's source-backed callers and names the boundaries of each
partial family check. It is an inventory aid, not a catalog closure proof.

The gap list records the source paths and representative names for: the joint
role graph and global migrations; identity, sessions, roster, blocking,
profile, uploads and permissions; inbox, SM, pending delivery, ingress, epoch,
receipt journal and PubSub projections; MAM, spaces and links; MUC and room
effect outboxes; Push, VAPID, notification outbox and activity; call teardown;
and extension data. Representative names are not a complete per-family
object list. The `DurableStore` registry alone misses global-schema consumers,
so it cannot establish this list's completeness. No complete privilege matrix,
function/dependency inventory, source-to-object closure, or added/removed
object detector is claimed.

The combined fixture first checks representative present/absent heaps and
owner OIDs, then owner-prepares selected previously absent space, Push,
notification and call tables in a separate phase. Its 21 named table ACL
witnesses are not a whole-family or joint role profile. The test-support-only
strict paired space preparation writes an identity-bound owner marker; both
space-store attachment diagnostics then pass on the attested main pool and
refuse marker or runtime-grant drift. These are sequential fixture checks,
not selected startup or one all-store transaction. When clustering is compiled,
the fixture
also prepares clustered MUC heaps even though runtime-disabled clustering does
not select that store. Runtime-disabled clustering has no claim pair, and the
compiled attested MAM selector currently rejects that case; the fixture checks
that specific refusal. Current startup also invokes owner-only constructors
for room effects, Push registration, Push Service and notification activity.
The fixture witnesses room effects, Push registration and activity refusing an
attested handle; the Push Service conflict is recorded from source. These are
recorded conflicts, not waived gates. Both existing startup refusals remain in
place.

Completing Gate 1 requires a separately reviewed enabled-caller-to-object map,
a versioned complete role and ACL matrix, a real combined owner-prepared fixture
for the selected runtime configuration, exact missing/extra object and ACL drift
witnesses, and an independent review. Same-transaction proof, old-backend and
provider fencing, conversion of all writers and serving activation are later
work. This prerequisite does not change workflows or assert hosted CI.
