# Waddle CI under 15 minutes

Prepared 20 September 2026. Source baseline: `main` at `c6169598d2426e483c1c7a2c062ca7f1aba352ac`. This is an implementation plan, not a change to the repository or runner settings. All proposed timings below are acceptance targets, not measured speedups.

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

CUE remains authoritative. Regenerate workflows with `cuenv sync ci -A` and verify drift. Preserve Clippy `-D warnings`, dedicated XEP coverage, and the existing CUE version pin.

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
32 CPU / 64 GB test job starting. Observed Namespace overlap reached 64 vCPU;
that supports resource contention as a cause, but does not establish the account's
configured quota. The compiler now starts directly, at higher scheduling priority.
Lightweight PR validation uses GitHub runners; four test workers use the existing
8 CPU / 16 GB profile. The new compiler's memory guard requires at least 56 GiB
available on a nominal 64 GB worker. The ordinary Nix check remains at one Cargo
job for smaller machines.

The replacement test path compiles once, proves that four nextest partitions
cover the original inventory without overlap, and transfers the exact Nix archive
output as a GitHub artifact. Its explicit runtime-library references remain in the
Nix closure. Each worker verifies the expected store path, imports the archive,
checks its partition's inventory, and runs with its own PostgreSQL instance and
unchanged nextest scheduling restrictions. Runtime fixture and executable paths
replace embedded builder paths in tests. Doctests and the distinct XMPP feature
configurations retain their existing jobs.

CUE generates the builder as a single matrix variant for its larger runner, and
four artifact-aggregation jobs for test execution. This is intentional: cuenv
0.55.0's ordinary matrix jobs do not preserve producer dependencies. The final
`nixTest` job depends on all four workers. It is not an always-reporting required
status check; do not install it as branch protection without implementing that
separate gate and merge-queue event support.

Main starts release-image and WASM builds alongside validation. Publishing uses
the same Nix image and five WASM outputs after every validation gate succeeds.
FlakeHub substitution on the publication worker still needs a live main run;
a cache miss may rebuild those outputs, so no publication timing claim is made.
