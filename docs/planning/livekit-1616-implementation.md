# Issue #1616: Restore observable LiveKit webhook ingestion

Source: https://github.com/waddle-social/waddle/issues/1616

## Implementation plan

1. Diagnose the existing namespace-to-handler delivery path using signed, effect-free diagnostic POSTs and packet/flow evidence where access permits.
2. Add real LiveKit ProtoJSON fixtures before fixing createdAt decoding; preserve signature and replay behavior.
3. Repair only the evidenced delivery boundary; prove webhook-silence alert behavior with rule tests.
4. Run focused SFU/server tests, Rust checks, rendered infrastructure checks and workspace tests; record live delivery evidence separately.

## Delivery

Use this isolated issue branch and PR. Preserve unrelated workspace changes. Keep application semantics XMPP-native, reuse existing dependencies, and record native fixture results separately from live provider or production acceptance. Do not merge or deploy as part of preparing the implementation.

