# Dark bounded SM control proof for #2046

## Scope

Add an unselected actor-driver path for one typed XEP-0198 `<r/>` request. Classify the request and reserve its proven maximum outbound payload before handler effects. The admitted operation may produce at most one typed `<a h='…'/>` acknowledgement. Reject every other inbound and peer operation on this bounded path until its output bound or retained continuation is proven.

Keep the existing serving selector and wire behavior unchanged. This slice proves the output payload bound for the admitted operation only; it does not establish a total heap bound, bounded control latency during other handlers, or production readiness.

## Design and verification

1. Define a private capability carrying the fixed output bound and make the driver consume it before dispatch. The retained result is `Option<SmAck>`, so it cannot contain a response batch or multiple frames; tests check the canonical serializer's payload size at decimal-width boundaries.
2. Preserve the sole actor state owner, the acknowledgement ordering, and XEP-0198 handled count semantics. Do not turn a reservation into a claim of delivery or durable custody.
3. Test exact boundary and one-byte over-bound behavior, unsupported operation rejection before effects, cancellation, control queue saturation, and acknowledgement order. Consult the official XEP-0198 source for wire behavior.
4. Run focused server tests, formatting, and scoped strict Clippy. Obtain independent read-only review of the final diff and evidence before marking the draft ready.

## Later gates

Variable-output producers need proven bounds or chunked retained continuations, with ordered writer or replay-owner handoff and preserved credits. Real control progress needs safe continuation boundaries and a bounded recovery path for stalled effectful handlers. Production activation also needs the remaining #2046 custody, authority, and qualification gates.

The 128-byte reservation covers at most one acknowledgement produced by `SmAck::to_xml`; it does not include a later transport close or other socket buffers. No acknowledgement after a checkpoint failure means the connection state has been put into closing; loss of authority is retained as an unknown completion. A future adapter must handle those terminal cases and their output custody separately. The request currently uses the driver's ordinary data queue, while the independent control queue contains only a barrier: this slice does not provide independently serviced SM control during a stalled handler.

The generic inbound and recipient core methods remain compiled for future work, but the bounded driver cannot call them until their producers have a proven bound or retained continuation. Their narrowly scoped dead-code expectations identify that gate in test-support builds.

A checkpoint result remains in operation custody when authority changes: a computed acknowledgement is retained only as an unknown completion, never as a current result. A failed checkpoint seals admission before publishing its result; queued requests remain not started. Unknown handler results also seal the ledger without relying on the separate authority watcher to run first.
