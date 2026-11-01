# Local connection binding identity

This W1.3 slice of [#2046](https://github.com/waddle-social/waddle/issues/2046) and [#2137](https://github.com/waddle-social/waddle/issues/2137), stacked on the frozen invitation route, adds an explicit local token and bounded P3.2 precursor fixes. An independent architecture gate reviewed these boundaries. It does not select the physical `ConnectionRef` representation or activate endpoint or actor authority.

## Plan

1. Mint a private typed `LocalConnectionBindingId` once in each `ConnectionEntry`, including remote-hosted mirror entries. Clones and re-publication retain it. Keep the XEP-0280 carbons Boolean flag and its handle independent.
2. Replace uses of carbons `Arc` pointer identity as an ownership fence throughout the local registry, actor mirror, retained retries, delayed sends, WebSocket registration/cleanup and local halves of remote-resource registration. Preserve explicit unconditional `None` unregisters. Remote owner mirrors have their own local binding ID; keep registration ID, socket generation and socket node as the existing cross-node wire fence.
3. Add deterministic replacement and delayed cleanup/send tests, plus failed-resume-to-bind and remote-mirror tests where the existing harness permits. Run default and clustering targeted checks, formatting, strict Clippy and diff checks. Review the final diff for missed pointer comparisons.
4. Fence roster and blocklist interest and Carbons toggles at the local registry mutation using the exact binding. A stale or missing binding fails the request without modifying the socket's stored feature flags or issuing the remote mirror update. Remote updates remain best-effort under their existing registration and socket-generation fences.
5. Remove presence under the same connections-key critical section as retirement, for all three unregister and both closed-sender eviction paths. The lock order is connections → presence; presence-map readers release their guards without acquiring the connections map. Characterize replacement during the three metadata requests and while each removal is blocked on presence.

The binding ID is equality evidence for a published entry, not node or database authority. Physical transport identity, durable endpoint CAS, ConnectionActor ownership, SM lifecycle, P2 custody and activation remain separate gates.

Account-wide last-activity and presence-broadcast ordering, remote resource-update revisions, and account-actor authority/persistence remain unresolved P3.2 work. In particular, this local fence is not a complete account snapshot or a cross-node ownership proof.
