# cuenv 0.55 CI prerequisite plan

This temporary, plan-only bootstrap document exists solely to open the required
draft pull request before implementation. It will be removed before finalizing
the packet.

## Plan

1. Use only the official cuenv `0.55.0` GitHub release binary.
2. Update the root dependency and CI configuration from `0.54.0` to `0.55.0`.
3. Regenerate the lockfile and all six cuenv-owned workflows, retaining the
   server VCS materialization guard and any still-generated Android drift check.
4. Prove repeated generation is stable, check generated workflow content, and
   run feasible scoped validation under the disk gate.

No XMPP protocol behavior or wire format is in scope.
