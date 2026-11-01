# Local connection binding identity

This is a dark W1.3 slice of [#2046](https://github.com/waddle-social/waddle/issues/2046) and [#2137](https://github.com/waddle-social/waddle/issues/2137), stacked on the frozen invitation route. An independent architecture gate cleared this narrow local-token change. It does not select the physical `ConnectionRef` representation or activate endpoint or actor authority.

## Plan

1. Mint a private typed `LocalConnectionBindingId` once in each `ConnectionEntry`, including remote-hosted mirror entries. Clones and re-publication retain it. Keep the XEP-0280 carbons Boolean flag and its handle independent.
2. Replace uses of carbons `Arc` pointer identity as an ownership fence throughout the local registry, actor mirror, retained retries, delayed sends, WebSocket registration/cleanup and local halves of remote-resource registration. Preserve explicit unconditional `None` unregisters. Remote owner mirrors have their own local binding ID; keep registration ID, socket generation and socket node as the existing cross-node wire fence.
3. Add deterministic replacement and delayed cleanup/send tests, plus failed-resume-to-bind and remote-mirror tests where the existing harness permits. Run default and clustering targeted checks, formatting, strict Clippy and diff checks. Review the final diff for missed pointer comparisons.

The binding ID is equality evidence for a published entry, not node or database authority. Physical transport identity, durable endpoint CAS, ConnectionActor ownership, SM lifecycle, P2 custody and activation remain separate gates.
