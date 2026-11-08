# Failed-resume reservation handback

Issue: #2046 and #2137. This change addresses one failed-resume case: finalizing
a published resume finds a replay gap, the binding has no accepted outbound
frames, and an old producer still holds a queue reservation.
[XEP-0198 resumption](https://xmpp.org/extensions/xep-0198.html#resumption)
recommends allowing a fresh resource bind after the documented resume
failures. This change uses the existing protocol responses and wire shapes.

The WebSocket task owns an opaque receiver paired with its sending endpoint.
During inline resume rollback, it first seals and removes the exact old binding,
then synchronously verifies that its own receiver is open and empty before
reclaiming the endpoint. The proof is the receiver's identity and queue state,
not available sender capacity: a reserved permit consumes capacity even though
no frame has been accepted. The old permit remains tied to the sealed admission
and is refused if its producer later attempts to commit it. It can hold capacity
until released, but cannot enqueue work onto the fresh binding.

A receiver with accepted queued work cannot authorize handback. Likewise, a
receiver from another queue and test-only endpoints without a paired witness use
no new handback proof. The old conservative full-capacity check remains available
for those test and legacy callers. The WebSocket path continues to refuse
handback when the accepted queue is nonempty; #2277 tracks the separate false
bind response and accepted-frame custody defect in that case.

The native XEP-0198 WebSocket regression holds an old reservation across a
replay-gap failure and a fresh bind on the same socket. It checks routed traffic,
the stale reservation's refusal and stale-cleanup fencing. Endpoint tests cover
capacity recovery and reject nonempty or mismatched receivers. These checks do
not establish durable custody, cross-node activation or recovery of accepted
frames; those remain prerequisites to the broader provisional-admission
cutover described in `2046-provisional-endpoint-admission.md`.
