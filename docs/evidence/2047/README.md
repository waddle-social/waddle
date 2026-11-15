# Evidence for issue 2047

This folder holds the evidence for the verification phase of the
[federation plan](../../planning/federation-2047.md#verification-phase).
Hosted CI results are recorded separately; what is kept here is recorded by
hand, from runs on `integration/2047`.

Nothing in this folder claims that a test passed unless it cites the run log
that shows it. A target with no cited run is "not run", and a prerequisite
that was unavailable is recorded as unverified, never as passed.

| File | Contents |
| --- | --- |
| [`unrun-tests.md`](unrun-tests.md) | The verification checklist: every test target added or changed on the branch, its owning slice, its prerequisites and its execution status, followed by the hardening and follow-up items the slices left open. |

The evidence log for the gates in the plan's
[gate table](../../planning/federation-2047.md#gate-ownership) is added here
as the verification steps are run.
