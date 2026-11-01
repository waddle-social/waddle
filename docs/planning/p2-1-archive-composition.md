# P2.1 archive-after-pin composition

Status: lock-order characterization complete; archive composition remains
blocked on durable pin-success provenance. This slice is stacked on #2157 and
does not activate P2.1 or change server startup, pools, migrations, or XMPP.

## Plan and gates

1. Extract the existing archive-after-pin transaction body into a private
   helper without changing its transaction boundary, return values, or lock
   order.
2. Exercise canonical-before-room and room-claim-before-canonical/takeover
   ordering against real PostgreSQL with bounded waits and deterministic
   coordination. Distinguish measured results from conclusions derived from
   source inspection.
3. Only after the lock-order evidence and an independent design review admit it,
   add a closed, dark archive operation to the typed authority. Its private
   transaction must retain the exact room fence, archive ordinal, and receipt
   settlement together, with schema-qualified PostgreSQL MAM SQL and SQLite
   semantics that use the same shared backend. Do not expose a raw executor,
   transaction, or arbitrary callback.

Verify the relevant PostgreSQL inventory, focused real-PostgreSQL tests, Rust
formatting, and strict Clippy. Report unverified assumptions and stop at the
extraction and test if the operation cannot satisfy either backend safely.

## Lock-order investigation

On disposable PostgreSQL 17.11, the repository-level queued-takeover test
observed a claim takeover waiting for a replay's shared room claim, and a
nested child waiting for an archive's canonical row. In that same schedule the
archive acquired a matching shared room claim while the takeover remained
queued: the hypothesized third blocking edge was absent. The old exact claim
was rejected after takeover committed. A separate test observed an archive
waiting on an already-held takeover claim and returning the typed timeout after
the test's four-second lock bound. Production currently uses 100 ms; the longer
test bound lets the observer establish the blocking edge. These results do not
prove that all application paths or schedules are deadlock-free.

The current dark authority and ingress unit of work use different private
transaction types. Existing MAM and ingress repositories issue unqualified
SQL, and the authority's separate SQLite constructor does not establish that it
uses the runtime's shared backend. These remain design blockers for a closed
archive operation; no such operation is added by this slice.

## Pin-success provenance

The public decision executor treats an `ApplyPin` effect as already receipted
when its supplied `external_receipts` are absent from `receipts_pending`. It
then marks those keys proven and may persist them through
`record_receipt_pooled`, without asking the actor. Both that generic writer and
the dark authority's generic receipt operation accept any existing intent key.
The foreign key proves that the intent exists, not that the actor applied it.
Thus neither the completion boolean nor an existing generic pin receipt is an
authenticated pin-success proof for a new archive authority.

The recorded `SystemMessageArchive` and `Pin` intents do not contain a durable
edge connecting a specific archive to its authorizing pin. `RoomPinSink` adds
`AfterRoomPin` to the in-memory plan, and recovery reconstructs the plan and
its dependencies; the frozen room manifest retains intents, not this edge.
Same-message, same-room co-presence therefore cannot establish the pairing
when multiple effects exist. The stored system message's pin-event is only a
projection and omits fields from the full pin identity. Comparing only a
semantic hash is also insufficient: `RoomPinMutation::storage_identity` joins
fields with delimiters and maps a missing preview nickname to an empty one.
The room actor's projection contains only pin/unpin and the target stanza;
its persisted fingerprint uses the target id, not the canonical message key
or full pin change.

### Blocking design gate

The smallest next candidate is a private dependency-and-success owner, first
exercised with unregistered laboratory schema and no runtime wiring:

1. Persist an immutable archive-to-pin edge in the acceptance transaction. A
   candidate table keys the edge by canonical message and archive intent key,
   references both the archive and pin intent rows, and binds the room and a
   digest of the canonical, full pin payload. Mint the edge from the trusted
   planning path; public `IngressSubmission`, `IngressPlan`, dependencies, or
   effect values alone cannot establish it. Reject a missing, ambiguous, or
   unsupported dependency graph. The archive ordinal may be filled later and
   is not part of the stable edge identity.
2. The trusted pin executor reads that edge and asks the matching room actor to
   apply the exact recorded change under its exact current fence. The actor's
   transaction must atomically commit the full pin-state update and an
   immutable outcome keyed by the stable operation ID. That outcome binds the
   canonical message, full pin identity, room, and historical actor/fence
   provenance. After a lost reply or restart, the owner reads this actor-side
   journal by operation ID; it does not insert a proof after the reply. A
   duplicate ID with the same full payload returns the original outcome
   without reapplying; a conflicting payload fails closed. No generic receipt,
   public completion outcome, caller-supplied boolean, or callback may mint the
   outcome. The eventual database privilege boundary must exclude other
   writers to these proof rows; a Rust-private API cannot protect against a
   caller with arbitrary SQL access to a privileged database handle.
3. The eventual archive transaction must use the existing prepared connection,
   lock and re-read the edge and success, decode both recorded intents, compare
   the full fields and exact room, assert the current room fence, and perform
   MAM insertion, ordinal finalization, and receipt settlement atomically.
   PostgreSQL relations must be schema-qualified; SQLite must use the actual
   shared backend. Tombstone, expiry, and errors roll back; an unknown commit
   is indeterminate. No actor call occurs while this transaction is held.

