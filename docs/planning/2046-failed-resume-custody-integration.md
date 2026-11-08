# Failed-resume outbound custody

Issue: #2046 and #2137. This plan covers the WebSocket, connection-registry,
stream-management and client boundaries. It does not select a new serving
authority until the custody and producer contracts below are proven together.

A resumed socket currently publishes an admitting route before its claim has
finished. A replay gap can reject that claim after an outbound envelope has
entered the socket queue. Resetting the resumed state leaves that envelope on a
receiver whose next consumer runs in pre-bind state, while a subsequent bind
can return success without publishing a route. A full detached replay buffer
cannot safely absorb the old queue by evicting its prefix.

## Integration plan

1. Give the failed attempt's accepted envelopes an explicit owner before its
   binding, recipient state and claim context are retired. Preserve the full
   envelope and its pending-row, ingress and write-acceptance obligations
   through queue handoff, recipient processing, transport failure and socket
   shutdown. Bound both frame count and payload bytes, and do not discard an
   accepted envelope when persistence or downstream disposition fails.
2. Publish a resume candidate as non-admitting. Activate the exact candidate
   only after claim completion, principal and node authority validation, and
   local and remote registration agree. Retire only its binding and generation
   on failure, so the authenticated socket can attempt a fresh bind.
3. Propagate an exact, typed pre-enqueue retry with the original envelope
   through local, actor, pending, ordered and remote producer paths. A refusal
   must precede queue admission, archive-frontier advancement and any positive
   delivery acknowledgement. Preserve retry custody on cancellation and full
   queues. Bump the ordered relay wire version if its serialized types change.
4. Coordinate the durable pre-accept and per-frame authority with the P2
   owner. Existing in-memory enqueue, SM counters, terminal promotion and local
   socket writes do not independently prove crash-safe custody. If a complete
   handoff cannot be established, land only a coherent functional subset and
   leave the serving selector closed.

## Verification

Exercise the actual WebSocket path with deterministic publication and cleanup
barriers: failed resume with accepted direct and peer envelopes, same and
different fresh resources, held reservation, full queue and replay gap, stale
cleanup, write failure, cancellation and shutdown. Verify the exact route before
a bind result, preserve pending rows and completion obligations, and test local,
actor and remote pre-enqueue refusals without false delivery acknowledgements.
Run the relevant native XEP-0198 and registry suites, followed by the required
server checks where resources permit. Report process-local, durable and
cross-node evidence separately.
