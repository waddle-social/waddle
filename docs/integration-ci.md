# Merge candidate verification

`waddle-integration` runs on every pull request, merge-group `checks_requested`
event, and explicit workflow dispatch. Its `integration` job is the proposed
required context. No repository rules or runner settings are changed by this
workflow addition. A successful PR check alone does not authorize merging while
the required context is not enforced on the queue candidate.

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

The planner compares the event's full base SHA with the exact candidate SHA.
Missing objects, invalid event identity, non-ancestor bases, and empty diffs fail.
An unrecognized surface selects every lane. Planner or generator changes also
select every lane. The aggregate requires planner success, every applicable job
success, and every nonapplicable job skipped. Missing, cancelled, failed, or
unexpectedly skipped applicable jobs fail. There is no cross-workflow polling or
reuse of prior PR results.

Thin source workflows invoke `cuenv ci`, whose pinned affected-task selection can
silently skip unresolved inline tasks or failed diffs. Integration replaces that
nested planner with explicit named execution: it discovers the exact project
using explicit-path `cuenv info --json`, reads its requested pipeline with
`--json --meta`, and requires a nonempty task inventory. Each executable must
exactly match a named binding in the project's task index. The runner invokes
`cuenv task --path PROJECT NAME` for every attested pipeline task, with normal
runtime and dependency execution, and fails on any nonzero exit. This includes
all tasks of every selected thin lane, even when its own affectedness would
have skipped them. Unsupported unnamed tasks, matrix task forms and pipeline
execution options fail instead of being silently approximated.

Named JSON equality alone does not prove cuenv's original source reference map:
an inline task can imitate output names. Direct named execution avoids relying
on that map. The pinned-cuenv regression reproduces both an unnamed inline false
and an inline false equal to a named binding; the first is rejected and the
second executes the named false task and fails. Every thin runner runs that
regression before executing its project.

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

Regenerate normal cuenv workflows first with the release in `cue.mod/module.cue`,
then run:

```sh
bun scripts/integration-ci.ts sync
bun scripts/integration-ci.ts --check
bun test scripts/integration-ci.test.ts
python3 scripts/test_integration_ci_project.py
# Requires cuenv 0.57.3 in PATH; every selected thin job runs this too.
WADDLE_TEST_PINNED_CUENV=1 python3 scripts/test_integration_ci_project.py
```

The existing server `checkCiDrift` and the integration planner check graph parity.
Adding or changing a source PR lane without regenerating fails those checks.
Original PR workflows remain active with their existing privileges, so applicable PR jobs temporarily execute
in both workflows. This increases demand and deliberately preserves coverage;
runner provisioning must be repaired before enabling the required gate. Omitting
FlakeHub cache can increase compile and download time; no verification commands,
runner resources, or assertions are removed. The pinned cuenv reporter only uses
check/PR write permissions for optional notifications; notification failures are
warnings in `crates/ci/src/executor/reporting.rs` of cuenv v0.57.3. Generated
integration jobs rely on Actions job results rather than those notifications.

## Reviewed settings proposal and activation evidence

At inspection on 2026-10-30, repository ruleset `6335231` contained a merge queue
but no `required_status_checks`; classic branch protection returned HTTP 404.
The current user was an always-bypass actor. The queue alone is not a CI gate.

The adjacent `integration-ci-ruleset-proposal.json` is an additive rule proposal,
not a complete ruleset replacement. After reviewing the full current ruleset,
append its `required_status_checks` rule while preserving deletion, linear
history, non-fast-forward and merge-queue protections. The context is
`integration` from the GitHub Actions app (integration ID `15368`). Preserve the
queue; set its check-response timeout only after measuring actual capacity.

A required context protects against accidental failed/missing verification; it
is not tamper resistant against changes to this repository's own workflow,
generator, filters or tests. For that threat model, enforce an organization
required workflow from a separately protected source, or require independent CI
owner review for all gate, source workflow, env.cue, pipeline and task
definition changes and remove routine bypass
actors. A same-repository context rule alone must never be represented as that
stronger assurance. Original workflows still have existing checks-write tokens
and OIDC access; their ability to forge a same-name context and their external
OIDC trust bindings have not been audited. That existing authority is a separate
activation blocker, even with the new integration jobs restricted to read-only
repository access. Requiring the context alone does not resolve it. This PR does
not silently add reviewers or remove bypasses.

