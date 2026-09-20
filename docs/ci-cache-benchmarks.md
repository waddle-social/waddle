# Nix cache experiments

The goal remains all applicable changed-code PR checks completing within 15
minutes, including queue, setup, transfer and post-job work. These experiments
measure cache availability and transfer costs. They do not compile Waddle and
cannot establish that goal by themselves.

Waddle currently runs on Namespace compute with FlakeHub and Hestia 2 binary
caches. The Rust workflows do not currently mount a Namespace Nix cache volume.
Cache location and build granularity are separate questions: Nix reuses whole
derivation outputs, while the current Crane checks include the workspace source
in a shared derivation. Changing a source file can therefore invalidate several
checks while the third-party dependency artifact remains reusable. Moving that
artifact to another provider does not change the derivation's source inputs.

## Qualification workflow

`ci/cache-benchmark/workflow.cue` generates
`.github/workflows/waddle-ci-cache-benchmark.yml`. Regenerate or check it with:

```sh
bash scripts/sync-cache-benchmark.sh
bash scripts/sync-cache-benchmark.sh --check
python3 scripts/test-ci-cache-benchmark.py
```

The workflow runs on changes to its own CUE, scripts or this document, or via
manual dispatch once GitHub exposes that trigger. It is an isolated diagnostic
workflow, not a replacement test gate. Every cache job uses Namespace Ubuntu
24.04 AMD64 with 8 vCPU and 16 GB RAM. An Ubuntu gate first waits for the other
observed workflows on the exact PR head SHA to finish, followed by 60 seconds
with none active. It accepts completed failures because qualification must also
diagnose slow failing CI. No Namespace probe can start if the gate fails.
Each matrix is serial, and its successor
waits for all cache post steps, preventing the experiment itself from launching
several competing restore streams. Its total workflow duration is deliberately
not an estimate of normal parallel CI latency.

| Phase | Variants | What it establishes |
|---|---|---|
| Existing cache | FlakeHub + Hestia 2; FlakeHub; Hestia 2; Hestia 3; FlakeHub + Hestia 3 | Whether current dependency outputs are available, and the cost to restore them |
| Seed | Namespace `/nix` volume; cache-nix snapshot; Magic GHA cache | Cost of importing existing FlakeHub outputs and saving each new cache |
| Fresh-runner warm | The same three new caches | Whether a different runner can restore the seeded data without FlakeHub fallback |

Every variant restores these exact outputs from the same checkout:

- `checks.x86_64-linux.waddle-server-check-deps`
- `checks.x86_64-linux.waddle-server-cargo-vendor`

It also restores the immutable shard-1 archive from trial
`02c406d38d31976c4c5bed78cfddc0763e460330`:
`/nix/store/rbb1dwfaldnawr2pnsb5263hgw2pqf9k-waddle-server-test-archive-0.1.0-shard1`.
The exported Actions NAR was 1,950,935,616 bytes. This output is pinned directly,
so edits to the working flake cannot quietly replace it with an uncached new
archive. Its FlakeHub availability is a question being measured; Hestia
intentionally excluded this output. Archive misses must remain visible and
cannot qualify that route as an archive-transfer winner.

The script records their evaluated store paths and source SHA. It sets
`--max-jobs 0` and `--builders ''`, disabling local and remote builds. A cache
miss records an unsuccessful probe and is never scored as a fast result. The
script returns exit 2 for measured misses; the workflow accepts that data outcome
so an expected Hestia archive miss does not masquerade as a product failure.
Setup, provider isolation, malformed data and harness errors remain failures.
Other matrix rows
continue so the report can distinguish missing data from setup or cache errors.
All variants retain `cache.nixos.org` for ordinary upstream dependencies. The
script overrides and then verifies the exact substituter allowlist, excluding
unselected providers. Authentication stays in the action-configured Nix netrc;
the script does not print Nix configuration or credentials.

Namespace uses a run-specific cache tag and the default 20 GB volume size.
`cache:nix` runs before Nix installation. The warm job permits upstream NixOS
substitution only, so missing Waddle outputs cannot be hidden by a FlakeHub hit.
Cache-nix similarly uses a run-specific key, disables cache purging, and uses
only that exact key on restore. A key miss is subsequently recorded by the
per-output probes. Neither touches an existing Namespace cache identity
or intentionally deletes existing GHA caches. GitHub's shared cache quota can
still evict old entries when these seeds are uploaded, so run this after the
active performance trial has finished.

