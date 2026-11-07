# Default server Clippy gate (#2046)

## Problem

Default-feature strict Clippy fails in `waddle-xmpp` on private provisional
connection activation helpers. Their registry entry points are already
compiled only for tests or `test-utils`; the internal methods remain in the
default build and trigger four dead-code diagnostics.

## Plan

1. Match each internal helper's `cfg` to its test or `test-utils` caller.
   Preserve the feature-gated provisional activation behavior and public API.
2. Run default-feature and test-support strict Clippy, formatting, and the
   focused registry activation tests. Review the exact diff independently.

This is a build-quality gate only. It does not activate provisional endpoint
publication, change WebSocket routing, or complete #2046's release matrix.
GitHub Actions and workflow files remain disabled until final qualification.