Activation must be separately reviewed. Keep the federation merge freeze during
bootstrap; do not merge this PR through the currently unenforced queue. Before
claiming enforced verification, inspect the activated ruleset, observe a real
merge-group run at its exact head SHA, deliberately fail an applicable lane and
prove the queue refuses it, then prove a clean candidate passes. Also exercise
an advancing base, cancellation, empty diff and a source-lane addition. Local
contract tests establish implementation behavior but not GitHub enforcement.

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

## Bootstrap without an unenforced interval

This is a proposed sequence requiring separate owner review; nothing here has
been applied. The current ruleset has no required status context, so an
operator's promise to wait is not an enforced freeze.

1. Create a separate active main-only bootstrap ruleset with no bypass actors
   and an `update` restriction. Proposed shape:

   ```json
   {
     "name": "integration-ci-bootstrap-freeze",
     "target": "branch",
     "enforcement": "active",
     "bypass_actors": [],
     "conditions": {"ref_name": {"include": ["refs/heads/main"], "exclude": []}},
     "rules": [{"type": "update"}]
   }
   ```

   First validate that restriction and ordinary-actor denial on a controlled
   canary branch/repository, including merge-queue updates. Read back the actual
   main rule after activation, preserve the existing rules, and stop federation
   queue admissions. Do not claim a freeze until its effective enforcement and
   bypass configuration are verified. Repository administrators can still edit
   rules; coordinate a single reviewed settings operator during bootstrap.

2. Repair capacity, complete exact-head PR CI and independent source review, and
   resolve the original workflow write-token/OIDC and definition-review policy.
   Keep main frozen. The required `integration` context must have a real recent
   successful run in this repository from Actions app 15368. GitHub documents a
   successful-check lookback of seven days, not a requirement that the workflow
   already be on main. Do not seed eligibility with a fabricated status or an
   unrelated dispatch check.

3. In a controlled repository with the same queue/rules capabilities, test both
   platform assumptions before depending on them here: add the required context
   while its new workflow exists only in a PR, then queue that candidate. Prove
   an intentionally failing applicable job produces a failing check on the
   exact `merge_group.head_sha` and blocks the queue even when its PR check was
   green. Then prove a clean candidate containing the newly introduced workflow
   runs and can pass. Record the event, workflow revision, SHA, app identity,
   effective rules and unchanged base on the failing case.

   The official merge-group event reference specifies the group SHA and
   separate trigger. Unlike several other events, its section does not state
   a default-branch-file requirement. That omission does not prove whether a
   workflow introduced only by the queued candidate will execute. Requiring a
   context before that workflow reaches main and candidate-only workflow
   discovery are both unproven for this repository until controlled live tests
   and API readback establish them. A rejected rule update is a blocker, not
   permission to omit the requirement.

4. With the freeze still active, add the reviewed required-status rule, confirm
   its effective configuration and the credential/review protections, and
   admit only the reviewed bootstrap PR. Remove the temporary update freeze
   only after the required gate is active and the controlled negative test has
   passed; never remove the required gate to get the bootstrap PR through.
   Observe its real merge-group run at the actual combined candidate SHA.
   Missing workflow discovery must leave the check pending and block the queue.
   Complete a controlled negative/positive test on the actual main policy before
   reopening normal admissions; hold federation PRs until that proof is recorded.

5. If candidate-only workflow discovery or pre-main context enforcement cannot
   be proved, retain the freeze. Use a separately protected organization
   required workflow or independently trusted CI integration that can verify
   the bootstrap candidate under an enforced rule, subject to account feature
   availability and separate review. Do not assume that capability exists, and
   do not substitute a one-off bypass, unchecked merge, or removed requirement.

The temporary freeze proposal is intentionally separate from the additive
required-status JSON so preserving or removing one cannot accidentally remove
the other. Relevant platform references are GitHub's
[merge-group event](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows#merge_group),
[required-check troubleshooting](https://docs.github.com/en/pull-requests/how-tos/merge-and-close-pull-requests/troubleshooting-required-status-checks),
and [update restrictions](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/available-rules-for-rulesets#restrict-updates).
