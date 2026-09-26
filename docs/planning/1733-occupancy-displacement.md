# Same-full-JID occupancy displacement (#1733)

Fresh resource replacement must stop the incumbent connection and finish its
generation-scoped cleanup before admitting the replacement. Concurrent binds
must serialize, and cancellation or a cleanup timeout must fail closed. Actual
XEP-0198 resumption retains the original occupancy generation.

## Implementation plan

1. Review the connection registration, cleanup, remote join authority, and
   generation-fenced teardown contracts independently before implementation.
2. Add regression coverage at the existing connection registration, native
   XEP-0045/XEP-0198, room actor, and teardown outbox seams.
3. Implement bounded displacement and serialized publication without holding
   registry map guards across awaits. Preserve normal resume semantics.
4. Resolve the issue's remaining generation/policy findings in room admission,
   remote reconciliation, SFU registration, and durable teardown.
5. Run focused tests throughout, then formatting, workspace Clippy with
   `-D warnings`, the workspace nextest suite and doctests. Independently review
   the integrated changes and fix actionable findings before delivery.

## Scope

Issue #1733, including its linked residual findings. Generation-specific token
issuance and LiveKit participant retirement tracked by #1732 remain separate.

## Verification cases

- A replacement cannot publish while its incumbent is handling or cleaning up.
- Two replacement binds cannot both claim the same full JID.
- Timeout, cancelled bind, or failed cleanup never authorizes replacement.
- Successful resume preserves room membership and occupancy identity.
- Failed room projection preserves incumbent call state.
- Delayed cleanup preserves a same-generation rejoin and a newer generation.
- Durable teardown retains its policy through deduplication, owner loss, and
  reconciliation; resumability exempts only the matching occupancy generation.
- Delayed remote joins cannot overwrite a newer authoritative generation.
