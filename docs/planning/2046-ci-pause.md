# Issue #2046 CI pause (closed)

Status: closed. Hosted CI is running on `main` again.

## What was paused

The owner paused hosted CI on 2026-11-01 while completing #2046. The source
baseline for the pause is `9242346ee2e070873ed9a1015115ecbc2bc0553b`; commit
`343ad2a7f` removed its 34 tracked GitHub workflow files from `main`. The
repository Actions setting and the 40 registered YAML workflows were disabled,
and the `copilot_code_review` rule was removed from the active `main` ruleset
(ID `6335231`) while its other protections stayed active. The exact API state
and workflow IDs are in
[#2046's CI pause comment](https://github.com/waddle-social/waddle/issues/2046#issuecomment-6561052066).

## Restoration

- #2395 (`f77c03814`, 2026-11-14) restored the workflow sources: the
  cuenv-generated workflows from `cuenv sync -A`, the three workflows owned by
  `scripts/sync-cache-benchmark.sh`, `scripts/integration-ci.ts` and
  `scripts/sync-rust-tests.sh`, and the four maintained by hand
  (`hestia-cache-gc.yml`, `operational-qualification.yml`,
  `waddle-apple-default.yml`, `waddle-apple-pullrequest.yml`).
- The repository Actions setting is enabled, the YAML workflows are active,
  and ruleset `6335231` has its `copilot_code_review` rule again. The
  separately disabled Code Quality ruleset (ID `19468092`) was disabled before
  the pause and still is.

33 of the 34 paused files are on `main`. The missing one,
`.github/workflows/waddle-server-parentmovenativepullrequest.yml`, was removed
on purpose by #2405 (`3b0dccab3`): its `parentMoveNativePullRequest` pipeline
repeated the pull-request image, clippy and fmt builds to run a proof that
gates nothing. The `parentMoveNative` pipeline
(`waddle-server-parentmovenative.yml`) runs the same proof and build closure
nightly and on dispatch; see
[CI performance](../ci-performance.md#main-publication-latency-and-runner-contention).

## Checking the inventory

To compare the paused baseline with the current tree:

```sh
diff <(git ls-tree --name-only 9242346ee2e070873ed9a1015115ecbc2bc0553b .github/workflows/ | sort) \
     <(git ls-tree --name-only HEAD .github/workflows/ | sort)
```

The only expected difference is the `parentmovenativepullrequest` file above;
any other missing or extra file is a regression or needs its own record here.
`bash server/scripts/check-ci-helper-contracts.sh` runs
`server/scripts/test_default_publish_graph.py` against the generated
`waddle-server-default.yml`, so it fails if that workflow is missing.
