# Native clustering dependency patch

Source: crates.io libp2p-swarm 0.48.0, upstream rust-libp2p commit
`7171dce2f90c05ba7892d4ba926abb1881db27c7`, directory `swarm`.
Crate SHA-256: `57ccbe1baeaef036ffde4b265871e11c64d29464036bba635378f356bcdca854`.
The MIT license is retained in LICENSE.
The Rust source is unchanged. Cargo.toml changes only the wasm-bindgen-futures
pin from =0.4.58 to =0.4.70 so Cargo can resolve the workspace with Waddle's
matched browser binding versions. Cargo.toml.orig retains the upstream input.

Waddle uses libp2p only for native server clustering. The browser client does
not depend on this crate. Upstream capped the bindings because an older
futures-timer/gloo-timers combination was incompatible with newer bindings.
Waddle's lockfile uses futures-timer 3.0.4 and gloo-timers 0.4.0; that historical
incompatibility is not asserted for this dependency graph. Browser libp2p is
untested: revisit this patch before enabling it for wasm. Remove the patch
when an upstream release supports the workspace's binding versions.
