# Merge candidate verification

`waddle-integration` runs on every pull request, merge-group `checks_requested`
event, and explicit workflow dispatch. **Its same-repository `integration`
aggregate is candidate-controlled and is not eligible as the required trusted
gate.** A candidate can edit the workflow, generator, adapter, tests or task
definitions and manufacture success. Action SHA pins, read-only permissions,
report validation and green checks do not establish an independent trust boundary.
This draft prepares verification machinery; the trusted external/organization
boundary and protected task contract remain open design blockers. No repository
rules, release pins or runner settings are changed by this addition.

## Coverage and candidate identity

The generator discovers every checked-in workflow with a `pull_request` trigger,
including native experiments. It copies its jobs, commands, runner sizes,
dependencies, and internal failure checks. Generated verification jobs are capped
at `contents: read` (or no permissions for a pure result check). They omit
FlakeHub cache setup, which requires cloud OIDC, retain Hestia, remove explicit
run-step GitHub tokens, and disable persisted checkout credentials. Original
workflow permissions and behavior are unchanged. It namespaces job and
artifact names, explicitly checks out `github.sha`, and selects lanes from their
original path filters. Branch filters are conservatively ignored. Unknown glob
syntax selects coverage rather than excluding it. Renames are diffed as deletion
and addition so both old and new paths participate; filenames stay NUL separated.

Every generated `uses:` reference must identify a full 40-character Git commit.
The generator translates only these reviewed mutable source aliases, resolved
with `git ls-remote` against their official upstream repositories on 2026-10-30:

