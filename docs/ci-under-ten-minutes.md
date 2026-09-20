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
