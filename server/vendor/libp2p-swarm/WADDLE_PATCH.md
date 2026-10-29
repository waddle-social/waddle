# Native clustering dependency patch

Source: crates.io libp2p-swarm 0.48.0, upstream rust-libp2p commit
`7171dce2f90c05ba7892d4ba926abb1881db27c7`, directory `swarm`.
Crate SHA-256: `57ccbe1baeaef036ffde4b265871e11c64d29464036bba635378f356bcdca854`.
The MIT license is retained in LICENSE.
Production Rust source is unchanged. Cargo.toml changes the wasm-bindgen-futures
pin from =0.4.58 to =0.4.70 so Cargo can resolve the workspace with Waddle's
matched browser binding versions. Cargo.toml.orig retains the upstream dependency input except for the test logging
removal described below.

Waddle uses libp2p only for native server clustering. The browser client does
not depend on this crate. Upstream capped the bindings because an older
futures-timer/gloo-timers combination was incompatible with newer bindings.
Waddle's lockfile uses futures-timer 3.0.4 and gloo-timers 0.4.0; that historical
incompatibility is not asserted for this dependency graph. Browser libp2p is
untested: revisit this patch before enabling it for wasm. Remove the patch
when an upstream release supports the workspace's binding versions.

Track [upstream PR #6671](https://github.com/libp2p/rust-libp2p/pull/6671),
which replaces the exact cap. Its proposed `^0.4.79` range still excludes
Waddle's `0.4.70`, so merging it alone is not the removal trigger. Remove this
patch after a published swarm release accepts the workspace's matched binding
family, or after upgrading that family together, and verify native clustering
and the browser WASM build with the replacement.

The upstream development log subscriber dependency and four test-only log
initializers are removed from both manifests and the unit test modules. This
keeps local patched sources within Waddle's native client dependency guard,
matching the existing Kameo patch. Test assertions and production logging spans
remain unchanged.
