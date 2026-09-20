# Waddle CI under 15 minutes

Prepared 20 September 2026. Source baseline: `main` at `c6169598d2426e483c1c7a2c062ca7f1aba352ac`. The initial plan is followed by the implementation and measurement record for PR #1801. Proposed timing budgets are acceptance targets, not measured speedups.

The primary obstacle is the all-features Rust test build. The latest server PR workflow took 55m51s, including 34m16s compiling tests and 10m51s executing them. Rust CodeQL independently took 19m42s. Both must improve to deliver the requested result.

## What the measurements show

Inspected the latest 50 workflow records, detailed jobs for six selected runs, logs for the main bottlenecks, current CUE/Nix/workflow configuration, and relevant existing PRs. Four recent successful source-changing server PR runs took 54m36s, 54m36s, 54m52s and 55m51s. This is a small diagnostic sample, not an established p95.

| Component | Observed duration | Interpretation |
| --- | ---: | --- |
| Latest server PR workflow | 55m51s | End-to-end workflow elapsed time |
| Time before its `nixTest` job starts | 7m53s | Includes dependency prewarm and scheduling delay |
| `nixTest` Cargo compilation | 34m16s | Largest target for improvement |
| `nixTest` execution | 10m51s | 10,645 passed; 1 skipped; 213 binaries |
| Rust CodeQL job | 19m42s | Extraction about 6m47s, finalization 1m27s, query phase about 10m48s |
| Default-feature XMPP server job | 12m55s | Compilation 4m16s, execution 5m25s, plus setup and cache work |
| Latest main server workflow | 16m36s | Includes publication; serial stages and repeated builds are opportunities |

