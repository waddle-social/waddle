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

| Variant | Machine | Cores per shard | Cargo jobs | Memory floor | Question |
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

Add the `ci-benchmark` label to a pull request. Add it again to measure a
newer commit. After the workflow is on `main` it can also be dispatched
manually. The comparison table is in the run's `runner-benchmark/summary` job;
raw samples and metrics are in the `runner-benchmark-*` artifacts.

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

None yet.
