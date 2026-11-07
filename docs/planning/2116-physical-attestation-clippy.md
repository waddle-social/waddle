# Physical-attestation Clippy cleanup

## Objective

Clear the 23 baseline diagnostics from the scoped physical-attestation Clippy
command on `c75ca81c`, while preserving runtime behavior and the currently
unselected candidate work.

## Plan

1. Remove re-exports that have no callers in the selected build, and gate
   feature-specific exports and helpers by the feature that owns their real
   callers.
2. Keep intentional staged APIs available only where their fixture or future
   caller needs them. For APIs that must remain compiled while dormant, use
   narrowly scoped `#[expect(dead_code, reason = ...)]` with an explicit reason;
   do not add crate-wide or broad module suppressions.
3. Avoid changes to runtime behavior, database operations, protocol behavior,
   or unrelated Clippy findings.

## Verification

Run `cargo fmt --all -- --check` and the requested command:

```sh
cargo clippy -p waddle-server --test physical_attestation \
  --features test-support --locked -- -D warnings
```

Use the same source baseline (`c75ca81c`) and report the exact diagnostics,
changed APIs, formatter result, and Clippy result. Check an all-features variant
only if the existing Cargo target has adequate free space; it is supplemental,
not a full workspace or release-CI claim.
