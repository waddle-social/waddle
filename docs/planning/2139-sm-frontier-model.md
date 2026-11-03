# P3.3 outbound SM frontier model

Status: an inactive, test-only model for #2139 and #2046, stacked on #2150.
It has no production caller and changes no persistence, socket or wire path.

The model retains a contiguous abstract replay interval and tracks monotonic
`peer_acked <= exposed <= appended` ordinals. Append retains a row but does not
expose it. Immediately before each write attempt, the caller must have current
authority and know that the corresponding exposure fence committed; the model
assumes those facts and advances new exposure one row at a time. A retry of an
already exposed, retained row does not advance it. An exposure fence can
conservatively precede a failed or interrupted write: it is not proof that the
peer received bytes. Once exposed, a guessed or noncausal ACK can advance over
that row even if a crash prevented the write. The caller must enforce ordered
writes and stop or recover after an uncertain write. Replay includes every
retained row after `peer_acked`, including rows never attempted.

The retained interval is capped below half the 32-bit counter space and the
model refuses capacity or ordinal exhaustion before mutation. It interprets an
`h` relative to the acknowledged floor: a forward value through exposure
advances the floor, a forward value beyond exposure returns typed
`HandledTooHigh`, and an exact antipode is rejected without deleting rows. If
an acknowledgement's actual ordinal is between one and `2^31 - 1` behind the
floor, the model identifies it as backward: a live ACK is ignored and a resume
ACK is rejected. The same wire `h` can also represent a far-ahead value, which
the model treats as backward without deleting rows.

Older ACKs outside that backward window can alias an exposed forward ordinal
and advance the floor, deleting retained rows. For example, at floor
`2^32 - 1` with exposure at `2^32`, an old `h = 0` from ordinal zero is
indistinguishable from the current `h = 0` and is accepted as forward. The
model neither authenticates the peer nor proves session or lifetime constraints
that prevent obsolete ACKs from reaching it.

[XEP-0198](https://xmpp.org/extensions/xep-0198.html) specifies wrapping
counters, replay after resumption and error handling for a handled count above
the sent count. This fixture explores the proposed exposure bound; it does not
implement protocol error emission or establish XEP conformance for a runtime
path. A future implementation must establish durable atomicity and commit
certainty, exact session and writer authority, ordered replay/write attempts,
recovery after unknown results, and safe integration with existing custody and
acknowledgement settlement. None is proven by this in-memory model.
