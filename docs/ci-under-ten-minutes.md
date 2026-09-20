# CI under ten minutes: experiment plan

Follow-up to PR #1801, based on c04dadeb. The baseline preserves 10,645 selected tests and all applicable CI/security checks. Ordinary CI measured 12m44s; the preceding Rust-source-change trial measured 12m56s.

## Goal and acceptance

Target all applicable ordinary PR workflows under 600 seconds on representative changed-source runs, including setup, queueing, cache transfer and reporting. Retain the 900-second reliability budget. Report exact-cache, changed-source, dependency/toolchain-cold and publication cases separately. Do not claim a sustained percentile from a small sample.

## Work

1. Investigate narrower XMPP source closures and reusable workspace artifacts without changing resolved features, native/XEP coverage, nextest scheduling restrictions or fixture inputs.
2. Measure archive packaging and backup-transfer overhead; remove serial work where cache availability and integrity guarantees remain intact.
3. Investigate duration-based shard balance and slow serialized tests, preserving test identity, assertions and database isolation.
4. Tune independent bottlenecks, especially Rust CodeQL and changed-source XMPP checks. Keep every existing query and validation gate.
5. Run controlled changed-source trials, compare all workflow timings and exact test inventories, and obtain independent adversarial correctness/security reviews.

CUE remains authoritative for generated workflows. No production XMPP wire behavior, official XEP shapes, or dedicated conformance suites may change for this work. No Docker-based tests, weakened assertions, retries masking failures, dropped checks or scanner suppressions are permitted. The existing xeps specifications and native conformance constraints apply throughout.

## Starting evidence

The final baseline Rust compile took 176 seconds; the critical shard executed tests for about 197 seconds. Other overhead accounted for about 391 seconds. Independent lanes measured 608 seconds for CodeQL and 493 seconds for Android device tests. XMPP server compliance took 688 seconds on the preceding source-changing trial. Optimizing Rust tests alone cannot deliver a full under-ten-minute result.

Results will be recorded here after implementation and live qualification. No improvement is claimed by this planning commit.

## First implementation trial

The focused trial keeps the existing binary/hash partitions and tests four changes:

- Compile and package once on the 32 CPU / 64 GB builder, then run all four existing Nix shard derivations locally with `--max-jobs 4`. Required Linux sandboxes isolate files, network and IPC; disabled sandbox fallback and remote builders prevent an unisolated execution path. Each shard pins its build shell, PostgreSQL and nextest descendants to eight disjoint allowed CPUs, retaining the previous per-worker concurrency and serialized groups.
- Package the four native nextest archives concurrently after serial inventory generation and exact union verification. Await every packager and propagate any archive, ELF-reference or checksum failure. Remove cross-runner raw NAR upload/download work from the ordinary test path; keep compact diagnostics and all eight worker inventory comparisons.
- Apply the already measured workspace-only mold and opt-level-0 settings to default-feature XMPP server tests. Keep the existing dependency artifact, exact feature set, selected tests and fixture hooks. Lower only the ephemeral PostgreSQL fixture's deadlock detection interval from its one-second default to 50 ms. The 200-round race, transaction semantics, lock/statement timeouts and assertions stay unchanged. Baseline logs showed 57 one-second deadlocks immediately preceding a 57.197-second test.
- Trial eight CPUs with 32 GB RAM for Rust CodeQL, preserving its eight threads and all queries. Expand CodeQL PR events to include stacked PR base branches. More memory is an unproven hypothesis and may cause resource queueing; measure total workflow time before keeping it.

Source narrowing and custom duration-based partitioning are deferred. The former needs feature/fixture parity work; the latter offers only about 29 seconds under ideal balance before other costs. The Rust change in this trial is an explanatory stress-test comment, which invalidates source-derived checks without changing test behavior. This is a controlled source-invalidation trial, not a representative production-code edit.

Local validation: 94 helper tests (including the three native Nix transfer tests), native parent/child CPU-affinity inheritance, four concurrently generated/relocated fixture archives, and injected archive/ELF/checksum failures. Both independent operational/security and coverage reviews must be clean before the trial is pushed. The installed cuenv 0.55 evaluator can exceed its local ten-second timeout; the previously reviewed scratch-only generator with a longer timeout is used for generation parity, and live CI still runs the original pinned binary.

Live timings, complete test identity parity and scanner results are pending. No under-ten-minute result is claimed yet.

## First trial result and runner correction

Commit `1419e418` failed before any Rust shard ran: Namespace's default runner container could not create the kernel namespaces required by `sandbox=true` with `sandbox-fallback=false`. The required isolation guard was retained. Compilation succeeded in 183.61 seconds; all four archives packaged in 25 seconds; OOM and OOM-kill counts were zero. This is not a successful CI timing or coverage result. The post-job FlakeHub drain took about 57 seconds after the early failure.

The first trial also exposed PR base-branch filtering: only seven workflows started while the PR was stacked. PR #1809 now targets main so the existing XMPP, root-sync and code-quality workflows participate. The next push performs another controlled Rust source invalidation through a comment-only change.

Namespace [documents privileged runner containers for Nix sandboxing](https://namespace.so/docs/solutions/github-actions/runner-controls/privileged-workflows). The builder now opts into `container.privileged` using its documented `-with-features` runner label. Host PID sharing is not enabled. A small local, non-substitutable Nix derivation checks required sandbox support before compilation; all shard builds continue to require sandboxes and disable fallback and remote builders. The four eight-CPU affinity masks and per-shard PostgreSQL instances remain unchanged.

The eight-CPU/32-GB CodeQL trial queued for 194 seconds, compared with about ten seconds on the preceding eight-CPU/16-GB baseline. The scheduling cause is unknown; its start preceded Android PR completion, so that job's resource release does not explain the delay. The next trial restores the original eight-CPU/16-GB runner and 14,336-MB analysis budget. CodeQL still covers every PR base branch and retains all queries.

The deadlock stress test is enabled by clustering and is absent from the default-feature XMPP lane, so its faster detection must not be counted as a default-feature speedup. That lane's qualification depends on its measured mold/profile improvement and full 4,119-test identity parity.
