# Issue #1712: Preserve Muji teardown receiver ownership

Source: https://github.com/waddle-social/waddle/issues/1712

## Implementation plan

1. Reproduce lost leave presence through the existing public call-store teardown seam with a receiver-sensitive WASM boundary.
2. Invoke Muji presence and any affected terminate method on their owning client.
3. Run focused regressions, chat type checks, lint/Knip and the complete chat test suite.

## Delivery

Use this isolated issue branch and PR. Preserve unrelated workspace changes. Keep application semantics XMPP-native, reuse existing dependencies, and record native fixture results separately from live provider or production acceptance. Do not merge or deploy as part of preparing the implementation.

