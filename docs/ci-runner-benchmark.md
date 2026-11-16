# CI queueing and the runner shape benchmark

CI feels slow because jobs wait for runners, not because they run slowly. This
document records the measurements behind that, the options for fixing it, and
the benchmark that tests the main option. The conclusion is at the end.

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

## Job-by-job review: 2026-11-16

Every job that had not been looked at, from step timings (old window plus
119 runs after #2472) and one typical slow log per job. Nothing here has been
changed or measured after a change; each row is a lead with its evidence.

Since #2472 the shard lane no longer runs on pull requests (manual dispatch
only). It still runs after every merge as `nixTestLocal`, so the 16x32
conclusion below applies there.

| # | Job | Time | What the log shows | Lead |
| --- | --- | ---: | --- | --- |
| 1 | `mainDoctest`, `nixDoctest` | 6 to 10 min | `cargo test --doc --workspace --all-features` compiles 729 crates for 8.3 min to run about 50 doctests | Run doctests only for the crates that have them, or inside the test job |
| 2 | `mainTest`, `namespaceRustChecks` `test` | 15 to 18 min | 9.2 min build: third-party crates finish after 3 min, then 6 min for `waddle-server` and 319 test binaries with the default linker; tests 6.8 min | mold and lighter debug info, as the Nix lane already has; dependency cache |
| 3 | `roomChildContractPostgres` | 9.6 min | 7.7 min compiling all of `waddle-server` to run 5 tests in 3 s; fetches 2.7 GiB of store paths and a rustup toolchain each run | Move the tests behind a smaller crate or onto the Nix test archive |
| 4 | `federationGrantFenceProof` | 4.9 min | rustup toolchain download each run; one test (`postgres_guard_first_stage`) takes 66 s | Small |
| 5 | `parentMoveNativeBuildClosure` | 1 or 7 min | On a cache miss it builds the `waddle-server-clippy` derivation for 5.3 min, the same thing `nixClippy` builds | Depend on the clippy job instead of rebuilding |
| 6 | FFI bindings | 2 to 4 min each | `checkXmppClientFfiBindings` builds the client twice (82 s and 59 s); `mainChecks` repeats it; Android builds it twice per job (4 min in `default`) | One build per job, shared features |
| 7 | One-minute checks | 1.5 min each | Besides 40 s of setup steps, every cuenv task waits 25 to 45 s before its command starts. Seven such jobs per server pull request are 80% overhead | One job for `checkCiDrift`, `checkAllCiDrift`, `checkRootSyncDrift`, `checkAlertRules`, `checkSwitchableAlternativeProgram`, `nixFmt` |
| 8 | `renderDeployment` | 3 min | One silent 65 s step before chart linting; the chart is linted three times | Find the 65 s step |
| 9 | `waddle-android-pullRequest` | 6 to 12 min | No Gradle configuration cache hit; lint analysis of two flavours takes about 4 min; R8 release minify runs on pull requests; SDK packages install each run | Fix the configuration cache; lint one flavour on pull requests |
| 10 | `waddle-android-deviceTests` | 8 to 9 min | 79 s emulator start, 200 s for 60 tests | Fine |
| 11 | `waddle-chat-pullRequest` | 4.2 min | 71 s wasm release build, `astro check` runs twice, Bun install 30 to 45 s | Small |
| 12 | `apple-xmpp-client` | 5.5 min | Six cargo builds of 14 to 33 s, XCFramework 131 s; up from 4.2 min in the old window | Fine; macOS never queues |
| 13 | `nixXmppServerTests` | 26.6 min | The slowest job in the repository: 6 min compile, 18.6 min for 6,113 tests on a 4 vCPU GitHub runner. Runs on main only | Shard it if main's turnaround matters |
| 14 | `nixXmppUnitTests` | 3.5 min | 2 min compiling `waddle-xmpp`, 15 s of tests | Fine |
| 15 | website, colony, design, cloud, linux | 1 to 1.5 min | Setup only | Fine |

Rows 1, 2 and 7 are the largest. Rows 1 and 2 hold Namespace machines after
every merge and on every server pull request; row 7 is seven of the 20
concurrent GitHub job slots for work that takes seconds.

## Options

| # | Change | Expected effect | Status |
| --- | --- | --- | --- |
| 1 | Run the shard lane on a smaller machine | Halves or quarters the lane's 65% share | Benchmarked: use 16x32 |
| 2 | Raise `CARGO_BUILD_JOBS` on the machine already paid for | Shorter compile | Benchmarked: no effect |
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

Queue wait here and in pass 2 counts from the start of the run, so it
includes the variants that ran before. The waits for a machine themselves
were 72 minutes for the first variant of pass 1, 13 minutes for pass 2 and
45 minutes for pass 3; later variants in a pass mostly started within a
minute or two of the previous one finishing.

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

### Pass 2, 2026-11-16

