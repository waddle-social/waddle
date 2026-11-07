# Default server strict Clippy (#2046)

## Problem

After the private provisional-binding helpers were gated in #2275, default
strict Clippy reaches `waddle-server` and reports 37 first-party diagnostics.
Most are unused items in release-compiled but unselected P2.1 pinned MAM and
P3 actor-local endpoint candidates. Removing those modules from default builds
would defeat the existing dark production compilation boundary.

## Plan

1. Preserve the release-compiled pinned MAM and actor-local endpoint modules.
   For genuinely dormant items, use narrow item-level `expect(dead_code)` with
   a reason that names the pending selector. Remove the unused default-only
   pinned MAM re-export while preserving its test-support API.
2. Gate only helpers whose sole callers already require `test-support`, and
   simplify the physical-join type annotation that triggers Clippy complexity.
3. Run default, clustering and test-support strict Clippy, relevant focused
   tests, formatting, and an independent exact-diff review. Confirm the
   release candidate modules still compile without `test-support`.

This is a lint and compilation boundary. It does not select the actor or MAM
candidate, enable restricted serving, or complete #2046's release matrix.
GitHub Actions and workflow files remain disabled until final qualification.

## Local verification

Strict `waddle-server --lib` Clippy passed with `-D warnings` in default,
`test-support`, clustering, and all-feature configurations. Six focused
real-socket actor-owner tests passed. Formatting and diff checks passed, and
the source diff received independent review. The pinned MAM PostgreSQL tests,
full workspace suite, and exact-release CI were not run for this lint-only
change; no serving selector was activated.