| Source alias | Fixed generated reference |
| --- | --- |
| `actions/checkout@v4` | [`11d5960a326750d5838078e36cf38b85af677262`](https://github.com/actions/checkout/commit/11d5960a326750d5838078e36cf38b85af677262) |
| `actions/setup-python@v5` | [`a26af69be951a213d495a4c3e4e4022e16d87065`](https://github.com/actions/setup-python/commit/a26af69be951a213d495a4c3e4e4022e16d87065) |

Existing full SHAs are preserved, including the separate utility checkout pin.
Unknown tags/branches, shortened SHAs, expressions, local actions and container
references fail generation. Full-graph tests verify every generated reference.
These pins freeze the referenced action commits; they do not make a
candidate-controlled workflow or its task definitions authoritative.

The planner compares the event's full base SHA with the exact candidate SHA.
Missing objects, invalid event identity, non-ancestor bases, and empty diffs fail.
An unrecognized surface selects every lane. Planner or generator changes also
select every lane. The aggregate requires planner success, every applicable job
success, and every nonapplicable job skipped. Missing, cancelled, failed, or
unexpectedly skipped applicable jobs fail. There is no cross-workflow polling or
reuse of prior PR results.

Thin source workflows retain the original `cuenv ci` executor, using its supported
`--all` selection policy to execute every declared task of each selected thin
lane with the original runtime, environment, inputs and dependency behavior.
There is no provider/event override and no switch to the different `cuenv task`
sandbox implementation. The adapter supports only the reviewed non-publishing
`pullRequest` pipelines and Android `deviceTests`. It uses explicit-path `cuenv info --json [--meta]` to
require one exact project, a nonempty executable task inventory, and consistent
named bindings. Environment/continue-on-error pipeline options and unsupported
task forms fail. Upstream `--all` independently rejects inline tasks, including
an inline task that imitates a named binding's JSON output references.

Before execution, the fixed report filename must be absent and every existing
parent must be a real directory. After a successful CLI exit, the adapter requires
a fresh non-symlink regular JSON report of at most 4 MiB, rejects duplicate JSON
keys, and checks its version, absolute project spelling, pipeline, provider,
unchanged event/ref/base/SHA, task multiset, successful task statuses and integer
zero exit codes. File modification and start/completion timestamps must lie
within this invocation's wall-clock window. Missing, stale, future, malformed,
duplicate-task, skipped, failed or wrong-identity reports fail. HEAD and the
resolved project definition digest are rechecked after execution. Dependencies
are executed by the original CI runner; its report lists pipeline entry tasks,
so real dependency ordering and failure propagation are covered by CLI fixtures.
Reports establish completion under trusted task definitions; they are not
cryptographic evidence against arbitrary candidate code forging its own files.

**Release blocker:** the repository still downloads cuenv 0.57.3, which lacks
`ci --all`. No nonexistent release or schema version is pinned here. The adapter
and its CLI regressions fail with that old binary. Downstream behavior is tested
only with the local binary built from reviewed
[cuenv #559](https://github.com/cuenv/cuenv/pull/559), exact commit
`65538df73b5b56dde0b7fb3f6d555e978efd6d97`; its version label is 0.63.0 and does
not mean that the published 0.63.0 release contains the patch. Keep this PR draft.
After upstream review/merge and an actual supported release, explicitly set
`ci.cuenv.version` to that existing release in the source configuration, regenerate
setup/source/integration workflows, test compatibility with the separately pinned
0.57.3 CUE schema, and repeat actual hosted lane execution and exact-head review.
That resolves only the CLI/hosted-execution blocker; it does not make the
candidate-controlled aggregate eligible as a trusted required gate. Do not publish or merge upstream as part
of this draft's local validation.

Each runner rechecks exact HEAD, base commit availability, ancestry and complete
NUL-safe diff success. Deleted, empty, malformed, stale or empty-pipeline
configuration fails project discovery. All seven current thin task/dependency
closures were inspected: they contain build, test, lint, SDK/bindings and token
checks, with no publication/deployment tasks; production pipelines are excluded.
Broader execution adds work, and source task/runtime changes still require
independent review. No product or XMPP behavior is changed.
A dispatch uses the selected workflow ref as its exact
candidate and requires an explicit ancestor `base_sha`; it does not certify a
later merge queue candidate.

Regenerate normal cuenv workflows with their deliberately selected CLI release
(the CUE schema dependency is a separate pin), then run:

```sh
bun scripts/integration-ci.ts sync
bun scripts/integration-ci.ts --check
bun test scripts/integration-ci.test.ts
python3 scripts/test_integration_ci_project.py
# Requires the reviewed --all CLI in PATH; every selected thin job runs this too.
WADDLE_TEST_ALL_CUENV=1 python3 scripts/test_integration_ci_project.py
```

The local inline/false fixtures scrub all `GITHUB_*` and `WADDLE_CI_*` variables,
Buildkite detection and GitHub token environment from every adapter subprocess.
A poisoned-ambient regression checks this per invocation. The separate real-event
fixture explicitly supplies its own merge-group identity. This test isolation
does not override the real workflow execution context.

The existing server `checkCiDrift` and the integration planner check graph parity.
Adding or changing a source PR lane without regenerating fails those checks.
Original PR workflows remain active with their existing privileges, so applicable PR jobs temporarily execute
in both workflows. This increases demand and deliberately preserves coverage;
runner provisioning must be repaired before any separately trusted gate is enabled. Omitting
FlakeHub cache can increase compile and download time; no verification commands,
runner resources, or assertions are removed. The original cuenv reporter only uses
check/PR write permissions for optional notifications; notification failures are
warnings in `crates/ci/src/executor/reporting.rs` of cuenv v0.57.3. Generated
integration jobs rely on Actions job results rather than those notifications.

## Open trusted-gate boundary

At inspection on 2026-10-30, repository ruleset `6335231` contained a merge queue
but no `required_status_checks`; classic branch protection returned HTTP 404.
The current user was an always-bypass actor. The queue alone is not a CI gate.

The adjacent `integration-ci-ruleset-proposal.json` is a historical illustrative
status-rule shape, **not an eligible or approved configuration to apply**: it
requires the candidate-controlled `integration` context from shared Actions app
15368. That binding does not identify trusted verifier code. Do not activate it
as the required trusted gate. Main policy is unchanged.

Two architecture directions remain open: a required organization workflow whose
verifier comes from a separately protected source, or an external verifier with
independent authority and a separately protected implementation. Neither boundary
is implemented here. Either design must bind evidence to the exact candidate,
use a protected complete task contract, handle candidate attempts to alter or omit
verification, and prevent other candidate jobs from forging the required result.
The protected contract must cover surfaces/path selection, projects, pipelines,
named tasks, inputs/assertions, dependency closures, hooks, runtime/environment
behavior and indirect executables. Candidate changes to that contract require a
separate reviewed transition; reading a candidate's own task inventory is not
proof that it preserved the required work. CI-owner review is useful but does not
replace the missing execution/authority boundary.

Original workflows still have checks-write tokens and OIDC access. Their ability
to forge a same-name context and their external trust bindings remain unaudited.
The new generated jobs retain read-only repository permissions, but that alone
does not isolate required-check authority from the original jobs. No organization
settings, repositories, apps, production release pins or main rules are created
or modified by this draft.

Any eventual activation needs independent architecture review, protected-source
and task-contract enforcement, audited credential authority, real current-head
execution and controlled negative/positive queue proofs on the actual policy.
Local contracts and isolated canary behavior do not establish main enforcement.

## Runner availability

At inspection, queued `namespace-profile-linux-x86` jobs had no assigned runner
(`runner_id: 0`, empty runner name and steps). The only two registered repository
runners were busy and advertised the explicit
`nscloud-ubuntu-24.04-amd64-32x64-with-features` label with privileged-container
and priority features. That proves an assignment/capacity bottleneck; repository
runner inventory alone cannot distinguish Namespace profile configuration,
organization quota, scheduler policy, or provisioning failure.

Repair proposal: in Namespace, verify the `linux-x86` profile exists and is
attached to this repository, inspect its provisioning failures and concurrency
quota, then restore available matching capacity. Retain each lane's existing
CPU/memory, architecture, privileged-container and Android KVM/cache requirements.
Verify a formerly queued `namespace-profile-linux-x86` job is assigned and runs
its actual task. Measure peak queue delay with both original and integration PR
workloads. Do not relabel the 32-core Rust builder onto a smaller runner, cancel
coverage, or claim queued jobs are successful. Runner labels in this PR stay
unchanged.

PR #2064's `nixClippy` job failed before its task because the pinned Hestia v2.0.0
release download reported `TypeError: fetch failed`. Retrying only the failed
jobs after workflow completion produced a successful second attempt of run
`41077599469` at head `4bf9233ca4439aec3a598dbf6973e6bebbbb4974`, including the
actual Clippy task. This resolved that transient download failure, not runner
availability or merge enforcement.

GitHub documents the separate merge-group trigger and skipped-job behavior in
[events that trigger workflows](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows#merge_group)
and [troubleshooting required checks](https://docs.github.com/en/pull-requests/how-tos/merge-and-close-pull-requests/troubleshooting-required-status-checks).

## Bootstrap evidence and remaining work

There is no executable main-activation sequence approved for this draft. Keep
federation merges blocked while the trusted boundary and protected task contract
are unresolved. A same-repository context requirement must not be substituted
for that work. A future bootstrap must establish and read back the independently
trusted rule before admitting candidates, retain an effective exclusive freeze
through the transition, and prove candidate failure/missing evidence cannot
advance main. The previously proposed no-bypass update freeze still needs queue
behavior proof; administrators' ability to edit rules also needs coordination.

Controlled canary evidence on 2026-10-30:
[#2072](https://github.com/waddle-social/waddle/pull/2072) used ruleset 26220154,
required Actions app 15368 context `integration-canary-2047`, and no bypass.
GitHub accepted the required context before the target contained its workflow and
ran candidate-only merge-group run 41089409061 at `d350fd6`. The failing candidate
was dequeued without changing base `d12e90e`; passing candidate `b4133ce` then
merged. This proves platform discovery/status enforcement for that isolated
canary, not independent trust in candidate-controlled code or the main policy.
[#2073](https://github.com/waddle-social/waddle/pull/2073) proved that a no-bypass
update restriction rejects direct pushes; queue behavior while frozen remained
inconclusive because an optional review was pending. Temporary rulesets and
remote canary branches were removed. No actual main enforcement is claimed.

Once a trusted design is independently reviewed and implemented, repeat the
actual exact-candidate workflow and negative/positive enforcement checks against
its final policy, including advancing base, cancellation, missing evidence and a
source-lane/task-contract change. Never remove a required gate or use a bypass
to get bootstrap through. Relevant platform references are GitHub's
[merge-group event](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows#merge_group),
[required-check troubleshooting](https://docs.github.com/en/pull-requests/how-tos/merge-and-close-pull-requests/troubleshooting-required-status-checks),
and [update restrictions](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/available-rules-for-rulesets#restrict-updates).