A migration would create the edge and success tables for both dialects,
including foreign keys, retention/cascade behavior, and PostgreSQL epoch-guard
manifest/trigger coverage. It must not infer proof from existing receipts or
pair old intents by room. Legacy rows without proof need a separately reviewed
recovery policy before any cutover. If the actor succeeds and the process dies
before success is recorded, current storage cannot distinguish success from
failure; replaying the mutation could overwrite newer room state. That window
must remain indeterminate until a durable actor-side protocol closes it.

The gate tests must include a forged decision and generic pin receipt, mismatched
message/room/change or producer, semantic-hash collision, two distinct pin and
archive pairs, actor refusal, rollback at each write, unknown commit, restart
after recorded success, ordinal refinement, and missing legacy proof. Exercise
the exact PostgreSQL relation names with a hostile search path, plus the actual
shared SQLite backend. These are proposed checks, not results from this slice.

## Actor-side durable pin falsification — blocking

Source review confirms that a post-reply success row cannot close the crash
window with today's room store:

- `RoomActor::ApplyPin` builds `RoomProjection::Pin` from only the target stanza
  ID, commits that projection, and then changes `MucRoom`'s in-memory pin list.
  `RoomProjection` explicitly carries no authoritative state
  (`server/crates/waddle-xmpp/src/muc/room_actor/implementation.rs:2910-2939`,
  `server/crates/waddle-xmpp/src/muc/durable/projection.rs:1-7,46-51`).
- `DurableRoomState` contains lifecycle coordinates, config, subject, and
  affiliations, but no pins. The fenced room load reads those same fields, and
  `install_durable_room_state` restores config, subject, and affiliations only
  (`server/crates/waddle-xmpp/src/muc/durable/implementation.rs:308-326`,
  `server/crates/waddle-xmpp/src/muc/room_actor/implementation.rs:1180-1204`,
  `server/crates/waddle-server/src/muc_durable/implementation.rs:2116-2184`).
  A new owner therefore cannot reconstruct the pin list from the current
  snapshot.
- The lifecycle row stores one current `mutation_fingerprint`; the pin
  projection fingerprint is just kind plus target. A later room commit replaces
  that fingerprint (`server/crates/waddle-server/src/muc_durable/implementation.rs:64-105,2001-2009`).
  It is not a retained operation journal and cannot prove an older mutation
  after a later revision. The existing actor transaction already has the right
  exact claim fence and can atomically write its own rows, but it does not store
  the full `PinStateChange`, stable operation ID, or recoverable pin state.
- The current mutation has observable order semantics: pin upsert removes the
  same target, moves the new entry to the front, and evicts the oldest entry at
  the cap; unpin removes by target
  (`server/crates/waddle-xmpp/src/muc/room.rs:762-795`). Replaying only a
  target-only fingerprint cannot reapply that full state transition.
- The existing `RoomPinMutation::storage_identity` is not injective: absent
  `preview.author_nick` and an empty nickname encode identically, and delimiter
  characters are not escaped
  (`server/crates/waddle-xmpp/src/ingress/effect_intent.rs:635-661`). It cannot
  be the new operation identity or the equality check for a proof.

The minimum viable closure crosses the current in-memory pin boundary: an
immutable accepted edge must be minted only by trusted ingress planning and
written with acceptance; the actor's exact-fence transaction must validate that
edge, persist a stable operation ID and canonical full mutation, update a
recoverable bounded pin snapshot (or an equivalently complete replay log), and
retain an immutable outcome keyed by operation ID. On duplicate ID, the actor
must compare the full typed payload and return the original outcome without
reapplying it. The original owner/fence/revision belong in historical
provenance; a later owner must read that history under its current exact fence,
and its archive transaction must independently use that current fence. A row
written only after the actor reply remains unsafe.

Neither `IngressSubmission`, `IngressDecision`, `IngressPlan`, public effect
values, generic receipts, nor a caller-supplied completion flag can mint that
edge or actor outcome. The persisted edge must name the exact canonical message,
archive intent, pin intent, room, stable operation ID, and full typed pin
payload. The actor store must resolve and validate those accepted facts in the
same fenced transaction that commits pin state and the journal; Rust visibility
alone is not a privilege boundary. The archive operation can then read
historical operation provenance but must fence its own write using the current
owner.

This is broader than the current projection: it changes MUC pin-state
durability and restore, which #1663's accepted P4 text classifies with pins as
`AwaitingDurableOwner`. Whether that recovery work belongs under #2046 or must
be dependency-wired separately is unresolved. Any implementation also needs a
schema migration for the edge, full mutation/outcome journal, and room pin
snapshot or replay log; foreign keys and retention/cascade policy; exact
PostgreSQL schema-qualified access; epoch-guard manifest/trigger coverage for
every new protected relation; and least-privilege writer/read grants. It must
not infer edges from legacy receipt rows. SQLite coverage can verify trusted
ingress edge persistence through the shared authority backend, but the current
clustered `MucDurableStore` is PostgreSQL-only, so SQLite cannot stand in for
actor takeover/recovery evidence.

The required implementation gate remains deterministic forged-plan and
generic-receipt rejection; exact pairing of multiple archive/pin effects;
full-field identity collision checks; rollback and lost/unknown commit recovery;
process restart after actor commit but before ingress settlement; duplicate
operation ID with same and conflicting payload; owner transfer with historical
provenance and a current fence; and stale-owner rejection. Run ingress schema
and edge tests against PostgreSQL and SQLite, plus the actor journal, restart,
and takeover tests against real PostgreSQL with the required roles and hostile
search path. None of those tests were run in this characterization. No schema,
runtime, role, or actor changes are included; this slice stops here pending the
cross-issue ownership decision and a design that includes pin-state restore.
