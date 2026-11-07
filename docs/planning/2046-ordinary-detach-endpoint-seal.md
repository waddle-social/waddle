# Seal the ordinary-detach outbound endpoint

Status: proposed narrow production fix for #2046, pending independent architecture review.

## Problem and boundary

During resumable detach the connection removes its exact registry entry, closes
the outbound receiver, and drains it with `try_recv`. A sender that cloned the
legacy endpoint before removal can race receiver closure after its closed check.
Its reserved permit can enqueue after the drain has observed an empty queue.

## Plan

1. Use the exact `ConnectionEntry` returned by successful owner-gated removal to
   seal that local binding before closing and finally draining its receiver. A
   successful seal orders all legacy sends and reserved-permit commits before
   or after the cutoff. Do not use a newly resolved entry or affect a successor.
2. Define a fail-closed outcome for a missing or mismatched binding or refused
   seal: retain the connection's unresolved queue and cleanup ownership rather
   than reporting a complete final drain. Establish this behavior in the
   architecture gate before selecting an implementation.
3. Audit every changed `Sealed` rejection. Preserve the original frame or its
   durable producer obligation until the caller has a proven refusal and a
   suitable retry or fallback. Ungated `send_to` may retry a current replacement
   only after a proven refusal; owner-gated and pending-flush paths must not
   retarget a different binding or release a successor's claim.
4. Keep publication, identical-entry republication, same-socket rebinding,
   actor/remote mirrors, and stale cleanup isolated by exact owner and binding.
   Verify lock ordering and do not hold lifecycle, route, or admission locks
   across an await or wake a receiver while holding an admission lock.

## Verification

Use deterministic barriers to exercise enqueue-before-seal, seal-before-enqueue,
pre-reserved permits, old cloned senders, receiver close, failed/missing seal,
stale owner, same-socket and different-socket replacements, and unchanged remote
or actor bindings. Check refusal custody and pending claim ownership. Run the
focused registry and WebSocket cleanup regressions, Rust formatting, and strict
scoped Clippy, then request a read-only final review.

This does not make the drained queue durable, resolve uncertain write attempts,
increase the SM replay capacity, or complete the general head/tail ownership or
P3 serving cutover.
