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

## First measured qualification: 2026-09-20

[Run 35510413179](https://github.com/waddle-social/waddle/actions/runs/35510413179)
completed all 11 original measurements at head `f7525e518882e2c2e7c5e04c68ee9f028d2b1ef5`
(checkout merge SHA `342f441c114fca63035c2aec40de92bb76043aa0`). No Waddle build
was permitted. The [final report job](https://github.com/waddle-social/waddle/actions/runs/35510413179/job/106083435842)
contains the joined timings and links to the result artifacts.

| Existing provider | Check-deps restore | Exact archive restore | Whole job | Result |
|---|---:|---:|---:|---|
| [FH + Hestia 2](https://github.com/waddle-social/waddle/actions/runs/35510413179/job/106079467291) | 26.586s | 43.958s | 127s | Complete through FH; Hestia reported an evicted dependency pack |
| [FH only](https://github.com/waddle-social/waddle/actions/runs/35510413179/job/106079467293) | 20.020s | 32.552s | 110s | Complete |
| [Hestia 2 only](https://github.com/waddle-social/waddle/actions/runs/35510413179/job/106079467328) | Miss/error | Miss | 56s | Incomplete; missing pack and deliberately uncached archive |
| [Hestia 3 only](https://github.com/waddle-social/waddle/actions/runs/35510413179/job/106079467282) | Miss | Miss | 43s | No available population; not a throughput result |
| [FH + Hestia 3](https://github.com/waddle-social/waddle/actions/runs/35510413179/job/106079467314) | 24.115s | 31.421s | 111s | Complete through FH; Hestia pushed no paths |

All successful archive probes started without the archive locally. Their
logical closure was 4,602,486,504 NAR bytes; the check-deps closure was
2,084,331,232 bytes. Vendor was already present after the dependency restore,
so its near-zero probe time is not an independent download measurement.
The FH + Hestia 2 archive log explicitly records the exact archive copied from
`https://cache.flakehub.com`. FlakeHub therefore captured it independently of
Hestia's archive exclusion. The 31–44s archive restores justify an end-to-end
FlakeHub transport trial against the previous 157–193s Actions downloads.
These probes were serial; the production trial must measure four concurrent
shards and must not assume identical throughput.

| New provider | Seed job | Cache-save evidence | Fresh-runner warm job | Warm result |
|---|---:|---|---:|---|
| Namespace | [102s](https://github.com/waddle-social/waddle/actions/runs/35510413179/job/106080433144) | Seed reported both mounted paths cached | [42s](https://github.com/waddle-social/waddle/actions/runs/35510413179/job/106083061090) | All three outputs missing; unqualified |
| cache-nix | [120s](https://github.com/waddle-social/waddle/actions/runs/35510413179/job/106080433059) | Save failed after 10s: tar could not read Determinate auth state | [45s](https://github.com/waddle-social/waddle/actions/runs/35510413179/job/106083061181) | Cache key absent; all three outputs missing |
| Magic GHA | [1,002s](https://github.com/waddle-social/waddle/actions/runs/35510413179/job/106080432981) | 899s post step; 1,179 individual uploads, including all three outputs | [66s](https://github.com/waddle-social/waddle/actions/runs/35510413179/job/106083061089) | HTTP 418 / GHA `ResourceExhausted` rate limit; substituter disabled, all three outputs failed |

The Namespace seed and warm jobs used the identical resolved tag
`waddle-ci-benchmark-35510413179` and 20 GB size, with the cache mounted before
Nix installation. Both setup logs reported the two cache paths missing.
[Namespace documents onboarding misses](https://namespace.so/docs/architecture/storage/cache-volumes#cache-volume-onboarding)
when a newly allocated machine lacks the population; that is consistent with
this result, not a proven diagnosis. The retry reuses the isolated experiment
tag and disables warm-job commits, avoiding both repeated onboarding resets
and replacing a useful seed with an empty warm store. Prior population remains
unknown; these retries are not cold-cache tests.

The snapshot failure is a configuration defect, not a provider speed result.
The corrected paths below exclude authentication state without changing its
permissions. Magic's successful seed save did not imply a usable warm cache:
the downloaded per-path logs establish the throttling failure, despite green
diagnostic jobs. Its warm post took only 1s because store diffing was disabled.
No new provider qualified in this first run. The second qualification verifies
the corrected snapshot, repeated Namespace identity, and a real Hestia 3
seed/warm pair. A subsequent focused Namespace retry repairs the fresh-host
installation defect described below, without repeating every provider. After
that retry, remove the PR trigger and retain
manual dispatch so cumulative PR diffs do not repeatedly schedule this long
serial diagnostic workflow.

## Second measured qualification: 2026-09-20

[Run 35513007420](https://github.com/waddle-social/waddle/actions/runs/35513007420)
finished at 14:07:23 UTC on head `cabddd0021504ce69cd8d587540b253505eaba17`.
The [report](https://github.com/waddle-social/waddle/actions/runs/35513007420/job/106090643520)
retains every probe, including the Namespace setup failure. The same immutable
archive and no-build restrictions were used; full payload hashing had not yet
been added.

| Existing provider | Check-deps restore | Archive restore | Whole job | Result |
|---|---:|---:|---:|---|
| [FH + Hestia 2](https://github.com/waddle-social/waddle/actions/runs/35513007420/job/106086009398) | 15.566s | 30.673s | 105s | Complete; Hestia still reported an evicted pack |
| [FH only](https://github.com/waddle-social/waddle/actions/runs/35513007420/job/106086009450) | 10.415s | 12.207s | 75s | Complete |
| [Hestia 2 only](https://github.com/waddle-social/waddle/actions/runs/35513007420/job/106086009449) | Miss/error | Miss | 57s | Incomplete |
| [Hestia 3 only, before seeding](https://github.com/waddle-social/waddle/actions/runs/35513007420/job/106086009396) | Miss | Miss | 45s | Unpopulated, not a throughput result |
| [FH + Hestia 3](https://github.com/waddle-social/waddle/actions/runs/35513007420/job/106086009443) | 9.962s | 11.026s | 73s | Complete |

| Provider | Seed evidence | Fresh-runner warm evidence | Classification |
|---|---|---|---|
| Namespace | [15s; retained volume found, 7.1 GB used](https://github.com/waddle-social/waddle/actions/runs/35513007420/job/106086811754), but stale receipt skipped fresh-host daemon installation | [42s](https://github.com/waddle-social/waddle/actions/runs/35513007420/job/106089533710); both mounted paths absent and all three outputs missed | Integration and population remain unqualified; focused retry below |
| cache-nix | [93s](https://github.com/waddle-social/waddle/actions/runs/35513007420/job/106086811609); corrected snapshot actually saved, 17s post step | [107s](https://github.com/waddle-social/waddle/actions/runs/35513007420/job/106089533803); 4,015,662,037-byte snapshot restored, all three outputs locally valid | Successful seed and fresh restore; snapshot survived the intervening Magic seed |
| Magic GHA | [1,189s](https://github.com/waddle-social/waddle/actions/runs/35513007420/job/106086811627), including 895s post-save | [68s](https://github.com/waddle-social/waddle/actions/runs/35513007420/job/106089533802); all three outputs failed, HTTP 418 / GHA `ResourceExhausted` rate limit again | Unqualified; successful upload does not imply usable restoration |
| Explicitly seeded Hestia 3 | [107s](https://github.com/waddle-social/waddle/actions/runs/35513007420/job/106090058462); all outputs registered, explicit drain 27.502s, manifest `m3#1` | [140s](https://github.com/waddle-social/waddle/actions/runs/35513007420/job/106090318370); dependencies 51.952s, vendor local 0.024s, archive 44.044s, all successful | Successful seed and isolated fresh restore, with FlakeHub excluded |

The successful Hestia 3 warm job establishes real seeded availability; the
earlier unpopulated miss is not its performance result. FlakeHub was faster in
these serial samples, but this is not repeated controlled end-to-end evidence.
Snapshot's near-zero per-path validity probes follow its full setup restore and
must not replace its 107-second whole-job cost. Namespace still needs the
single focused recovery test; no broad provider rerun is planned.

## Focused Namespace recovery: 2026-09-20

[Run 35516047870](https://github.com/waddle-social/waddle/actions/runs/35516047870)
at `9a0c1c84eb7f9a047589b760eabd2dfb9eabe65b` ran only the Namespace pair.
The [26-second seed](https://github.com/waddle-social/waddle/actions/runs/35516047870/job/106093973554)
found both retained cache paths (767 MB used), preserved the receipt, completed
fresh-host installation and connected to its daemon. The installer's earlier
best-effort FlakeHub login had raced the stale socket and failed with connection
refused. All three restores consequently received HTTP 401; no archive payload
read or hash was produced. This proves host recovery, not a seeded cache or a
fast restore. The [43-second warm job](https://github.com/waddle-social/waddle/actions/runs/35516047870/job/106094051662)
landed without either cached path and missed all three outputs. Its hash
comparison therefore remained unverified. The report completed at 14:33:58 UTC.
The next focused retry repeats the installer's supported OIDC
login only for the seed, after bounded daemon readiness, with a 60-second command
deadline and a hard failure if authentication does not succeed. Tokens and
authentication state are not printed, deleted or copied into reports.

## Qualification workflow

`ci/cache-benchmark/workflow.cue` generates
`.github/workflows/waddle-ci-cache-benchmark.yml`. Regenerate or check it with:

```sh
bash scripts/sync-cache-benchmark.sh
bash scripts/sync-cache-benchmark.sh --check
python3 scripts/test-ci-cache-benchmark.py
```

The workflow runs on changes to its own CUE, scripts or this document, or via
manual dispatch once GitHub exposes that trigger. PR pushes now select only
the Namespace seed/warm pair. Manual dispatch offers `scope=all` (the default)
or `scope=namespace`; skipped provider groups do not block the selected pair.
It is an isolated diagnostic
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
| Explicit Hestia 3 seed, then fresh-runner warm | Hestia 3.0.1 | Upload and restore of the same three outputs, including the archive, after all other warm checks finish |

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

Namespace reuses the isolated first-experiment cache tag
`waddle-ci-benchmark-35510413179` and the default 20 GB volume size.
`cache:nix` runs before Nix installation. The warm job permits upstream NixOS
substitution only, so missing Waddle outputs cannot be hidden by a FlakeHub hit.
Its documented `nscloud-cache-exp-do-not-commit` label prevents warm probes
from replacing the seeded volume.

The second run's [Namespace seed](https://github.com/waddle-social/waddle/actions/runs/35513007420/job/106086811754)
restored both cache paths and reported 7.1 GB used. Its retained installation
receipt then caused the Determinate action to skip installation, although the
fresh host lacked `/usr/local/bin/determinate-nixd` and a running daemon. This
is an integration failure, not a cache-speed result. The focused retry preserves
that receipt in a private runner-temporary directory before normal installation,
only when no host daemon binary or `nix` on PATH exists; unexpected host state
fails closed. The pinned installer's
[Linux curing tests](https://github.com/DeterminateSystems/nix-installer/blob/a0b0252e916a0fde9c89fd7917de6134093db944/nix/tests/vm-test/default.nix#L240)
exercise this receipt relocation with an existing store and missing host users,
services or configuration. We never invoke uninstall or `reinstall:true`.
The installer retains existing store/database directories, imports its bootstrap
registrations, and recreates host setup; it can refresh bootstrap paths and
ownership. Setup timing includes this recovery. A daemon ping and the existing
per-output validity probes must then succeed, without compiling anything.

Validity and closure metadata alone need not read lazy volume blocks. Every
successful archive restore in the focused retry and later runs therefore also
streams the full `archive.tar.zst` through SHA256 with a 180-second deadline.
Its byte count, read duration, hash and observed network bytes are recorded
separately from restore time; setup/probe and whole-job time include this read.
A failed or timed-out read fails the probe. The report compares each available
seed/warm pair's path, size and digest and fails on a mismatch. Matching hashes
show pair consistency, not comparison with an independently trusted digest.
This forces archive payload access but does not measure extraction, all closure
files or compilation. The earlier runs did not include this read and cannot be
compared using the later setup/probe total without separating that addition.

Cache-nix uses a run-specific key, disables cache purging, and uses
only that exact key on restore. A key miss is subsequently recorded by the
per-output probes. Its paths input removes the automatic `/nix` root and includes
only `/nix/store` and `/nix/var/nix/db`, retaining database checkpoint/merge
handling while excluding `/nix/var/determinate` authentication and sockets.
A child-only exclusion would still let tar recurse from the included parent.
Neither touches a production Namespace cache identity
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

The explicit Hestia 3 pair runs last so its uploads cannot evict another
provider before that provider's warm check. The seed restores through FlakeHub,
then uses the pinned release's documented `hestia hook` command to register only
the three successful outputs. Since the hook returns zero even on failure, the
helper verifies its acknowledgement before explicitly draining with a 300-second
deadline. The reported `m3` manifest version is passed to the fresh runner's
`wait-manifest-version` input. Hestia's upstream filter matches only
`cache.nixos.org-1`, so it does not exclude FlakeHub-signed Waddle outputs.
The normal post drain has a further 30-second bound for this seed. Registration
and a manifest commit still require successful fresh-runner restoration; neither
alone qualifies the cache. This uses the PR's existing Hestia 3 root rather than
inventing an unsupported action root-key input.

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
store appear free. Nix validity and path metadata do not force every cached
volume block to be read: lazy Namespace volume materialization can defer I/O
until archive extraction or compilation. A real workload must measure that cost.
The seed cost remains visible alongside warm results. The Magic action exposes
no run-specific cache-version input, so its prior GHA population is unknown;
its seed must not be described as a proven empty-cache measurement.

The first run is qualification, not a statistical ranking. Whole-run reruns
reuse seed identities, and Namespace keeps its experiment identity across runs;
the per-path local-presence fields expose that state rather than labelling it
a new cold cache. Repeat eligible
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
