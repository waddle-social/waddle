# Dark durable SM custody for one local direct frame

Status: unselected repository candidate for #2046, behind `test-support` and
`clustering`. No production selector, registered migration, WebSocket switch,
GC activation, or resume activation is included.

## Implemented boundary

`server/crates/waddle-server/src/sm_custody/` owns a typed PostgreSQL repository
and unregistered owner DDL. It accepts one prepared direct frame and one captured
generation per SM session. Unique session and gate constraints reject generation
replacement and a second frame. No existing live/detached writer is converted.

The UOW's exact attested pool capability must match the full physical configuration,
including database/schema OIDs and roles. Admission checks the current principal,
exact node/claim, captured endpoint binding, session generation, canonical envelope,
frozen prepared direct intent, resource, original receipt time and real never-started
send lease. Immutable full-payload admission, attempt start and retention invalidation
commit together. Only a subsequent known committed immutable slot returns the
capability a future adapter could use for memory acceptance. No repository method
enqueues or writes to a socket. Captured endpoint equality is not live publication
or pump-quiescence proof.

The slot binds exact gate, nonwrapping ordinal and payload. Exposure commits before
the proposed attempt; repeat exposure is rejected. ACK must match the exact exposed
ordinal and wrapping wire `h`. Arbitrary cumulative ACKs/replay are outside this
one-frame candidate. Transfer atomically copies full payload, resource and original
receipt time into an independent candidate sink before terminal settlement; that
sink is not the selected pending-delivery store. Authoritative tombstone/retraction
integration is deferred: there is no tombstone API and SQL rejects disposition 3.

## Unknown commit and recovery

An irreversible durable gate seal waits for earlier admissions/binds and rejects
delayed original attempts, including those not yet enrolled. Commit errors return
no acceptance capability. Reconciliation requires a known committed seal and an
exact operation-ID query without a page limit. It never rolls back generation,
claim or frontier. Inventory exposes an explicit cursor and exhaustion result;
an incomplete page cannot imply absence.

A fresh UOW rediscovers stored gate token, old principal, endpoint and generation
using exact SM session/resource and the successor's current claim. Sealed inventory
and full-payload transfer need no live old principal or old claim. Enumeration
requires no original in-memory capture, frame, lease or operation ID.
Every recovery operation compares the entire immutable stored capture under the
gate lock, binding the asserted current session claim to that exact gate. An
unrelated session's valid claim cannot seal, inspect, reconcile or transfer it.

Journals/transfers remain immutable after canonical collection. They are not
reclaimed here. Total journal count and payload bytes have hard limits (at most
1024 rows and 16 MiB); retained transfer copies can double that payload budget.
Saturation fails closed. Session-gate reclamation remains deferred.

## Retention and candidate writer fence

Candidate order: epoch, principal, exact claim, gate, canonical parent, attempt,
quota, then journal/slot children. Recovery starts with fresh claim and gate;
it does not assert the old principal. Identity guards stay with their transaction.
This is a candidate-local order, not a global production lock-order proof.

Admission invalidates the old tail. Positive ACK/transfer starts a new tail only
after existing canonical blockers settle. The narrow collector checks its exact
terminal operation under the canonical lock, conservatively refuses owner-bound
or rejection-parent/child rows, and removes alias/delivery children atomically.

Database triggers serialize raw candidate admission INSERTs with gate/canonical,
reject missing parents after collection, and prevent canonical deletion while
candidate custody is pending. Gate identity and frontiers cannot roll back.
These triggers cover the new candidate relations, not legacy SM writers. The
disposable runtime role can bypass other repository policy with raw DML; a reviewed
restricted/definer authority boundary is still required for production.

## Verification boundary

`tests/physical_attestation/sm_custody.rs` requires PostgreSQL 17 and a test URL;
actual cases never silently skip. A separately invoked ignored subprocess entry
runs the real binding API in an independent native origin process. The parent
observes a backend inside deferred COMMIT, an ordinary absent read and a waiting
successor seal. Schedules cover origin-death rollback and a stopped origin whose
COMMIT becomes visible before it is killed without reading the reply. The child
is kill-on-drop and the barrier connection is scoped.

Other cases cover full physical identity, payload/lease admission, commit-before-memory
handoff, wrong generation, second-generation denial, wrong/unexposed ACK, transfer
payload/time, wrapping `h`, bounded admission, unsupported tombstone rejection,
pending-delete protection, fresh retention, raw late candidate insertion after
collection, and successor discovery/transfer after old-principal deletion and
claim turnover.
An independent two-claim regression also checks that a forged session/gate
combination leaves the target unsealed and its pending custody unchanged.

## Deferred production gates

Convert or deny every countable live/detached/batch/terminal/relay writer; provide
the real P3 session/endpoint lifecycle and hydration CAS; authenticate durable
tombstone evidence; integrate the real pending sink and retention coordinator;
complete restricted runtime/owner schema transitions; reconcile uncertain backend
connections; drain old binaries, sockets, relay queues and attempts; qualify global
lock order, multiprocess recovery and XEP-0198 behavior before production acceptance,
GC or resume.
