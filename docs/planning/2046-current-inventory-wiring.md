# Current connection inventory and wiring reconciliation

Status: plan for [#2046](https://github.com/waddle-social/waddle/issues/2046),
based on `main@b2597ee60bbcf9cff2824d7306a66b882bf9b118`. This record will
supplement, not rewrite, the historical
[caller inventory](2046-caller-inventory.md) and
[wiring reconciliation](2046-wiring-reconciliation.md).

## Plan

1. Enumerate current connection-registry, entry, socket-state, registration,
   resource-mirror, room-dispatch and remote-resource symbols and their
   production call paths. Classify state and callers by present authority,
   derived index, transport concern or obsolete migration glue, with evidence
   and explicit limits for textual rather than type-resolved discovery.
2. Reconcile the current build features, production callers, committed
   configuration and test definitions for #1661, #1662, #1986 and #1948.
   Keep configured tests, executed tests and live verification separate.
3. Validate source links and regenerate documented searches against the base
   commit. Record what the snapshot does and does not establish and which
   discovery tasks it can support. Do not change workflows or run Cargo.