Sources: [latest server run](https://github.com/waddle-social/waddle/actions/runs/35473755879), [test job and log](https://github.com/waddle-social/waddle/actions/runs/35473755879/job/105979513082), [previous server run](https://github.com/waddle-social/waddle/actions/runs/35470471264), [Rust CodeQL job](https://github.com/waddle-social/waddle/actions/runs/35473755880/job/105979297577), [XMPP compliance run](https://github.com/waddle-social/waddle/actions/runs/35473756169), [main run](https://github.com/waddle-social/waddle/actions/runs/35461966304).

`nixBuildDeps` ended at 22:36:56; `nixTest` started at 22:43:08, another 6m12s later. That delay is visible in job timestamps. Runner capacity or scheduling needs investigation; account quotas were not inspected, so the cause is not proven.

## 1. Establish a reproducible benchmark and preserve coverage

First implementation PR: instrument the current graph before changing its semantics.

- Record event creation, job eligibility, job start, setup/cache restore, Cargo compilation, linking, test execution, artifact transfer, cache upload and check reporting separately.
- Capture Cargo timings, peak resident memory, CPU utilization, archive bytes and nextest per-test results. The current one-job Cargo setting limits concurrent compiler processes; it does not imply that rustc uses only one CPU.
- Record the test inventory separately for default features and all features, including ignored tests, doctests, supported targets, XEP suites and release-image validation. Count equality alone is insufficient: compare test identities and feature configurations.
- Use representative server, XMPP, shared client, frontend, CI, dependency and toolchain changes. Distinguish a warm dependency cache from an exact-source result-cache hit.

The 11m52s workflow on closed, unmerged [PR #1797](https://github.com/waddle-social/waddle/pull/1797) is not proof that this optimization already exists. Its Nix test step took only 81 seconds, while the separate default-feature Cargo job built and ran a different configuration. The observed fast cached result is not comparable to the source-changing 34-minute build.

## 2. Reduce compilation before multiplying test runners

Files: `flake.nix`, `server/Cargo.toml`, `server/env.cue`, and the runner profile configuration.

The [Nix test derivation](https://github.com/waddle-social/waddle/blob/c6169598d2426e483c1c7a2c062ca7f1aba352ac/flake.nix#L362-L382) explicitly sets `CARGO_BUILD_JOBS=1`: its comments document a roughly 10 GB server lib-test compiler process and OOM failures on 8 CPU / 16 GB runners. Removing this cap on the current runner would reintroduce a known failure.

Run an A/B benchmark on an isolated 16 CPU / 64 GB builder, starting with Cargo jobs 2 and 4. Choose the best measured combination of elapsed time, peak memory and cost per successful commit. Do not assume proportional scaling or set jobs to the CPU count blindly.

The [CI test profile](https://github.com/waddle-social/waddle/blob/c6169598d2426e483c1c7a2c062ca7f1aba352ac/server/Cargo.toml#L138-L150) already has LTO off, 16 codegen units, optimization level 1 and line-table debug information. Benchmark optimization level 0 for workspace/test code while retaining optimized dependencies. Measure total build-plus-test time; reject it if slower execution eliminates the compilation savings. Keep production build profiles intact and preserve deliberate feature/profile coverage.

If compilation cannot reach the six-minute budget, use Cargo timings to choose the next structural fix. There are 92 top-level server integration-test roots and 93 XMPP roots. Consolidating related roots into a measured number of domain harnesses can reduce repeated linking, but is contingent on linking being material. Preserve dedicated named XEP test modules, fixture discovery and test identities. Update all nextest binary filters, test groups and machine reservations together. Avoid one giant replacement harness that creates another memory bottleneck. If the server lib-test remains dominant, split its test-heavy domains along real crate boundaries.

Exit condition: a representative changed-source all-features build meets the target without OOMs or reduced coverage. The current evidence does not establish that runner resizing alone can achieve this.

## 3. Compile once per configuration, then shard execution

Files: `flake.nix`, `server/env.cue`, `server/.config/nextest.toml`, and test runner scripts.

Create one immutable nextest archive for each distinct build configuration. Key reuse by package selection and the resolved feature graph, as well as source, target, profile and toolchain. Default-feature and all-features builds require separate artifacts; package-only XEP invocations with `test-utils` can require further artifacts because workspace selection can unify additional dependency features. Preserve each invocation unless equivalence is demonstrated. Fan each archive out to initially four execution shards, using recorded durations to avoid a long tail. Do not run a complete Cargo compilation in every shard. Nextest directly supports [archiving builds](https://nexte.st/docs/ci-features/archiving/) and [partitioning execution](https://nexte.st/docs/ci-features/partitioning/). Retain `cargo test --doc` separately: nextest archives do not replace the doctest gate.

Safety and correctness are part of this change:

- Give each shard a fresh PostgreSQL instance or fully isolated database. Preserve the existing serial PostgreSQL test group within a shard until its shared-table assumptions are removed.
- Preserve exclusive-machine execution for the clustering, cross-node resume and full-server scenarios. Reserve a separate execution lane where useful.
- Use the same source revision, nextest version, target, toolchain, features and profile for archive producers and consumers. Carry fixtures, helper binaries, Nix runtime closures, shared libraries and required environment. A nextest archive alone is not proof that Nix-built tests are portable.
- Fix or reproduce compile-time source paths before splitting workers. Tests such as [capability evidence](https://github.com/waddle-social/waddle/blob/c6169598d2426e483c1c7a2c062ca7f1aba352ac/server/crates/waddle-server/tests/server_capability_manifest.rs#L37-L39) and [CUE scenarios](https://github.com/waddle-social/waddle/blob/c6169598d2426e483c1c7a2c062ca7f1aba352ac/server/crates/waddle-server/tests/xmpp_e2e_cue.rs#L842-L849) use `env!("CARGO_MANIFEST_DIR")`, which embeds the Nix builder's source path. A source checkout or nextest path remap cannot rewrite those strings. Add runtime fixture roots or recreate the required layout, then execute the archive on another runner with a different checkout path, covering capability, scenario and infrastructure drift tests.
- Compare the union of executed test identities with the expected inventory for each configuration. Fail on missing shards, unexpected exclusions or dropped tests. Keep retries at zero.
- Measure transfer and extraction before choosing artifact shape. Large archives can consume the entire savings; split transferable artifacts by domain if measurements require it.

The current [nextest configuration](https://github.com/waddle-social/waddle/blob/c6169598d2426e483c1c7a2c062ca7f1aba352ac/server/.config/nextest.toml) documents why database serialization and whole-machine reservations exist. Do not remove them to make the timing chart look better. The longest observed individual test was about 61 seconds; the three-and-a-half-minute shard budget remains an experiment because grouping and distribution also matter.

## 4. Fix CodeQL and scheduling in parallel

File: `.github/workflows/codeql.yml`; runner capacity settings.

The Rust job uses four query threads and approximately 14.6 GB of CodeQL RAM. Its bottleneck is extraction and analysis, not a long Cargo autobuild step. Benchmark Rust alone on 8 CPU / 32 GB and, if necessary, 16 CPU / 32 GB runners, with explicit resource settings. Target completion within ten minutes including startup. Leave the smaller language jobs on their existing economical runners. GitHub recommends [increasing available memory or cores](https://docs.github.com/en/code-security/reference/code-scanning/troubleshoot-analysis-errors/analysis-takes-too-long) for this case.

Retain the current query coverage and cross-crate analysis. Do not remove production code, split security data-flow boundaries blindly, or defer Rust security scanning to a scheduled run to claim sub-15-minute PR checks. Add same-PR cancellation to CodeQL, which currently lacks a concurrency block. Generated server workflows already cancel superseded runs.

Measure eligible-to-start delay and reserve sufficient builder and execution capacity for the new fan-out. Start expensive builds early; consolidate short setup-heavy checks only where failure reporting remains clear. Preserve the dependency-prewarm job until an A/B test establishes that removing its dependency will not duplicate cold builds or recreate concurrent cache-compression memory pressure. Reducing queue time is a separate requirement from reducing execution time.

## 5. Reuse verified outputs and align PR with main

Files: `server/env.cue`, `flake.nix`, `ci/contributors/nix.cue`, generated workflows and cache documentation.

Make PR and main consume the same validated task definitions and preserve both default-feature and all-features coverage. Their current commands differ. Do not assume the default-feature XMPP server job covers every default-feature workspace target from main.

Build release images and WASM extensions alongside tests, then publish those exact verified outputs after checks pass. Main currently waits for validation before building extensions, and its publishing task compiles the five extensions again on another runner. The generated upload is not paired with a corresponding download in the publisher. Preserve production fat-LTO codegen and packaging verification on PRs; shipping compilation must not disappear outside the measured CI boundary. See [task selections](https://github.com/waddle-social/waddle/blob/c6169598d2426e483c1c7a2c062ca7f1aba352ac/server/env.cue#L113-L174), [extension dependencies](https://github.com/waddle-social/waddle/blob/c6169598d2426e483c1c7a2c062ca7f1aba352ac/server/env.cue#L444-L466) and [publication rebuild](https://github.com/waddle-social/waddle/blob/c6169598d2426e483c1c7a2c062ca7f1aba352ac/server/env.cue#L937-L950).

Narrow Nix source inputs and per-job selection to actual transitive dependencies, retaining manifests, build scripts, fixtures and configuration inputs. Do this after the main bottlenecks are fixed. Hestia and FlakeHub already exist; [their documented coverage excludes ordinary Cargo target directories](https://github.com/waddle-social/waddle/blob/c6169598d2426e483c1c7a2c062ca7f1aba352ac/docs/ci-hestia-cache.md#L14-L18). Cache claims must identify exactly which outputs are reused. Preserve trusted/untrusted cache separation and bind reusable outputs to their complete inputs.

CUE remains authoritative. Regenerate workflows with `cuenv sync ci -A` and `bash scripts/sync-rust-tests.sh`, then verify both generators in drift checks. Preserve Clippy `-D warnings`, dedicated XEP coverage, and the existing CUE version pin.

The open [Dagger trial, PR #1800](https://github.com/waddle-social/waddle/pull/1800), should use the same benchmark and coverage contract. Its own plan acknowledges cold Rust builds without persistent caching. An orchestration migration is not a prerequisite for fixing the measured bottlenecks.

## Timing contract and rollout

Measure from the latest commit's CI event to completion of every applicable check, including scheduling delay, cache/transfer work and reporting. Measure main publication separately from PR checks, also targeting under 15 minutes. External provider outages must be visible, not silently discarded from the report.

| Critical test path stage | Target budget |
| --- | ---: |
| All scheduling, worker bootstrap and dependency preparation, including shard startup | 2m |
| Changed-source compilation | 6m |
| Archive transfer and extraction | 1m |
| Longest test shard | 3m30s |
| Result aggregation and reporting | 30s |
| Total working target | 13m |
| Remaining margin before 15m | 2m |

CodeQL, release-image validation, lint, doctests, XEP feature variants and applicable frontend/mobile checks run alongside this path. Their individual completion budgets must fit within the same 15 minutes. The latest native/client checks were below 15 minutes, but runner contention and shared Rust changes still belong in the validation sample.

Implementation order:

1. Instrument and baseline; benchmark compiler memory/parallelism and Rust CodeQL independently.
2. Land the measured compiler improvement. Apply harness/crate restructuring only if compilation misses budget.
3. Add immutable archives and isolated execution shards, with explicit inventory verification.
4. Align PR/main coverage and reuse image/WASM outputs; tune scheduling and cache invalidation.
5. Introduce an always-reporting aggregate CI check after parity is proven. It must fail on any selected job failure, cancellation or unexpected skip. Ensure merge-queue events run it before making it required; existing workflow-level path filters must not strand checks.

The inspected active main ruleset has merge-queue configuration but no required-status-check rule. Its grouping wait setting is ten minutes; do not confuse that configured setting with an observed delay on every merge. Verify effective enforcement when installing the aggregate gate. Actual merge-queue latency is a separate operational metric.

Initial acceptance: 20 representative changed-code runs complete all applicable checks in under 15 minutes, including default and all-feature variants, queueing, source-cache misses and artifact work. Include dependency and toolchain cache misses in explicit stress cases and report their timings separately; do not declare the universal target met if these still exceed it. Then track rolling p50/p95, cost per successful commit, peak memory, flakes and cache performance. Twenty runs are an initial acceptance sample, not statistical proof of a long-term percentile.

Stop conditions: missing coverage, new OOMs, hidden retries, incorrect cache reuse, or any prerequisite exceeding its budget. Retain the previous complete graph until replacement coverage and artifacts have been demonstrated. A 15-minute timeout alone does not deliver faster successful CI.

## Implementation pilot: PR #1801

Goal: every applicable PR check finishes within 15 minutes of the commit event,
with a 13-minute working budget. A successful cached run alone does not establish
this target. Main publication is measured separately, also against 15 minutes.

The first pilot at `6c9bd09d` kept the complete test suite in one job to measure
compiler parallelism independently. Rust CodeQL used a 16 CPU / 32 GB runner,
16 threads and 28 GB analysis memory, retaining the existing languages and query
configuration. The [CodeQL workflow](https://github.com/waddle-social/waddle/actions/runs/35501509537)
finished in **9m47s** from workflow creation; its Rust job took **8m22s**, compared
with **19m42s** in the baseline. This is one measurement, not a percentile claim.

The [Rust pilot](https://github.com/waddle-social/waddle/actions/runs/35501509477)
exposed **7m14s** of waiting between the dependency-prewarm job finishing and the
32 CPU / 64 GB test job starting. Its compilation finished in **8m34s**,
versus **34m16s** in the baseline; the measured compiler phase including nextest
setup took 516 seconds, with an 11.3 GiB largest-process RSS.
Observed Namespace overlap reached 64 vCPU;
that supports resource contention as a cause, but does not establish the account's
configured quota. The compiler now starts directly, at higher scheduling priority.
PR validation, root synchronization and the smaller XMPP lanes use GitHub runners;
the heavy XMPP server lane and four test workers use the existing
8 CPU / 16 GB profile. The new compiler's memory guard requires at least 56 GiB
available on a nominal 64 GB worker. The ordinary Nix check remains at one Cargo
job for smaller machines.

The replacement test path compiles once and assigns most whole test binaries to
four groups balanced by executable size. The server library and cluster end-to-end
binary are included in every archive so their slow, database-serialized tests can
still be partitioned across four independent databases. Each worker runs its
unique binaries, then its hash partition of the two shared binaries, reusing one
extraction. Inventory checks prove the selected tests cover the original inventory
without overlap; binary overlap is allowed only for those two explicit IDs.
Native nextest binary filters create
four separate archives without rebuilding. Each archive is a separate output
of the same Nix derivation and a separate raw GitHub artifact; a worker downloads
only its own output. Explicit runtime-library references remain in each output's
Nix closure. Each worker verifies the expected store path, imports its archive,
checks its partition's inventory, and runs with its own PostgreSQL instance and
unchanged nextest scheduling restrictions. Runtime fixture and executable paths
replace embedded builder paths in tests. Doctests and the distinct XMPP feature
configurations retain their existing jobs.

The Rust workflow is generated directly from `ci/rust-tests/workflow.cue`, which
imports the task commands from `server/env.cue` and the shared Nix/cache setup.
cuenv 0.55.0's matrix generation neither preserves ordinary dependencies nor
exposes artifact compression, and its task wrapper loads the complete development
shell. The focused generator runs the same Nix tasks directly, uploads the shard NARs without an additional ZIP layer, and retains
every previous path trigger. The metadata travels separately; workers verify the
NAR checksum and expected output before importing it.
Run `bash scripts/sync-rust-tests.sh` after changing those inputs; `checkCiDrift`
checks this generator alongside cuenv. The final `nixTest` gate always reports and
requires the archive and all four workers to succeed. Merge-queue triggers remain
a prerequisite before installing it as a required status check.

Main starts release-image and WASM builds alongside validation. Publishing uses
the same Nix image and five WASM outputs after every validation gate succeeds.
FlakeHub substitution on the publication worker still needs a live main run;
a cache miss may rebuild those outputs, so no publication timing claim is made.

The superseded single-job pilot also failed the concurrent extension groupchat
PostgreSQL test's terminal-receipt assertion. Its 32-CPU runner raised nextest's
default test concurrency from 8 to 32 against one database. The archive workers
restore the baseline 8-CPU concurrency with separate databases. The failure's
cause remains unproven: source review ruled out shared-schema interference and
the response-timeout explanation. An unverified test synchronization change was
reverted. The terminal-receipt assertion, PostgreSQL grouping and production
behavior remain unchanged; retain this failure in the pilot record and investigate
missing receipts if it recurs on the 8-CPU workers.

Validation-only Nix checks no longer export Cargo target directories. Their
commands and dependency artifacts are unchanged; release, WASM, archive and shard
outputs keep their required contents. In the changed-source trial, the
XMPP server check passed all 4102 tests but then uploaded 3.3 GiB and spent 81 seconds
in Hestia/FlakeHub post-job cache work. These target exports have no downstream
consumer. Removing them preserves cached check results while avoiding that
unnecessary upload; savings still require a live comparison.

The changed-source archive trial compiled with **rustc/Cargo 1.98.1**, the latest
stable release listed by the Rust project on 20 September 2026. The repository
already pins it. A compiled probe using the same Nix compiler and GCC wrapper
confirmed LLD 22.1.8 is selected; no flag disables the faster linker. Those gains
are already in the baseline, so no Rust version bump is needed. See the
[1.98.1 announcement](https://blog.rust-lang.org/2026/09/03/Rust-1.98.1/).
The trial's compile phase took 513 seconds. The visible cgroup lifetime peak was
61.5 GiB with no OOM events; this includes cache and setup memory and is not compiler
RSS. It does not justify increasing Cargo parallelism yet.

The [archive producer](https://github.com/waddle-social/waddle/actions/runs/35502717093/job/106057270776)
waited 5m13s to start, compiled tests in 8m32s, produced a 7.18 GB compressed archive,
and spent 4m09s uploading it. The producer job took 17 minutes before workers could
start. That trial misses the target; the remaining work must reduce scheduling,
archive size/transfer and execution overhead, not count a cache hit as a solution.

One worker in that trial reported a successful artifact download but received an
incomplete file set; the other downloads took about ten minutes but eventually
completed. This matches the open upstream
[download-artifact ZIP failure](https://github.com/actions/download-artifact/issues/454).
The next trial uses the supported raw-file artifact mode with explicit SHA-256
verification. No test retries or assertions were weakened.

The archive trial measured lossless long-range Zstandard compression with a
128 MiB window. Recompression took 67.62 seconds to reduce 7,260,878,677 bytes
to 6,528,387,293 bytes, only 10.1%. Remove this extra pass: its observed cost
exceeds the expected transfer saving at the measured upload rates. Keep
nextest's native archive compression. A larger 512 MiB window was rejected
because pinned nextest cannot decode it without a different extraction path.

At David's suggestion, the compiler experiment compares the pinned Nix
`mold` 2.42.0 package against bundled LLD. A workspace-only rustc wrapper explicitly
selects Nix-wrapped mold and participates in Cargo fingerprints, retaining cached
third-party dependencies. Every archived ELF must identify the pinned mold
version, and runtime library paths are verified before transfer. Acceptance uses
the full changed-source compile
phase, the unchanged test inventories and all passing workers. Published linker
benchmarks are motivation, not Waddle measurements. The experiment keeps release
packaging and its separate validation intact.

The [mold producer](https://github.com/waddle-social/waddle/actions/runs/35504814800/job/106062722062)
compiled in 478.3 seconds versus the previous LLD trial's 512 seconds, about 7%
faster. This is one comparison, not a sustained performance claim. Cargo reports
1,134 fresh dependency units and 225 compiled units; the archive contains the
same 213 test binaries, 10,621 selected tests and one ignored test. All archived
ELF executables identify mold. Raw NAR upload took 44 seconds versus the earlier
ZIP upload's 249 seconds. The builder still took 14m21 after starting, plus its
6m20 queue wait; the complete workflow remains above target.

Cargo's timing report identifies the server library test binary at 352 seconds
and the ordinary server library at 231 seconds, including 165 seconds of code
generation. The next experiment applies named, archive-only `opt-level=0`
overrides to `waddle-server` and `waddle-xmpp`; third-party dependency settings,
release profiles, line tables and test selection remain unchanged. Acceptance
still requires complete passing tests and measured end-to-end latency.

Three workers in that first archive trial subsequently passed: shard 2 ran 2,684
tests in 91 seconds, shard 3 ran 2,630 in 135 seconds, and shard 4 ran 2,573 in
154 seconds. Extraction took 49–56 seconds. Shard 1 did not execute because its
download was incomplete, so this remains a failed run with incomplete coverage.
See [shard 2](https://github.com/waddle-social/waddle/actions/runs/35502717093/job/106059873574),
[shard 3](https://github.com/waddle-social/waddle/actions/runs/35502717093/job/106059873481),
and [shard 4](https://github.com/waddle-social/waddle/actions/runs/35502717093/job/106059873478).

The next run, at `536c6784`, again waited 6m20 for the archive builder. Move
chat's PR job to GitHub's Ubuntu runner to release another shared Namespace
slot; its main deployment runner stays unchanged. The account's actual
concurrency entitlement has not been verified, so this is a measured queue
reduction experiment rather than a guaranteed capacity fix.

That run also exposed a missing system CA bundle in the GitHub runner's Nix
test sandbox. The exact Rust certificate loader found zero roots without an
explicit bundle and 121 roots with the pinned `cacert` bundle. Set
`SSL_CERT_FILE` in the shared test environment, including shard and XMPP jobs;
certificate verification stays enabled and dependency-cache identities are
unchanged. Full CI validation of this correction is pending.

Keep the archive out of the duplicate binary-cache upload: its first upload
took another 57 seconds for 7.1 GiB after the Actions artifact had already been
published. A command-scoped post-build hook filters only the exact archive
output and delegates all dependency outputs to the existing hook. Four focused
tests and a real Nix dependency/archive build verify this behavior. Cache
restores, dependency uploads and global Nix configuration remain in place.
This filters Hestia only: FlakeHub independently watches Nix store events and
has no supported per-path exclusion in the pinned version. In the mold trial,
Hestia took 87.7 seconds and the following FlakeHub drain waited another 52
seconds for the archive. Removing one duplicate upload reduces cache pressure;
the subsequent trial must measure how much critical-path time it actually saves.

The shared 6.54 GB archive still took 9m31–9m43 to download in three raw-transfer
workers; eliminating ZIP improved upload time but did not fix download latency.
All four workers ultimately passed all 10,621 selected tests, but the workflow
completed at 11:04:58 after starting at 10:20:57: 44m01, above target.

The next revision emits four filtered archives instead of sending the entire
archive to every worker. Pure whole-binary partitioning was rejected before CI:
observed server-library PostgreSQL-group tests account for about 391 seconds of
serialized execution, and cluster end-to-end tests another 177 seconds of
whole-machine reservations. Keeping each on only one worker would undo test
parallelism. Share those two binaries and partition their tests, while placing
the remaining binaries uniquely by size. Actual test duration and transfer
balance must be measured in CI. Coverage checks include ignored and zero-test binaries, not just the count
of selected tests. The transfer manifest binds each uniquely named shard payload
to its exact Nix output before any import.

The GitHub-hosted XMPP server lane regressed from 4m13 compilation to 12m47,
after another 5m25 of input restoration. Return that lane to Namespace in a
separate CUE-generated pipeline; unit and XEP lanes stay on GitHub. Trial CodeQL
with 8 CPU / 16 GB, eight threads and 14,336 MB instead of the successful
16 CPU / 32 GB configuration. Together with moving chat's PR lane, the known
simultaneous Namespace allocation becomes 64 CPU / 128 GB. Account limits remain
unverified. CodeQL's original GitHub-hosted four-thread scan used approximately
14.6 GB, but its new eight-thread completion time is still an experiment and must
fit the full 15-minute gate without changing query coverage.

## Hybrid archive trial: `02c406d3`

The [changed-source Rust trial](https://github.com/waddle-social/waddle/actions/runs/35508451230)
finished in **16m57s**, with one failing test; the goal remains unmet. This was a
real workspace rebuild, not an archive cache hit. Compilation took **180.65s**
with the two archive-only optimization overrides, compared with 478.3s before.
The builder started nine seconds after the workflow event and finished in 6m43s.
Four archives were approximately 1.95 GB each; raw uploads took 48 seconds total.
Hestia's drain took 4.18s, with no archive outputs uploaded, and FlakeHub's post
step took 1.11s. The visible cgroup lifetime peak was 60.88 GiB with no memory
events; this is not compiler-phase RSS and does not justify more Cargo jobs.

| Worker | Archive download | Unique-binary tests | Shared-binary tests | Result |
| --- | ---: | ---: | ---: | --- |
| 1 | 3m06s | 17.222s | 261.454s | Passed |
| 2 | 2m37s | 35.991s | 84.860s | Passed |
| 3 | 2m45s | 20.976s | 112.817s | One failed test |
| 4 | 3m13s | 10.286s | 147.133s | Passed |

All 10,645 selected tests executed; 10,644 passed. The 24-test increase from the
preceding trial came from upstream commit `b341e180` entering the PR merge
checkout; exact test-identity comparison found 24 additions and zero removals.
The janitor cancellation test
raced an outer one-second deadline against an intended one-second terminal
cleanup attempt. Its correction uses paused Tokio time and checks both the
cleanup duration and cancellation bound. Chat also exposed an editor test's
dependence on a different test leaving `navigator` installed; its fixture now
owns and restores that global. Neither correction changes production behavior.

The root drift gate caught cuenv setup rewriting the lock from incomplete
project discovery. Setup must preserve runtime locks, and explicit project
checks must fail on evaluation errors. The previously green discovery-based
drift lane is insufficient evidence because cuenv 0.55 can silently omit failed
projects. Setup now synchronizes only root VCS dependencies, and the drift gate
checks each tracked project explicitly. Local generation and its consistency
check passed using a scratch-only copy of cuenv with its evaluation timeout
extended from 10 to 60 seconds. The original binary and CI's 0.55.0 pin are
unchanged; the corrected live checks with that original tool remain required.

The next trial uses nextest's native hash partitioning for the two shared
binaries. Applying the pinned algorithm to this run's measured test durations
estimates serial database/cluster work at 164/161/176/104 seconds, versus
261/85/113/147 seconds with count partitioning. These are scheduling estimates,
not measured future runtimes. Four workers keep 8 CPU / 16 GB and receive
[scheduling priority](https://namespace.so/docs/solutions/github-actions/runner-controls/job-ordering)
to reduce the delayed fourth worker. Inventory equality and complete coverage
remain mandatory.

Other observed workflows finished within 15 minutes: CodeQL 9m51s, server
validation 8m24s, XMPP server compliance 11m18s, smaller XMPP checks 5m46s,
Android PR 10m45s, Android device checks 7m30s and Apple 3m27s. Root sync and chat
failed, so these timings do not establish an all-green result. Measurements run
from workflow creation to last job completion; event delivery and later check
reporting are not included.

[Reference-project review](ci-reference-projects.md) separates Deno/Warp's Cargo
artifact caching from Union's source-scoped package builds. The
[cache qualification](ci-cache-benchmarks.md) tests existing services on identical
outputs without allowing compilation. Provider qualification runs after ordinary
CI and is experimental measurement, not a new required production gate or proof
of changed-code latency. The archive itself is included to test whether direct
Nix substitution can improve the remaining Actions download bottleneck.

## First fully green changed-source trial: `f7525e51`

The [Rust workflow](https://github.com/waddle-social/waddle/actions/runs/35510413164)
passed all 10,645 selected tests and exact inventory checks in **16m26s**.
All other ordinary workflows were green; the slowest was XMPP server compliance
at 11m27s. Server validation took 10m59s, CodeQL 9m12s and chat 5m23s.
Both root synchronization and all six explicit project CI/lock checks passed
using unmodified cuenv 0.55.0. The goal remains unmet.

Compilation took 185.08s; the builder job took 7m16s. All four workers started
within 17 seconds of the builder completing. Shared tests took
163.45/159.90/160.13/104.74 seconds with hash partitioning, reducing the previous
261-second maximum. The janitor regression passed in 0.014s.
Archive downloads still took 154–175 seconds, followed by approximately 70–79
seconds of import, runtime preparation and extraction before tests started.
The last worker completed at 15m46s; scheduling and completing the aggregate
gate added another 40 seconds. Cache qualification starts only after this
ordinary run, so its traffic did not influence these measurements.

The next trial restores each archive through the binary caches first, after
validating current-run metadata and its independently evaluated output path.
Local and remote archive builds are disabled. A fixed content manifest binds
the archive, filters, complete comparison inventories and runtime references;
cache hits are verified again before tests. Only the pinned client's exact
missing-output response permits the existing raw Actions fallback. Corruption,
unexpected paths and other cache errors fail closed. Raw fallback uploads remain
available, and new metrics expose restore, export and checksum costs.

Workers now use a no-compiler derivation and direct nextest execution, retaining
the same source/fixtures, environment and PostgreSQL setup/cleanup. Comparison
inventories are losslessly gzipped to avoid turning incidental metadata paths
into Nix runtime references; all ELF runtime roots remain unchanged. Four native
fixture workers passed with compiler/Cargo/Go/protoc commands blocked, including
helper execution and all eight inventory comparisons. These changes still need
the full four-worker live trial. The new producer manifest changes the archive
derivation. On a complete-output miss, its dependency-only Cargo artifact should
require real workspace compilation again; confirm that in the live Cargo
timings. This is an archive-derivation rebuild with unchanged Rust source, not
a changed-crate or cold-dependency trial.

## Cache-first archive trial: `cabddd00`

The [Rust workflow](https://github.com/waddle-social/waddle/actions/runs/35513007365)
finished in **13m14s**, but failed one group-DM reconciliation test. All 10,645
selected tests ran: 10,644 passed, one failed; exact identity comparison with
`f7525e51` found zero additions or removals. All other ordinary workflows passed
below 15 minutes. From the earliest ordinary workflow event to the final gate,
the run took 13m16s. This failed run does not satisfy the goal.

The builder took 7m26s and performed 182.92s of real workspace compilation;
Cargo reported 225 dirty units and 1,134 fresh dependency units. Rust source
was unchanged, and archive packaging changes triggered the rebuild. Every
worker restored the new archive through the binary cache and verified its
content; none downloaded the raw fallback.

| Shard | Cache restore/verify step | Preparation including extraction | Whole tests | Shared tests | Result |
| --- | ---: | ---: | ---: | ---: | --- |
| 1 | 36s | 32.12s | 15.13s | 157.77s | Passed |
| 2 | 37s | 32.89s | 35.82s | 157.27s | Passed |
| 3 | 35s | 30.25s | 21.37s | 185.79s | One failed test |
| 4 | 39s | 34.52s | 9.56s | 96.50s | Passed |

The concurrent cache steps replaced 154–175s raw artifact downloads; minimal
workers reduced subsequent preparation from about 70–79s to 30–35s. Each worker
fetched 56 additional runtime paths, 163.5 MiB compressed. Archive metadata no
longer references `rust-minimal`; vendor and zstd-sys source references remain.
Do not claim those remaining dependencies were removed.

The failed test was
`admin::channels::group_dm_durable_reconciliation_tests::group_dm_rename_recovers_and_arms_the_committed_config_reservation`.
Its one-second recipient wait expired after it spawned a single outbox drain.
The handler schedules arming asynchronously, while that drain captures a fixed
eligibility timestamp and exits when no row is due. It can therefore run before
arming completes and never deliver the row. The correction waits for the exact
committed row using the existing one-second readiness helper before taking the
drain timestamp. Recipient timeout, write acknowledgement and exactly-once
assertions remain intact. A complete corrected run is still required before
accepting the performance result. The previously corrected janitor test passed
in 0.011s.
