# Dark bounded SM control proof for #2046

## Scope

Add an unselected actor-driver path for one typed XEP-0198 `<r/>` request. Classify the request and reserve its proven maximum outbound payload before handler effects. The admitted operation may produce at most one typed `<a h='…'/>` acknowledgement. Reject every other inbound and peer operation on this bounded path until its output bound or retained continuation is proven.

Keep the existing serving selector and wire behavior unchanged. This slice proves the output payload bound for the admitted operation only; it does not establish a total heap bound, bounded control latency during other handlers, or production readiness.

## Design and verification

1. Define a private capability carrying the fixed output bound and make the driver consume it before dispatch. Keep output accounting on the completed result as a secondary assertion.
2. Preserve the sole actor state owner, the acknowledgement ordering, and XEP-0198 handled count semantics. Do not turn a reservation into a claim of delivery or durable custody.
3. Test exact boundary and one-byte over-bound behavior, unsupported operation rejection before effects, cancellation, control queue saturation, and acknowledgement order. Consult the official XEP-0198 source for wire behavior.
4. Run focused server tests, formatting, and scoped strict Clippy. Obtain independent read-only review of the final diff and evidence before marking the draft ready.

## Later gates

Variable-output producers need proven bounds or chunked retained continuations, with ordered writer or replay-owner handoff and preserved credits. Real control progress needs safe continuation boundaries and a bounded recovery path for stalled effectful handlers. Production activation also needs the remaining #2046 custody, authority, and qualification gates.
