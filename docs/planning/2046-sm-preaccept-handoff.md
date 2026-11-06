# Dark source-to-SM pre-accept handoff for #2046

## Scope

Build one unselected, typed handoff for a prepared local direct frame. Carry a known committed full-payload custody operation and exact immutable SM slot from source delivery into a candidate memory admission. Source completion must wait for durable payload custody; count or queue admission must wait for the committed slot. Keep the original operation, source receipt time, resource, session generation and binding identity through cancellation or authority loss. Reject unsupported source classes, a second frame, and a replacement generation before effects.

This slice may use the #2218 test-support and clustering repository and unregistered PostgreSQL schema. Do not select it for live WebSocket, relay, batch, detached, GC or resume paths. The existing production routing and wire behavior remain unchanged.

## Implemented candidate

`sm_custody/handoff.rs` supplies a separate, unselected source contract. It starts
after earlier authorization transactions have closed and before the production
`SendAttemptRepository::start` call. Wrapping the existing production enqueue
closure after its start is incompatible and remains unsupported.

The candidate finishes three transactions in order: full-payload admission plus
attempt start; exact immutable slot binding; then exact attempt completion plus
an immutable `sm_custody_handoffs` marker referencing that slot. Completion checks
the operation's original message, receipt, resource, node incarnation and attempt
token. No pooled admission/bind call occurs while a source transaction holds the
canonical lock. A Completed attempt without the committed marker cannot produce
the opaque handoff capability.

In this candidate, source Completed/Delivered means transfer into durable payload
custody. It never means transport exposure, a successful socket write or client
ACK. No production source completion or wire behavior changes.

`CandidateSmMemory` is an owned one-entry receiver. Fresh principal, exact claim,
gate and complete capture checks precede synchronous hydration. The receiver
rejects a different gate, session, binding or generation, and deduplicates the
immutable operation/ordinal without counting again. Its exclusive borrow hides
provisional memory until the authority transaction succeeds. Commit failure or
future cancellation drops only a new provisional entry; a previously committed
idempotent entry remains intact.

Failures retain a typed operation/session/resource/generation lookup identity;
hydration errors always retain the committed handoff's identity, including when
a mismatched repository rejects it before effects.
That identity alone is not proof of commit. Same-authority exact-ID retry uses
the durable admitted payload and slot, not another allocation. After revocation
or origin death, #2218 recovery can rediscover, inventory and transfer the old
operation under a successor claim; the successor cannot hydrate the old binding.

The receiver is deliberately unregistered. It does not prove unique live endpoint
publication, pump quiescence or cross-process memory ownership.
Idempotence is per `CandidateSmMemory` object: two `new(capture)` receivers can
both hydrate the same handoff. A production singleton/pump publication and
hydration CAS remain separate gates. The existing
one-frame/one-generation schema limits remain enforced, and an unprepared source
or previously started production attempt fails closed.

## Verification

Required PostgreSQL 17 tests in `tests/physical_attestation/preaccept_handoff.rs`
observe deferred COMMIT barriers separately at admission, slot binding and source
completion. They check source state, slot/marker visibility and zero memory before
the corresponding commits. Other cases exercise repeated handoff/hydration,
unsupported source contracts, stale receiver identity, successor recovery after
principal retirement, and post-commit revocation.
The wrong-repository regression checks the original handoff's complete recovery
identity, zero queued frames, and unchanged committed custody.

A native origin process stops inside the source marker's COMMIT; the parent
observes source completion and marker becoming visible together, then kills the
origin before it reads the reply. The durable operation and exact slot remain
recoverable. A separate disposable fault fixture causes an actual hydration
COMMIT error/cancellation and checks provisional memory removal and preservation
of earlier idempotent memory. It changes no runtime schema selection or timeout.

The combined custody suite explicitly invokes its ignored native subprocess
entries; actual PostgreSQL cases require a configured PG17 URL and never skip.
Independent read-only final review remains required before this draft is ready.

## Production gates

A real selector needs one acceptance boundary across live, relay, batch, detached and terminal writers, with multiple-frame allocation, replay and ordered exposure. It also needs authoritative ACK/transfer/tombstone settlement, pending sink adoption, canonical retention and GC, role/epoch migration, unknown-commit reconciliation, old-writer drain, P3 session authority and XEP-0198 qualification. The candidate cannot be described as production custody while any of these are open.
