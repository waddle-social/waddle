# CI reference projects and cache experiments

Reviewed 20 September 2026 for [PR #1801](https://github.com/waddle-social/waddle/pull/1801).
These are implementation references, not evidence that the projects meet Waddle's
15-minute target. The timing and coverage contract remains in
[ci-performance.md](ci-performance.md).

## What the reference projects actually do

| Project and inspected revision | Flake | Actual CI and applicable lesson |
| --- | --- | --- |
| Deno `abd22074e47c6a5cd14e9e4e84743f084aa5a575` | Whole-source `buildRustPackage`, pinned prebuilt V8, LLVM/LLD, `doCheck = false`; no Crane compilation layers. | Native Cargo target caches, content-aware mtime restoration, default-branch cache warming, application binaries built once and transferred to selected test jobs; heavier test harnesses split into 2–3 PR shards. Benchmark reusable workspace artifacts as well as Nix substitutions. |
| Union `031785bb6dc6b957c624e62bc64c184409c97d7b` | Custom Crane builder computes package dependency closures, filters sources and rewrites manifests/lockfiles. Packages use `buildDepsOnly` followed by `buildPackage`; some packages are deliberately grouped. | Garnix selects builds; some GitHub workflows delegate to a shared workflow with a remote Nix builder. Whole-workspace tests, Clippy and docs still use broad sources. Borrow source-closure discipline, not a claim that all checks rebuild one crate at a time. |
| Warp `c1a27a6a21bf001fbcb83adc30c80f05b1181dae` | Experimental Linux packaging: Crane vendors dependencies, then `buildRustPackage` builds the application; `doCheck = false`. | Native Cargo/nextest, Namespace Rust/brew caches on Namespace macOS, Swatinem Rust cache elsewhere, reduced debug information, and a documented 16-core/64-GB Linux runner. Compare Cargo-target persistence separately from a persistent Nix store. |

### Deno

The [flake](https://github.com/denoland/deno/blob/abd22074e47c6a5cd14e9e4e84743f084aa5a575/flake.nix)
provides a development shell and binary packaging. Its disabled package tests are
not the test strategy used by the
[CI generator](https://github.com/denoland/deno/blob/abd22074e47c6a5cd14e9e4e84743f084aa5a575/.github/workflows/ci.ts#L307-L403).
CI keys target caches by OS, architecture, profile and job. Main saves new target
caches; PRs restore them. Separate Cargo-home caches avoid redownloading sources.

The [mtime helper](https://github.com/denoland/deno/blob/abd22074e47c6a5cd14e9e4e84743f084aa5a575/.github/mtime_cache/action.js)
requires a clean checkout and keys each tracked file by mode, Git blob, EOL state
and path. It preserves old mtimes only for matching content, allowing Cargo to
recognize unchanged sources after checkout. Blindly backdating changed files
would not reproduce this behavior safely.

[Test jobs](https://github.com/denoland/deno/blob/abd22074e47c6a5cd14e9e4e84743f084aa5a575/.github/workflows/ci.ts#L1362-L1515)
download only the application binaries they need and compile their test harnesses
through Cargo. This is not a complete precompiled nextest archive fan-out. Some
jobs have 30–60-minute timeouts; no comparable end-to-end speed was measured here.

### Union

The [custom builder](https://github.com/unionlabs/union/blob/031785bb6dc6b957c624e62bc64c184409c97d7b/tools/rust/crane.nix)
filters Rust sources plus explicit package/test inputs, computes dependency
closures, and rewrites workspace members, workspace dependencies and Cargo.lock.
Its dependency traversal assumes local dependencies use `workspace = true`;
Waddle also has direct path dependencies, so copying it literally is incorrect.
Package builds normally remove dev dependencies; tests must retain them.

The [workspace checks](https://github.com/unionlabs/union/blob/031785bb6dc6b957c624e62bc64c184409c97d7b/tools/rust/crane.nix#L630-L740)
still use whole-workspace sources and dependency-only artifacts. Union therefore
supports trying narrower Waddle source inputs; it does not prove reusable
compiled workspace layers or a universal per-crate test cache.
[Voyager](https://github.com/unionlabs/union/blob/031785bb6dc6b957c624e62bc64c184409c97d7b/voyager/voyager.nix#L43)
also groups selected modules/plugins into a shared build.

The [flake cache configuration](https://github.com/unionlabs/union/blob/031785bb6dc6b957c624e62bc64c184409c97d7b/flake.nix#L644)
and [Garnix selection](https://github.com/unionlabs/union/blob/031785bb6dc6b957c624e62bc64c184409c97d7b/garnix.yaml)
describe a different service arrangement, not a FlakeHub/Namespace comparison.
The [E2E workflow](https://github.com/unionlabs/union/blob/031785bb6dc6b957c624e62bc64c184409c97d7b/.github/workflows/e2e.yml)
delegates to a pinned shared build workflow. No mold selection or measured
sub-15-minute result was established by this review.

### Warp

The public repository now includes application source. Its
[flake](https://github.com/warpdotdev/warp/blob/c1a27a6a21bf001fbcb83adc30c80f05b1181dae/flake.nix)
uses `vendorCargoDeps`, not `buildDepsOnly`, and is explicitly experimental Linux
support. Crane's presence is not evidence of compiled crate caching.

[Environment setup](https://github.com/warpdotdev/warp/blob/c1a27a6a21bf001fbcb83adc30c80f05b1181dae/.github/actions/prepare_environment/action.yml)
selects Namespace's `rust` cache for Namespace runners and Swatinem elsewhere,
with Swatinem saves restricted to master. A
[warming workflow](https://github.com/warpdotdev/warp/blob/c1a27a6a21bf001fbcb83adc30c80f05b1181dae/.github/workflows/populate_build_cache.yml)
runs on manifest/toolchain changes and weekdays.
[CI](https://github.com/warpdotdev/warp/blob/c1a27a6a21bf001fbcb83adc30c80f05b1181dae/.github/workflows/ci.yml)
compiles nextest tests, then runs package/shell subsets and separate doctests.
It uses line-table debug information and a 25-minute test-job timeout.
Fork-specific SSH exclusions and a disabled remote-server job are not applicable
shortcuts for Waddle's coverage contract. Neither Warp nor Deno's reviewed main
CI setup enabled sccache; this does not establish whether sccache helps Waddle.

## Waddle's measured cache granularity

An isolated snapshot of `02c406d38d31976c4c5bed78cfddc0763e460330` received one
comment in `server/crates/waddle-ecdysis/src/lib.rs`. Before/after Nix evaluation
compared eleven derivations. This experiment performed no builds and changed no
working-tree source files.

| Result | Derivations |
| --- | --- |
| Changed (8) | Test archive, release package, server tests, XMPP unit checks, XMPP XEP checks, Clippy, doctests, WASM extensions |
| Unchanged (3) | CI-test dependencies, release dependencies, vendor directory |

All compared consumers retained the same Cargo dependency-artifact input. The
[broad source selection](https://github.com/waddle-social/waddle/blob/02c406d38d31976c4c5bed78cfddc0763e460330/flake.nix#L380)
explains why an unrelated crate comment also invalidates XMPP checks. Nix caches
derivations; a cache provider cannot repair overly broad derivation inputs.
This experiment establishes invalidation behavior, not rebuild duration.

A second isolated prototype on `f7525e51` narrowed the XMPP workspace to four
local crates while preserving their full directories, extension/WIT inputs,
infrastructure fixtures and sibling server-test fixtures. Ten evaluation probes
showed that unrelated ecdysis source, manifest and dev-dependency-feature edits
left both XMPP check derivations and their dependency artifact unchanged.
Relevant XMPP source/manifests, nextest configuration, WIT and fixture edits
still invalidated the checks. Adding ecdysis as an XMPP dev dependency expanded
the closure to five crates and invalidated it. The complete archive remained
the unchanged control when introducing the prototype.

Offline `cargo metadata --no-deps --locked` preserved all 95 XMPP targets and
declared dependencies/features, but does not establish resolved feature or
compiled-test parity. The prototype is not ready to ship: the original nextest
configuration refers to the excluded server package and fails to parse against
the smaller workspace. Those filters, test groups and profiles were deliberately
left intact for this experiment. No Waddle compilation or timing improvement is
claimed. This demonstrates why Union's approach is worth testing and why copying
only its source filter is insufficient.

The earlier [changed-source compilation](https://github.com/waddle-social/waddle/actions/runs/35504814800/job/106062722062)
reported 1,134 fresh units and 225 dirty units. All 224 units with nonzero compile
time in its Cargo timing data belonged to Waddle workspace packages. Third-party
reuse already works; workspace reuse is the unresolved opportunity.

## Controlled experiments to run

These are hypotheses and benchmark arms, not measured winners. Keep the source
patch, machine, toolchain, flags and complete test inventory constant within
each comparison. Separate exact-result hits, changed-source builds and dependency
changes. Record queue, setup, restore, compile/link, test, transfer and save costs.

| Comparison | Question answered |
| --- | --- |
| Current Hestia 2 + FlakeHub; each provider alone | Which cache performs useful substitution, and what do duplicate uploads cost? |
| Pinned Hestia 3 alone and with FlakeHub | Do newer restore/prefetch behavior and cache management improve the same graph? |
| Namespace Nix volume, where existing access permits | Does retaining the store improve total time versus downloading the same paths? |
| Native Cargo workspace-artifact persistence with validated freshness | Can unchanged workspace crates survive a changed-source checkout economically? |
| Narrow XMPP source closure, followed by a compiled workspace-artifact layer | Can unrelated edits stop invalidating checks, and can relevant edits reuse compiled crates? |
| sccache and LLD/mold under identical compiler settings | What compilation/link work is actually saved, including cache overhead? |

For the source-closure pilot, prove an unrelated ecdysis edit leaves XMPP
derivations unchanged and a relevant XMPP/fixture/config edit invalidates them.
Preserve the complete feature graph, build scripts, fixtures, WIT/config inputs,
dedicated XEP suites, doctests and Clippy `-D warnings`. Keep the all-features
archive as the coverage control. Provider swaps and source-granularity changes
need separate measurements so the source of any improvement remains clear.
