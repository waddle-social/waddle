# GitHub merge queue

`env.cue` declares the native queue parameters for Waddle's active `main` ruleset. GitHub waits up to 30 minutes for 100 eligible entries, then merges the available entries. A full group can merge early. Build concurrency is one. GitHub dispatches `merge_group` checks independently of this batching window. See [Managing a merge queue](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/configuring-pull-request-merges/managing-a-merge-queue#managing-a-merge-queue).

From the repository root, use a GitHub CLI session with repository Administration write access. GitHub only exposes bypass actors to accounts that can administer this ruleset.

```sh
cuenv task -p infrastructure/github plan
WADDLE_QUEUE_PLAN='paste the fingerprint printed by plan' cuenv task -p infrastructure/github apply
cuenv task -p infrastructure/github check
```

`plan` shows each changed parameter. The fingerprint covers the desired policy and the live ruleset, including its revision. `apply` reads the ruleset again, rejects a stale plan, and submits the current writable fields with the queue parameters replaced. It performs a fresh read to verify every writable field. GitHub exposes a ruleset-wide update with no documented atomic version precondition. Serialize administrator edits during the apply and inspect the ruleset history if verification fails. `check` exits nonzero on drift. To roll back, review and apply a plan against the previous CUE policy.

Run the offline reconciler tests with `python3 -B -m unittest discover -s infrastructure/github`.
