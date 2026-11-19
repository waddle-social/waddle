# Issue #1712: Preserve Muji teardown receiver ownership

Source: https://github.com/waddle-social/waddle/issues/1712

## Implementation plan

1. Reproduce lost leave presence through the existing public call-store teardown seam with a receiver-sensitive WASM boundary.
2. Invoke Muji presence and any affected terminate method on their owning client.
3. Run focused regressions, chat type checks, lint/Knip and the complete chat test suite.

## Delivery

Use this isolated issue branch and PR. Preserve unrelated workspace changes. Keep application semantics XMPP-native, reuse existing dependencies, and record native fixture results separately from live provider or production acceptance. Do not merge or deploy as part of preparing the implementation.

## Result

The teardown presence callback invokes `raw.update_muji_presence` on the client instead of extracting it into an unbound function. This preserves the receiver used by wasm-bindgen to read the client handle. The existing session-terminate helper already invokes its method on the client and needs no production change.

The regression exercises the public `tearDownActiveCall` seam with receiver-sensitive external WASM methods. It requires the leave presence followed by the terminate and no call error. Existing stale-call guards, bounded sends and cleanup behavior remain covered by the teardown suite.

## Verification

- Red: the new receiver regression failed on the original implementation because only terminate was sent; leave presence was missing.
- Green: the receiver regression passes, and `bun test tests/call-teardown.test.ts` passes all 95 tests.
- `bun run lint` passes, including rebuilding WASM from the current Rust source, Panda generation and Knip.
- `bun run build` passes; Astro checks 494 files with zero errors and zero warnings (five hints).
- `bun run test --timeout 30000` passes all 4,429 tests across 362 files. An earlier full run hit the default five-second timeout in three unrelated connection-notice rendering tests under host CPU contention; that file passed all five tests separately with the default timeout. No source or test deadlines were changed.
- Independent read-only code review reports CLEAN. `git diff --check` passes.

Dependencies were installed from the existing frozen lockfile in the isolated worktree. Generated WASM declaration/wrapper changes were excluded from the implementation. No live browser or production call was performed.
