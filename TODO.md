# TODO.md

## Test runtime snapshot (2026-02-13)

- `cargo test -p waddle-xmpp --lib`
  - Build + run (latest): **~7.9s** total (`7.77s` build + `0.15s` test execution)
- `just quick-caas`
  - Latest duration: **46.81s**
- XEP-0030-only compliance
  - Command: `... --enabled-specs XEP-0030 ...`
  - Latest duration: **93.51s**
- Full scoped compliance (RFC6120,RFC6121,XEP-0030)
  - Command: `... --enabled-specs RFC6120,RFC6121,XEP-0030 ...`
  - Latest duration: **11,267.63s** (**~3h 07m 48s**)
- Fast sharded gate (timeout 2000ms, 3 shards)
  - `core-nonheavy` (`--disabled-tests=<6 heavy suites>`): **54.66s**
  - `heavy-b` (`--enabled-tests=<3 suites>`): **782.91s** (~13m 03s)
  - `heavy-a` (`--enabled-tests=<3 suites>`): **1500.09s** (~25m 00s)
  - Expected wall-clock in CI (parallel shards): **~25–30m**

Artifacts:
- `test-logs/quick-caas/summary.json`
- `test-logs/rka-dev-xep0030-check-1/summary.json`
- `test-logs/rka-dev-full-final-6/summary.json`
- `test-logs/todo-shard-core-nonheavy/summary.json`
- `test-logs/todo-shard-heavy-a/summary.json`
- `test-logs/todo-shard-heavy-b/summary.json`

---

## Plan: reduce full compliance from ~3h to <=30m

### Baseline hotspot analysis (from `test-results.xml`)

~99% of total test time is concentrated in six RFC6121 matrix suites:

- `RFC6121Section8_5_2_1_1_MessageIntegrationTest` → **2800s**
- `RFC6121Section8_5_3_2_3_IqIntegrationTest` → **2240s**
- `RFC6121Section8_5_2_1_3_IqIntegrationTest` → **2240s**
- `RFC6121Section8_5_3_2_2_PresenceIntegrationTest` → **1400s**
- `RFC6121Section8_5_3_2_1_MessageIntegrationTest` → **1240s**
- `RFC6121Section8_5_2_1_2_PresenceIntegrationTest` → **1120s**

Total from these six: **11,040s** (~3h 4m) out of ~11,110s testcase time.

### Step 1 (fastest lever): lower reply timeout

Current full runs use `--timeout-ms 10000` (`sinttest.replyTimeout=10000`).

- [ ] Calibrate at `--timeout-ms 3000`
- [ ] Calibrate at `--timeout-ms 2000`
- [ ] Calibrate at `--timeout-ms 1500`
- [ ] Track pass/fail flake rate per setting across 3 consecutive runs

Progress note: `timeout-ms=2000` is now validated for all three CI shards (no failures across the latest shard runs).

Rule of thumb from current timing: runtime scales roughly with timeout.
Estimated total runtime:
- 3000ms → ~56m
- 2000ms → ~38m
- 1500ms → ~30m

Target command for calibration:

```bash
WADDLE_COMPLIANCE_CONTAINER_TIMEOUT_SECS=0 cargo run --bin waddle -- compliance \
  --profile best_effort_full \
  --timeout-ms 1500 \
  --enabled-specs RFC6120,RFC6121,XEP-0030 \
  --artifact-dir ./test-logs/rka-dev-full-fast-<n>
```

### Step 2: shard heavy suites across parallel jobs

- [x] Add CLI/harness support for Smack `enabledTests` / `disabledTests` passthrough (`-Dsinttest.enabledTests=...`).
- [x] Split the six heavy suites into at least 2 shards and run in parallel CI jobs.
- [x] Keep a small non-heavy shard for the rest.

Status (implemented):
- `waddle compliance` now supports `--enabled-tests` / `--disabled-tests`.
- Harness env passthrough: `WADDLE_COMPLIANCE_ENABLED_TESTS` / `WADDLE_COMPLIANCE_DISABLED_TESTS`.
- CI fast matrix now uses 3 shards at `timeout=2000ms`: `core-nonheavy`, `heavy-a`, `heavy-b`.
- Latest shard aggregate: `tests=422`, `failed=0`, `errors=0`, `skipped=8` (matches full scoped inventory).

Measured with 3-way sharding at `timeout-ms=2000`: max shard is ~1500s (~25m), so expected CI wall-clock is ~25–30m.

### Step 3: CI policy split

- [x] PR gate: `just quick-caas` + fast compliance shard(s)
- [x] Nightly gate: full, unsharded reference run (for drift detection)
- [x] Fail PR only on fast-gate regressions; alert on nightly regressions

---

## Failing tests (latest full run)

Latest full run (`test-logs/rka-dev-full-final-6`):
- **`TEST FAILED` count: 0**
- `failed_tests`: `[]`

So there are **no currently failing tests** in the latest completed full run.

---

## TODO for non-green tests (skipped/incomplete coverage)

> Note: Full run has 0 failures but 8 skips (`tests_started=422`, `tests_completed=414`, `skipped=8`).

### 1) Roster versioning behavior (2 skipped)
- [ ] Evaluate/decide whether to implement per-item roster pushes for ver-requests (instead of returning full roster snapshot).
  - `RFC6121Section2_6a_VerStreamFeatureIntegrationTest.testRosterInterimPushesAreCondensed`
  - `RFC6121Section2_6a_VerStreamFeatureIntegrationTest.testRosterPushOrder`
- [ ] If current behavior is intentional, document rationale and align compliance gate expectations.

### 2) "Without initial presence" test setup (6 skipped)
- [ ] Adjust CAAS/test profile so test resources can stay connected **without auto-sending initial presence**.
- [ ] Re-run skipped tests once harness config supports that scenario:
  - `RFC6121Section2_3_AddIntegrationTest.testRosterSetGeneratesPushToInterestedResourceSelfWithoutInitialPresence`
  - `RFC6121Section2_3_AddIntegrationTest.testRosterSetGeneratesPushToInterestedResourceOtherResourceWithoutInitialPresence`
  - `RFC6121Section2_4_UpdateIntegrationTest.testRosterUpdateGeneratesPushToInterestedResourceSelfWithoutInitialPresence`
  - `RFC6121Section2_4_UpdateIntegrationTest.testRosterUpdateGeneratesPushToInterestedResourceOtherResourceWithoutInitialPresence`
  - `RFC6121Section2_5_DeleteIntegrationTest.testRosterDeleteGeneratesPushToInterestedResourceSelfWithoutInitialPresence`
  - `RFC6121Section2_5_DeleteIntegrationTest.testRosterDeleteGeneratesPushToInterestedResourceOtherResourceWithoutInitialPresence`

### 3) Compliance gating
- [x] Decide CI acceptance rule for report-only mode:
  - Selected Option A: `tests_failed == 0` and completed testcases are 100% passing (`tests_passed == tests_completed`), with `tests_completed > 0`.
  - Option B (`skipped == 0`) remains too strict for current harness behavior.
- [x] Encode final rule in CI and documentation.
  - Harness `compliance_failed` logic now applies Option A semantics in report-only profiles.
  - CI summaries continue reporting skipped counts for visibility.
