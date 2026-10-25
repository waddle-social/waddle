# Rust module and import conventions

Use explicit imports, with each `use` statement importing items from one immediate
module. Group names from the same module, and split nested module paths:

```rust
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
```

This convention applies to production code, integration tests, and separate-file
unit test modules. Keep stable `cargo fmt` as the formatter; the nightly-only
`imports_granularity = "Module"` option is not required. The source check enforces
the convention independently of formatter version:

```sh
python3 server/scripts/check-rust-structure.py .
```

Within a server-only checkout or Nix source, run:

```sh
python3 scripts/check-rust-structure.py --server-only
```

Wildcard imports are permitted only for `pub use child::*` reexports, local enum
variant imports such as `use ErrorKind::*` inside a function, and `use super::*`
inside an inline module. A separate file is not an inline module: its parent
imports must list names explicitly. Prelude traits must also be named explicitly,
including GTK, base64, tracing and wasm-bindgen traits or macros. The workspace's
`clippy::wildcard_imports` lint and source check complement each other because
Clippy's default configuration exempts some parent/prelude wildcards.
Crate-root `pub(crate)` reexports can still trigger its private-import rule;
the existing WASM facade uses precise, non-test lint expectations on those
reexports to retain its surface. The source check independently verifies that
each expectation remains on an allowed reexport, and test builds omit the
expectations because Clippy skips this lint in test crates.

Keep non-leaf modules as facades: declarations, reexports and small glue only.
Put implementation in leaf modules, and reexport items when their established
public paths must remain stable. Preserve the original visibility boundary when
moving types or fields; do not broaden the public API to make an extraction
compile. Keep test module names stable so nextest selection continues to cover
the same cases.

The [large-file split list](planning/rust-large-file-splits.md) tracks the remaining
files over approximately 5,000 lines. Splitting them is separate from broad
filesystem hierarchy work in [PR #2004](https://github.com/waddle-social/waddle/pull/2004).
