# Bumping the Rust Toolchain

Waddle pins one explicit Rust version in `server/rust-toolchain.toml`
(`channel = "1.98.1"`, plus the rustfmt and clippy components and the wasm and
Android targets). Every build path reads that file:

- Nix and cuenv: `flake.nix` builds the toolchain with
  `rust-bin.fromRustupToolchainFile`, so the server checks, the cuenv tasks on
  `main`, the chat wasm build, and the Android `cargo ndk` build all use it.
- Darwin runners and Xcode Cloud: the Apple workflows and
  `apps/apple/ci_scripts/ci_post_clone.sh` run `rustup toolchain install`
  inside `server/`, and `scripts/build-xcframework.sh` runs cargo from
  `server/`, so rustup resolves the same pin.
- Local development: rustup honours the file whenever cargo runs inside
  `server/`.

## Procedure

1. Edit `channel` in `server/rust-toolchain.toml` to the new stable version.
2. Update the flake inputs so `rust-overlay` knows the new release:

   ```sh
   nix flake update
   ```

   If nix is not installed locally, run the same command in a container:

   ```sh
   docker run --rm -v "$PWD:/src" -w /src nixos/nix:latest \
     nix --extra-experimental-features "nix-command flakes" flake update
   ```

3. Regenerate the cuenv lock, since it digests `flake.lock`:

   ```sh
   cuenv sync -A
   ```

   cuenv 0.55.0 skips any project whose CUE evaluation exceeds its hard
   10-second timeout and then writes the lock without that project's
   `[runtimes.*]` entry (#1480; `chat` is the usual victim). Never hand-edit
   `cuenv.lock`; rerun the command until the diff only touches `digest`
   lines. The same drop can hit `checkRootSyncDrift` in CI, where a rerun
   is the fix.

4. Run the existing enforcement locally and fix every new lint properly. The
   `-D warnings` policy in the Clippy hard rule does not change for a bump,
   and `#[allow(...)]` is not an acceptable fix:

   ```sh
   cd server && cargo fmt --all -- --check && \
     cargo clippy --all-targets --all-features -- -D warnings
   ```

5. Open the PR and expect the first CI run to be slow. The Hestia cache is
   keyed on store paths, so a new toolchain or nixpkgs snapshot rebuilds the
   whole Rust dependency graph once. A rustc `SIGKILL` in `nixTest` on that
   cold run is the known memory-pressure flake; capture the log before
   rerunning so a real failure is not mistaken for it.

6. After merge the server auto-deploys with a normal RollingUpdate. Do the
   standard post-deploy check (pods healthy, alerts quiet, ingress and relay
   metrics flowing).

A nixpkgs bump can also move other dev-shell tools. Run the full PR pipeline
and treat a newly failing non-Rust gate as a tool regression to investigate;
`flake.nix` holds cue at 0.16.1 for exactly that reason (cue-lang/cue#4421).

Related pins that are not Rust but live next to it: the Determinate Nix
installer action revision in `ci/contributors/nix.cue` and the cuenv version
in the generated workflows. Regenerate workflows with `cuenv sync ci` after
changing either.
