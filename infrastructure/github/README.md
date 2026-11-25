# GitHub merge queue

`env.cue` declares the native queue parameters for Waddle's active `main` ruleset. GitHub waits up to 30 minutes for 100 eligible entries, then merges the available entries. A full group can merge early. Build concurrency is one. GitHub dispatches `merge_group` checks independently of this batching window. See [Managing a merge queue](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/configuring-pull-request-merges/managing-a-merge-queue#managing-a-merge-queue).

Applying this policy also requires the `PR smoke` job from GitHub Actions (App ID `15368`). One workflow emits that context for every pull request and merge group. The ordinary smoke check exercises candidate-controlled code. The integration workflow reports its deeper combined-revision checks separately.

Before applying the policy, merge the smoke workflow to `main` and confirm a successful `PR smoke` check from GitHub Actions on each PR waiting to enter the queue. Update older PR branches to include the workflow, let their checks finish, then enqueue them. The reconciler creates the status-check rule with strict up-to-date checks disabled so unrelated merges do not force another PR build. Existing status-check settings and unrelated requirements remain part of the ruleset.

From the repository root, use a GitHub CLI session with repository Administration write access. GitHub only exposes bypass actors to accounts that can administer this ruleset.

```sh
cuenv task -p infrastructure/github plan
WADDLE_QUEUE_PLAN='paste the fingerprint printed by plan' cuenv task -p infrastructure/github apply
cuenv task -p infrastructure/github check
```

`plan` shows each changed parameter. The fingerprint covers the desired policy and the live ruleset, including its revision. `apply` reads the ruleset again, rejects a stale plan, and submits the current writable fields with the declared queue parameters and smoke requirement. It verifies every writable field on a fresh read, accounting for the order of rules and checks and GitHub’s optional default fields. A conflicting issuer for the smoke context stops reconciliation. GitHub exposes a ruleset-wide update with no documented atomic version precondition. Serialize administrator edits during the apply and inspect the ruleset history if verification fails. `check` exits nonzero on drift. To roll back a queue parameter, review and apply a plan with its previous value. Removing a required check needs an explicit ruleset edit.

Run the offline reconciler tests with `python3 -B -m unittest discover -s infrastructure/github`.
