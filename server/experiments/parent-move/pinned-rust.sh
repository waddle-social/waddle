# Sourced inside the native Nix shell. Cargo subcommands prefer Cargo-home/bin
# over PATH, and retained HOME can supply compiler-wrapper configuration.
: "${FENCE_RUST_BIN:?enter the repository-pinned native shell}"
export CARGO="$FENCE_RUST_BIN/cargo"
export RUSTC="$FENCE_RUST_BIN/rustc"
export RUSTDOC="$FENCE_RUST_BIN/rustdoc"
export RUSTFMT="$FENCE_RUST_BIN/rustfmt"
export RUSTC_WRAPPER=
export RUSTC_WORKSPACE_WRAPPER=
# cargo-clippy installs its own pinned sibling driver as the workspace wrapper.