[Run 43809236991](https://github.com/waddle-social/waddle/actions/runs/43809236991):
the three smaller variants with test threads equal to cores per shard.

| Variant | Outcome | Queue wait | Lane | Compile | Slowest shard | vCPU-min | Busy cores compile (mean / max) | Busy cores tests (mean / max) | Peak memory |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `32x64` (baseline, pass 1) | pass | 72 min | 20.8 min | 5.9 min | 12.7 min | 665 | 10.1 / 26.9 | 1.6 / 11.0 | 19.6 GB |
| `16x32` | pass | 13 min | 24.0 min | 7.5 min | 13.4 min | 384 | 6.5 / 14.0 | 1.8 / 8.1 | 18.5 GB |
| `16x32-j8` | **fail** (one test) | 38 min | 22.5 min | 6.2 min | 14.4 min | 360 | 7.0 / 15.8 | 1.8 / 7.6 | 19.2 GB |
| `8x32` | **fail** (one test) | 61 min | 24.1 min | 6.2 min | 16.4 min | 193 | 5.0 / 7.7 | 1.7 / 5.3 | 18.6 GB |

`16x32` passes all tests on half the machine: 3.2 minutes slower (15%) for
42% fewer vCPU-minutes, and two fit where one 32x64 does.

- Tests cost 40 s on the slowest shard (801 s against 761 s); shard times
  were 801, 644, 656 and 450 s.
- Compile took 448 s here but 366 s and 371 s in the two invalid pass 1 runs
  on the same shape and settings. That 80 s spread is run-to-run noise, so
  the compile penalty of 16 vCPUs is somewhere between 15 s and 95 s.
- Peak memory was 18.5 GB of 32.

`16x32-j8` and `8x32` each ran every test and failed one. Lane times are
still comparable because nothing stopped early.

- `16x32-j8` failed `clustering_cluster_e2e`
  `failure_matrix::same_full_jid_bound_concurrently_on_two_nodes_leaves_one_binding`.
  The production 32x64 lane failed the same test the same night
  ([run 43797636635](https://github.com/waddle-social/waddle/actions/runs/43797636635)),
  so this is an existing flake, not evidence against the shape.
- `8x32` failed
  `ledger_preflight::tests::postgres17_ledger_preflight_v1030_read_only_locks_in_order`
  ("database is being accessed by other users" on teardown). That test did
  not fail in any other run here. With two cores per shard it may be a timing
  failure the smaller machine provokes; one run cannot tell.
- `8x32` compiles as fast as the others (373 s) but its tests are slower: the
  slowest shard took 982 s against 761 s on the baseline.
- Raising cargo jobs to 8 on 16 vCPUs made no measurable difference, as on
  32 vCPUs.

### Pass 3, 2026-11-16

[Run 43819149809](https://github.com/waddle-social/waddle/actions/runs/43819149809):
the same three variants again.

| Variant | Outcome | Lane | Compile | Slowest shard | vCPU-min | Busy cores compile (mean / max) | Busy cores tests (mean / max) | Peak memory |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `16x32` | **fail** (one test) | 23.9 min | 6.5 min | 14.0 min | 382 | 6.8 / 14.0 | 1.7 / 6.2 | 18.5 GB |
| `16x32-j8` | pass | 23.8 min | 6.0 min | 14.2 min | 381 | 6.7 / 15.8 | 2.0 / 7.0 | 18.8 GB |
| `8x32` | **fail** (one test) | 28.7 min | 10.4 min | 16.5 min | 230 | 4.6 / 7.9 | 1.7 / 4.9 | 18.0 GB |

- `16x32` failed `xep0198_durable_ws`
  `a_session_live_when_the_process_died_resumes_after_a_restart`, a known
  flake that is being fixed separately.
- `8x32` failed `selector_cluster_characterization`
  `durable_coordinator::stanza_from_the_same_node_to_a_detached_session`
  ("Timed out waiting for XMPP close acknowledgement"), and its compile took
  623 s against 373 s in pass 2.

### Flakiness limits what one run shows

The production lane failed twice out of three attempts on this pull request
during the benchmark, on two different cluster tests
(`live_session_resumes_on_the_other_node` and the `failure_matrix` test
above). A single failure on a smaller shape therefore cannot be blamed on the
shape, which is why the smaller variants ran twice.

## Conclusion

Move the shard lane to 16 vCPU / 32 GB, with 4 cores and 4 test threads per
shard and `CARGO_BUILD_JOBS` left at 4.

| Shape | Valid runs | Lane | Compile | Slowest shard | vCPU-min | Failures |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| 32 vCPU / 64 GB | 2 | 20.8 min | 5.7 to 5.9 min | 12.7 to 13.2 min | 665 | none |
| 16 vCPU / 32 GB | 4 | 22.5 to 24.0 min | 6.0 to 7.5 min | 13.4 to 14.4 min | 360 to 384 | two, both flakes seen elsewhere |
| 8 vCPU / 32 GB | 2 | 24.1 to 28.7 min | 6.2 to 10.4 min | 16.4 to 16.5 min | 193 to 230 | two, neither seen elsewhere |

- **16 vCPU costs about 3 minutes.** The lane takes 2 to 3 minutes longer and
  uses 43% fewer vCPU-minutes. Two lanes fit under the Namespace limit where
  one fits today, so the wait for a machine, which was 13 to 72 minutes
  for the first variant of each pass, should fall by more than the lane grows.
- **Cargo jobs do not matter.** 8 jobs against 4 on 16 vCPUs, and 16 against
  4 on 32, are within run-to-run noise.
- **8 vCPU is not safe.** Both runs failed a timing-sensitive test that failed
  nowhere else, the slowest shard is 3 to 4 minutes slower, and compile time
  varied from 6 to 10 minutes. Two runs do not prove the shape causes the
  failures, but the saving over 16 vCPU is not worth finding out.
- **Cores and test threads must change together.** `--cores` 4 with
  `--test-threads 8` silently skips tests (pass 1).
- **Memory is not a constraint.** No run used more than 20.3 GB.

The shape change itself is not in this pull request. It is one edit to
`productionShardLane` in `flake.nix` plus the runner label in
`ci/rust-tests`.
