# CI queueing and the runner shape benchmark

CI feels slow because jobs wait for runners, not because they run slowly. This
document records the measurements behind that, the options for fixing it, and
the benchmark that tests the main option. Results are added as runs complete.

## Findings: 2026-11-14 to 2026-11-15

Source: all 1,000 workflow runs and about 2,700 jobs created between
2026-11-14 09:00 and 2026-11-15 23:20 UTC, read from the GitHub Actions API.
Queue wait is a job's `started_at` minus its `created_at`.

| Runner pool | Jobs | Queue p50 | Queue p90 | Worst | Hours queued | Hours running |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Namespace `namespace-profile-linux-x86` (8 vCPU / 16 GB) | 447 | 4.5 min | 48 min | 109 min | 111 | 40 |
| Namespace 32x64 (`nixTest`, `nixTestLocal`) | 116 | 2.5 min | 15 min | 21 min | 8 | 20 |
| GitHub-hosted `ubuntu-24.04` | 1,556 | 2 s | 2 min | 32 min | 17 | 102 |
| Namespace macOS | 58 | 20 s | 29 s | 36 s | 0.3 | 4 |

Job durations are close to their September values (`nixTest` median 15.7 min).

### Two concurrency limits

**Namespace Linux: 64 vCPU / 128 GB.** This is the documented Team plan limit
([resource limits](https://namespace.so/docs/architecture/compute/resource-limits)),
and the data matches it: the combinations that ran at once were eight 8-vCPU
runners, four plus one 32x64, or two 32x64. Of 315 waits longer than two
minutes, 306 ended within 90 seconds of another Namespace job finishing.
macOS has a separate limit and never queued.

About half the waiting minutes fell where this reconstruction shows spare
capacity. Something not visible from the GitHub side also counts against the
limit, or it is applied differently. The Namespace
[concurrency usage page](https://cloud.namespace.so/workspace/usage/concurrency)
is the authority; it was not available for this analysis.

**GitHub-hosted: 20 concurrent jobs** (Free plan). Concurrency peaked at
exactly 20, and 109 of 160 hosted waits over two minutes happened at 20. It
matters only in bursts.

### Where the Namespace capacity goes

Of 955 Linux vCPU-hours in the window:

| Job | Shape | Share |
| --- | --- | ---: |
| `waddle-server-rustTests` `nixTest` (pull requests) | 32x64 | 49% |
| `waddle-server-default` `nixTestLocal` (main) | 32x64 | 13% |
| `waddle-integration` `nixTest` (merge queue) | 32x64 | 3% |
| `waddle-server-namespaceRustChecks` `test` | 8x16 | 7% |
| Android (three workflows) | 8x16 | 10% |
| Everything else | 8x16 | 18% |

A third of pull-request `nixTest` runs (24 of 72) were cancelled by a newer
push. 301 `linux-x86` jobs were cancelled before they got a runner.

### What the 32x64 lane uses

From the `WADDLE_CI_METRIC` lines of one 19-minute `nixTest` run
([job 132977811438](https://github.com/waddle-social/waddle/actions/runs/43768145403/job/132977811438)):

| Phase | Wall time | CPU used | Note |
| --- | ---: | --- | --- |
| Compile | 5.3 min | 311% (about 3 cores) | `CARGO_BUILD_JOBS = 4`; largest process 16 GB |
| Tests, four shards | 12.7 min | 11–37% of one core per shard | each shard is pinned to 8 cores |

Shard 1 took 743 s and shard 4 took 404 s; the slowest shard sets the job's
length. The per-shard figures are nextest's own process tree and may leave out
PostgreSQL. This is one run. The benchmark below exists to replace it with
whole-machine measurements.

### Job priority

The 32x64 label carries `job.priority=1`. Namespace's
[job ordering](https://namespace.so/docs/solutions/github-actions/runner-controls/job-ordering)
says jobs with any priority start before jobs with none once the limit is
reached. Every profile job, including main's, therefore waits behind every
pending `nixTest`.

## Caching findings: 2026-11-16

Source: step timings of every successful job in the same window (14,753
steps), and the logs of one typical slow run of each major job.

Transferring caches is not what costs time. Compilation that no cache covers
is.

| Where the time goes | Share of runner time | Note |
| --- | ---: | --- |
| The job's own work | 83% | mostly compiling, see below |
| Install Nix | 7% | about 18 s per job, 1,130 jobs |
| FlakeHub and Hestia setup and post steps | 5% | about 14 s per job |
| cuenv, Bun and runner setup | 4% | `Install Bun dependencies` averages 35 s |
| Checkout | 2% | |

Inside the Nix lanes, substituting store paths takes 0.6 to 1.0 minutes per
job and the dependency artifact unpacks in about 4 seconds. The earlier
decision to stay on FlakeHub plus Hestia stands; a different cache provider
has little left to win.

### 1. Plain-cargo jobs rebuild every third-party crate on every run

These jobs call cargo directly, outside Nix, and start from an empty target
directory:

| Job | Runner | Crates compiled | Compile time | Job time |
| --- | --- | ---: | ---: | ---: |
| `namespaceRustChecks` `test` | Namespace 8x16 | 660 | 7.9 min | 16.2 min |
| `waddle-server-default` `mainClippy` | Namespace 8x16 | 733 | 7.8 min | 9.7 min |
| `parentMoveNativeProof` | GitHub-hosted | 1,340 over five builds | 16.2 min | 19.7 min |
| `spaceFanoutContractPostgres` | GitHub-hosted | 589 | 9.8 min | 11.8 min |

Only 9 to 19 of those crates are workspace crates. `mainTest`, `mainDoctest`,
`roomChildContractPostgres` and `federationGrantFenceProof` have the same
shape but their logs were not read. The Nix lanes already avoid this through
the shared `waddle-server-deps` artifact.

Options, cheapest first:

- Persist the cargo target directory for third-party crates on the Namespace
  cache volume. The `linux-x86` profile already has a 50 GB volume, and the
  Android jobs use `nscloud-cache-action` for Gradle. The cuenv sandbox path
  changes between runs, which needs checking: cargo may treat path-dependent
  outputs as stale.
- Move the GitHub-hosted contract jobs onto the Nix dependency artifact.
- sccache. `docs/ci-cache-benchmarks.md` lists it as untested for Waddle.

Rough size: 3 to 6 minutes off each of these jobs. On Namespace that also
returns capacity: `namespaceRustChecks` `test` alone is 7% of the pool.

### 2. `waddle-server` is compiled again in about ten jobs

Clippy, doctest, the CI build, the image build, the test archive, the XMPP
lanes and the contract jobs each compile the `waddle-server` crate for 4 to 7
minutes, in different configurations. Because the pull request changed that
source, no cache can supply it. Two ways to shrink it, neither a cache change:
run fewer configurations per pull request, or split the crate so an edit
invalidates less.

### 3. Fixed setup on very short jobs

Nix, FlakeHub, Hestia and cuenv setup is about 40 seconds per job. For
`checkCiDrift`, `checkAlertRules` and `checkSwitchableAlternativeProgram` that
is half the job. Merging the one-minute checks into one job removes most of
it and also eases the GitHub 20-job limit.

### 4. Smaller leads, not verified

- Android: the first Gradle build of a run executed 102 of 111 tasks with 9
  from cache, and stored a new configuration cache entry every time, although
  `~/.gradle` is on a Namespace cache volume. The cause was not investigated.
- Chat: `Install Bun dependencies` runs 146 times at 35 seconds with no
  package cache.

## Options

| # | Change | Expected effect | Status |
| --- | --- | --- | --- |
| 1 | Run the shard lane on a smaller machine | Halves or quarters the lane's 65% share | Benchmarked here |
| 2 | Raise `CARGO_BUILD_JOBS` on the machine already paid for | Shorter compile | Benchmarked here |
| 3 | Rebalance the four shards | About 3 minutes per run | Not started |
| 4 | Run the lane less often (drafts, merge queue only) | Removes most cancelled runs | Policy decision |
| 5 | Drop or demote `namespaceRustChecks` `test` | 7% of capacity | Policy decision |
| 6 | Move `fmt` in `waddle-server-default` off the 32x64 machine | Unblocks main's other jobs | Not started |
| 7 | Give main's jobs priority over pull requests | Main stops queueing behind PRs | Not started |
| 8 | Raise the Namespace plan limit | Removes the ceiling | Commercial decision |

## The benchmark

`waddle-ci-runner-benchmark` (source `ci/runner-benchmark/workflow.cue`) runs
the pull-request shard lane, unchanged apart from its machine and the two
settings below, once per variant:

| Variant | Machine | Cores and test threads per shard | Cargo jobs | Memory floor | Question |
| --- | --- | ---: | ---: | ---: | --- |
| `32x64` | 32x64 | 8 | 4 | 56 GiB | Baseline: today's lane |
| `32x64-j16` | 32x64 | 8 | 16 | 56 GiB | Does the compile use the cores if allowed to? |
| `16x32` | 16x32 | 4 | 4 | 28 GiB | Same result on half the machine? |
| `16x32-j8` | 16x32 | 4 | 8 | 28 GiB | Half the machine, faster compile, within memory? |
| `8x32` | 8x32 | 2 | 4 | 28 GiB | Does a quarter of the vCPUs still pass? |

The variants are `benchmarkShardLanes` in `flake.nix`.
`WADDLE_NEXTEST_BENCHMARK_LANE` selects one in
`server/scripts/run_nextest_shards.py`; unset, the script runs the production
lane exactly as before. A benchmark lane runs the same tests and the same
required-test gate but writes no qualification evidence.

`scripts/ci-machine-usage.py` samples `/proc/stat` and `/proc/meminfo` every
two seconds and reports, per variant: lane and compile time, the slowest shard,
busy cores during compile and during tests, peak memory, vCPU-minutes, and how
many such jobs fit under the limit at once.

### Running it

```sh
bash scripts/run-runner-benchmark.sh
```

This pushes the current commit, plus one throwaway commit that appends a
comment to the server crate, to `ci-benchmark/runner-shapes`. The extra commit
gives every variant a source tree no cache has seen, as a pull request that
edits the server does; without it a later pass substitutes earlier archives
and reports no compile time. Each benchmark lane also has its own archive
derivation, including `32x64`, which repeats the production settings. Run the
script again for another pass and delete the branch afterwards. To run only
some variants, name them:
`bash scripts/run-runner-benchmark.sh runner-shapes 16x32,8x32`. After the
workflow is on `main` it can also be dispatched manually. The comparison table
is in the run's `runner-benchmark/summary` job; raw samples and metrics are in
the `runner-benchmark-*` artifacts.

It is deliberately not a `pull_request` workflow: `scripts/integration-ci.ts`
mirrors every such workflow into the merge queue as a verification lane.

Variants run one at a time and carry no `job.priority`, so the benchmark holds
at most one machine and ordinary CI is scheduled first. A full pass takes
about two hours of wall time.

### Reading the results

- The lane must pass. A smaller shape that fails tests or runs out of memory
  is not a candidate, whatever its time.
- Compare lane minutes against the baseline, then vCPU-minutes. A variant that
  is slightly slower but holds half the vCPUs reduces queueing for everything
  else.
- Each variant runs once per pass. Treat differences under about a minute as
  noise until a second pass agrees.
- Not measured: queue wait (the point of the change, but it depends on load),
  the `Validate CI helper contracts` step, and cold caches.

## Results

### Preliminary: tests only, 2026-11-16

[Run 43779552061](https://github.com/waddle-social/waddle/actions/runs/43779552061)
ran the production lane on 32x64 and was then cancelled: the compiled archive
came from a cache, so there is no compile measurement and it is not a
baseline. Its test phase is the first whole-machine measurement of the lane:

| Measure | Value |
| --- | ---: |
| Lane time, tests only | 14.4 min |
| Busy cores during tests, mean / p95 / max | 1.4 / 4.8 / 10.0 of 32 |
| Peak memory during tests | 3.4 GB of 64 |
| Shard times | 749 s, 590 s, 598 s, 391 s |

The job waited 58 minutes for a runner. During the test phase the machine was
about 95% idle, PostgreSQL included.

### Pass 1, 2026-11-16

[Run 43787962349](https://github.com/waddle-social/waddle/actions/runs/43787962349).
Every variant compiles from scratch.

| Variant | Outcome | Queue wait | Lane | Compile | Slowest shard | vCPU-min | Busy cores compile (mean / max) | Busy cores tests (mean / max) | Peak memory |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `32x64` (baseline) | pass | 72 min | 20.8 min | 5.9 min | 12.7 min | 665 | 10.1 / 26.9 | 1.6 / 11.0 | 19.6 GB |
| `32x64-j16` | pass | 116 min | 20.8 min | 5.7 min | 13.2 min | 665 | 8.1 / 31.9 | 1.7 / 11.4 | 20.3 GB |
| `16x32` | invalid (see below) | 161 min | — | 6.1 min | — | — | 6.5 / 13.9 | 1.6 / 14.8 | 18.7 GB |
| `16x32-j8` | invalid (see below) | 180 min | — | 6.2 min | — | — | 6.7 / 15.9 | 1.5 / 15.0 | 19.4 GB |

Baseline observations:

- The machine averaged 4.1 busy cores of 32 over the whole job and never used
  more than 20 GB of its 64.
- Compile used 10 cores on average with `CARGO_BUILD_JOBS = 4`: rustc and the
  linker run their own threads inside each job. The earlier 311% figure was
  one process tree, not the machine.
- Shard times were 761 s, 604 s, 609 s and 395 s, within 2% of the
  preliminary run. Shard 1 sets the lane length.
- The job waited 72 minutes for a machine and ran for 21.

`32x64-j16`: raising `CARGO_BUILD_JOBS` from 4 to 16 saved 13 seconds of
compile (341 s against 353 s) and nothing on the lane. The compile briefly
used all 32 cores and then averaged fewer than the baseline, so it is limited
by a few large crates that build one after another, not by the job count.
Option 2 is not worth pursuing on its own.

`16x32` and `16x32-j8` failed because of the benchmark's own configuration,
not the machine. Both stopped shards 1 and 4 after about 30 seconds with
"tests were not run" and no failing test (`16x32` also hit the known
`xep0198_durable_ws` restart-resume flake once).

- Cause: the first version of these lanes pinned each shard to 4 cores but
  still passed `--test-threads 8`. With more test threads than cores, nextest
  0.9.143 silently drops the tests of a `max-threads = 1` group that are
  queued behind a `threads-required = 'num-cpus'` test in the same group. The
  `postgres` group has exactly that shape. A 275-test toy crate reproduces
  it: pinned to 4 CPUs, `--test-threads 8` runs 266 tests and fails;
  `--test-threads 4` runs all 275.
- Fix: every lane now passes `--test-threads` equal to its cores per shard.
  Production is unchanged at 8 and 8.
- Consequence for a real change: `--cores` and `--test-threads` must move
  together. The lane fails loudly if they do not, but only by not running
  tests.
- What did complete on 16x32 was not slower: compile took 366 s and 371 s
  against 353 s, and shards 2 and 3 took about 570 s and 585 s against 604 s
  and 609 s. Peak memory was 19 GB of 32, set by the compile.
- The `8x32` variant was cancelled; it had the same fault.

The three smaller variants are re-run as pass 2 below.

### Pass 2, 2026-11-16 (in progress)

[Run 43809236991](https://github.com/waddle-social/waddle/actions/runs/43809236991):
the three smaller variants with test threads equal to cores per shard.

| Variant | Outcome | Queue wait | Lane | Compile | Slowest shard | vCPU-min | Busy cores compile (mean / max) | Busy cores tests (mean / max) | Peak memory |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `32x64` (baseline, pass 1) | pass | 72 min | 20.8 min | 5.9 min | 12.7 min | 665 | 10.1 / 26.9 | 1.6 / 11.0 | 19.6 GB |
| `16x32` | pass | 13 min | 24.0 min | 7.5 min | 13.4 min | 384 | 6.5 / 14.0 | 1.8 / 8.1 | 18.5 GB |

`16x32` passes all tests on half the machine: 3.2 minutes slower (15%) for
42% fewer vCPU-minutes, and two fit where one 32x64 does.

- Tests cost 40 s on the slowest shard (801 s against 761 s); shard times
  were 801, 644, 656 and 450 s.
- Compile took 448 s here but 366 s and 371 s in the two invalid pass 1 runs
  on the same shape and settings. That 80 s spread is run-to-run noise, so
  the compile penalty of 16 vCPUs is somewhere between 15 s and 95 s.
- Peak memory was 18.5 GB of 32.
