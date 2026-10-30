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
An unrecognized surface selects every lane. Thin pipelines retain their existing
affected-task semantics from the complete base diff: selecting all lanes does
not unconditionally execute every task in each lane. Planner or generator changes also
select every lane. The aggregate requires planner success, every applicable job
success, and every nonapplicable job skipped. Missing, cancelled, failed, or
unexpectedly skipped applicable jobs fail. There is no cross-workflow polling or
reuse of prior PR results. Thin `cuenv ci` steps explicitly use the local provider
and the validated base SHA with full Git history: cuenv 0.57.3 otherwise ignores
`--from` under GitHub provider detection and falls back to the last commit on
merge groups. Each thin step rechecks its own checkout HEAD, base commit availability,
base ancestry, and successful full diff before invoking cuenv: the pinned local
provider otherwise silently accepts Git diff failures as zero affected tasks.
Each runner also requires `cuenv info PATH --json` to discover exactly the
requested project, then checks the exact instance from `--json --meta` has the
requested pipeline with nonempty executable tasks. Every task must carry a
consistent named output reference and exactly match that binding in the project
task index; unsupported inline tasks are rejected because pinned cuenv can
silently treat them as unaffected. Deleted, empty, malformed or
stale project/pipeline definitions fail before cuenv can report no-op success.
The info commands evaluate one explicit path, avoiding recursive partial
discovery. Their Git diff disables rename collapsing. The outer planner
rejects encoded/non-ASCII filenames when a thin cuenv lane is selected because
that pinned local provider parses newline-delimited Git paths; it fails visibly
rather than silently omitting affected tasks. Other lanes retain NUL-safe paths.
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