Magic's seed enables both FlakeHub and GHA caching, with a store diff to capture
the downloaded outputs. Its warm job explicitly disables FlakeHub and allows
only the Magic loopback substituter plus cache.nixos.org. This measures the GHA
backend rather than an accidental FlakeHub fallback. Existing Hestia rows use
the cache population already available to the PR. A missing or evicted Hestia
output is a coverage result; it is not a controlled warm-throughput comparison
with a freshly seeded provider.

## Measurements and interpretation

Each job uploads `result.json` and per-output Nix logs. The final report joins
them to GitHub's job and step timestamps, including cache post steps. It records:

- Per-output local presence, successful substitution or miss, and restore time.
- Evaluated source SHA and store paths, which must agree across comparisons.
- Uncompressed closure NAR bytes and closure path count.
- Observed network receive bytes during each probe and setup. These include
  concurrent traffic and are not compressed cache payload byte counts.
- Setup-plus-probe time, queue time when supplied by GitHub, and complete job
  time including uploads. Step timings identify individual setup and post costs.

A failed job, missing report, missing path or reported cache post failure is
ineligible. Restore completion is separate from cache-save verification: Hestia
and other actions can warn about an upload failure while the job still succeeds.
The report explicitly leaves cache post status unverified and does not designate
an eligible winner. Inspect post logs and verify seed outputs through their
fresh-runner warm probes before choosing a provider. Unknown timings remain
unknown. Namespace/snapshot restores happen
during setup, so compare complete job time and setup-plus-probe time; comparing
only the final `nix build` command would incorrectly make an already-restored
store appear free. The seed cost remains visible alongside warm results.

The first run is qualification, not a statistical ranking. Whole-run reruns
reuse the run-specific seed identity; the per-path local-presence fields expose
that warm state rather than labelling it a new cold cache. Repeat eligible
providers on the same inputs before selecting one; record the actual runner,
cache population and eviction outcomes. Do not count a no-change complete
output-cache hit as evidence for the 15-minute changed-code goal.

## Subsequent build experiments

Keep Rust 1.98.1, lockfile, feature set, test inventory, linker, runner resources
and source change fixed. Compare the current route with the best qualifying
cache route using a real changed crate, including compilation, archive transfer
and all test shards. Record the crates rebuilt and Cargo timings so provider
transfer gains cannot be mistaken for incremental compilation gains.

Test these architectural options separately:

- Crane per-package source closures and artifacts: unchanged independent crates
  should retain derivation identities. Tests, Clippy and protocol coverage must
  remain complete; dependency features must match the workspace build.
- Native Cargo target caching with content-validated mtime restoration, under
  the same pinned Nix toolchain. Deno and Warp use native Cargo cache patterns;
  a persistent `/nix` store does not test that architecture. Validate actual
  rebuild sets and full tests before comparing end-to-end timing.
- sccache: first measure hit rate on a small real workspace crate. Upstream
  Rust support requires incremental compilation disabled and excludes crates
  invoking the system linker, including binaries and proc macros. It cannot
  remove every test executable link and requires deliberate Nix sandbox
  integration rather than merely adding an action.

Cachix requires a configured cache and write token; Attic requires server,
storage and authentication. Neither is configured for Waddle, so this workflow
does not invent credentials or provision infrastructure. They remain candidates
if the accessible providers cannot meet the measured requirements.

## Pinned sources

- [Hestia 3.0.1 release](https://github.com/Mic92/hestia/releases/tag/v3.0.1),
  action `f1f4df2801140a36398ed423533c8460618539df`. Its pack read coalescing,
  read-ahead and bulk closure export are released. `read-only`, eviction
  preflight and the newer `prefetch` CLI are on unreleased main as of
  2026-09-20; this experiment does not attribute those features to 3.0.1.
- [Namespace Nix cache mode](https://namespace.so/docs/reference/github-actions/nscloud-cache-action)
  and [volume labels](https://namespace.so/docs/reference/github-actions/runner-configuration).
- [FlakeHub action](https://github.com/DeterminateSystems/flakehub-cache-action)
  and [Magic action inputs](https://github.com/DeterminateSystems/magic-nix-cache-action/blob/2cdbb78a6eed25b3bc8b97f863549be5f6567e55/action.yml).
- [cache-nix-action](https://github.com/nix-community/cache-nix-action/tree/7df957e333c1e5da7721f60227dbba6d06080569).
- [Crane workspace source filtering](https://crane.dev/examples/quick-start-workspace.html),
  [sccache Rust limitations](https://github.com/mozilla/sccache/blob/main/docs/Rust.md),
  [Cachix setup](https://docs.cachix.org/getting-started), and
  [Attic architecture](https://docs.attic.rs/).
