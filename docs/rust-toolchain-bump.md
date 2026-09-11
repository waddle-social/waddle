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

   If nix is not installed locally, run the same command in a container from
   the repository root. The container runs as root, so on Linux hosts
   `chown` the lock back afterwards (Docker Desktop on macOS remaps it for
   you):

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

6. After merge the server auto-deploys. Check
   `infrastructure/waddle.cloud/gitops/waddle-server/helmrelease.yaml` before
   merging: a bump inherits whatever `updateStrategy` that file currently
   carries, and a migration cutover may have left it on `Recreate`, which
   drops every connection instead of rolling. Wait for the flip-back to
   `RollingUpdate` unless a hard cutover is acceptable. Either way, do the
   standard post-deploy check (pods healthy, alerts quiet, ingress and relay
   metrics flowing).

A nixpkgs bump can also move other dev-shell tools, and those moves are the
likeliest source of fallout. Run the full PR pipeline and treat a newly
failing non-Rust gate as a tool regression to investigate rather than a flake.
Two such pins already exist for that reason:

- `flake.nix` holds `cue` at 0.16.1 because 0.17.1 does not terminate on the
  `server/` CUE package (#1763). Remove that override only after measuring
  `cd server && time cue vet .` on the candidate version, never because an
  upstream issue looks closed.
- `flake.nix` asks for `postgresql_17` explicitly so the major the server
  tests run against cannot move with the nixpkgs default.

## Related pins

The Determinate Nix installer action revision lives in
`ci/contributors/nix.cue`. Change it there and regenerate the workflows with
`cuenv sync ci`.

The cuenv version is less obvious, and worth knowing before you regenerate
anything. There are two separate values:

- `cue.mod/module.cue` pins the cuenv **CUE schema** dependency.
- `cuenv_version` in the generated workflows is the cuenv **binary** CI
  installs. It is not read from `module.cue`. `cuenv sync ci` stamps in the
  version of the cuenv binary that runs it, because no project sets
  `config.ci.cuenv.version` and cuenv then falls back to its own
  `CARGO_PKG_VERSION`.

They agree at 0.55.0 today only because both were set together. Two
consequences:

- Bumping `module.cue` alone does not move the CI pin, even after
  `cuenv sync ci`.
- Running `cuenv sync ci` with a newer cuenv installed locally rewrites
  `cuenv_version` across every generated workflow with no `module.cue`
  change. The `cuenv sync -A` in step 3 does the same, because it runs the
  CI provider too, so the "only `digest` lines" expectation there holds only
  when your local cuenv matches the pin. Check
  `git diff .github/workflows` before committing a regeneration.

Move the two together deliberately, or set `config.ci.cuenv.version` to a
concrete version so the CI pin lives in the repo instead of on whoever
regenerates. That field is read per project, so it has to be set in each
project's `env.cue` rather than once at the root.
