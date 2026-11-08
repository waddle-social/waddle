# Remote-route compiler-site review

Source target: `4219bdd71af9fb0116c82a5c33ab1ed98eecc35d`.
The [site review](remote-site-reviews.tsv) covers the exact compiler keys in
[`capture-4219bdd`](capture-4219bdd/README.md) for `RRE`, `RRETYPE`,
`RRECOMPAT`, `RRESERVED`, `BRIDGE`, `RELAY`, and `MUCKIND`. There are 1,892
clustering keys and no default-profile keys for these tags. The table records
the operation at each marked span; a call or constructed request does not by
itself prove that its effect completed.

| Kind | Reviewed keys |
| --- | ---: |
| RRE | 878 |
| RRETYPE | 594 |
| RRECOMPAT | 90 |
| RRESERVED | 8 |
| BRIDGE | 221 |
| RELAY | 1 |
| MUCKIND | 100 |
| **Total** | **1,892** |

The compatibility route and frame codecs are used by the registered baseline
and live handlers. A second baseline ask is allowed only after an
`UnknownMessage` response and only where the baseline can represent the
request ([selection](../../../server/crates/waddle-server/src/clustering/relay/remote_resource_compat.rs#L30),
[route handlers](../../../server/crates/waddle-server/src/clustering/relay/remote_resource_compat.rs#L155),
[frame handlers](../../../server/crates/waddle-server/src/clustering/relay/remote_resource_compat.rs#L201)).
The v5 registration, v2 update, and v3 unregister identity DTOs and IDs are
[reserved and have no handlers](../../../server/crates/waddle-server/src/clustering/relay/resource_identity_wire.rs#L1).
The `deliver_reserved_muc_proxy` name instead refers to the execution of an
already-reserved ordered relay; it is not evidence that those identity DTOs
have endpoints.

Ordinary remote-frame `Delivered` can reflect enqueue or an existing ingress
disposition. The separate write-accepted endpoint waits for a writer token;
closed, pending, stale, and unavailable remain different results
([ordinary](../../../server/crates/waddle-server/src/clustering/route_bridge/delivery/remote_socket.rs#L431),
[write acceptance](../../../server/crates/waddle-server/src/clustering/route_bridge/delivery/remote_socket.rs#L601)).
A remote unregister `RecordedRetry` records an outstanding obligation; it is
not proof that removal completed
([owner handling](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/owner.rs#L592)).
Fresh-bind retirement and the separate early acknowledgement policy must be
read with their respective custody checks
([socket handling](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/socket.rs#L1214)).

Owner-update admission uses a bounded queue per registration identity. Removing
an interior turn does not grant its successor; only removal of the head grants
the next live turn ([queue removal](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/owner_update_order.rs#L193)).
If readiness closes before a turn is granted, the bridge returns `Unavailable`
without applying that update ([wait](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/owner_update.rs#L55)).
On observing a poisoned lock, admission or turn drop drains the queues and
closes ungranted waiters; an already granted head may finish, and poisoning
alone need not wake waiters immediately. Owner arrival ordering does not fence
in-flight reorder or an update that may commit after a sender timeout.

Two capture rows carry a `test` source-class hint at
[mod.rs:363](../../../server/crates/waddle-server/src/clustering/route_bridge/mod.rs#L363)
and [mod.rs:366](../../../server/crates/waddle-server/src/clustering/route_bridge/mod.rs#L366).
The containing timeout helper is not `cfg(test)`; the `cfg(test)` immediately
below applies to the following helper. Both rows have non-test compiled-target
provenance. Source hints and compilation do not prove runtime selection.

The inventory validator reparsed both retained, hash-checked compiler streams
and matched all 7,958 review keys, including this shard, to the refreshed use
tables. It checked source locations, citations, semantic-label syntax, both
coverage tables and all 52 declaration-file hashes. It also reconciled 83
handler candidates and 179 supplied indirect edges with their reviews. These
checks do not establish capture authenticity, semantic classification
correctness, or discovery of every indirect edge. Independent review remains
separate integration work; no runtime or Cargo test was run for this
reconciliation.
