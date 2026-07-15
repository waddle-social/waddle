# Delivery status

Last verified: 2026-07-15 against `origin/main` at `67980ca5`.

This document records the completion state of the Lane J adversarial follow-up
(epic #1279) and the shared observability/hygiene work discussed with it. It is
not a replacement for `TODO-ISSUES.md`; that file remains the ordered backlog
and must be updated and merged after each completed issue.

## Executive summary

- The original Lane J (#1243–#1268) is complete.
- Nine of the twelve Lane J follow-up issues are genuinely complete.
- The three remaining product issues are #1289, #1283, and #1285.
- #1289 is incorrectly closed on GitHub even though its durable journal and
  production coordinator are not implemented. It must be reopened or correctly
  rescoped before the epic can close.
- Shared completion work remains for #1136, #1163, and the unfinished portions
  of #1174.
- No open implementation PR described below is currently ready to merge.
- Every code PR remains gated on a clean internal adversarial review, exact-head
  Qodo and Greptile approval, zero unresolved review threads, and fully green CI.

By issue count, the Lane J follow-up is 75% complete. The remaining issues are
the largest state-machine and recovery changes, so the remaining engineering
effort is materially greater than 25%.

## Completed Lane J follow-up issues

| Issue | Result |
| --- | --- |
| #1280 | XEP-0198 sender responsibility preserved after handler timeout; PR #1292 merged. |
| #1281 | MUC-PM MAM history isolated by full occupant JID; PR #1293 merged. |
| #1282 | Untrusted MUC delay timestamps stripped; existing implementation and dedicated regression verified. |
| #1284 | Dead-node RoomActor claims reconciled; completed by PRs #1332–#1335, #1340, and #1344. |
| #1286 | XEP-0085 chat states advertised; PR #1307 merged. |
| #1287 | MUC-PM displayed state isolated by full occupant JID; PR #1308 merged. |
| #1288 | Cross-occupant message-ID collision merges prevented; PR #1304 merged. |
| #1290 | XEP-0313 `with=self` intersection semantics implemented; PR #1309 merged. |
| #1291 | Duplicate capability-form `FORM_TYPE` values rejected; PR #1310 merged. |

## Supporting work merged during this delivery

- PR #1305: actor-local mediated-invite grant and rollback protocol.
- PR #1332: exact SM ownership across node incarnations.
- PR #1333: resumable SM ownership published after transport commit.
- PR #1334: exact RoomActor ownership and bounded local reconciliation.
- PR #1335: bounded, fail-closed orphan recovery; closed #1284.
- PR #1340: retained claim cleanup across failed worker restart.
- PR #1343: restored rooms publish only under current ownership.
- PR #1344: room recovery and rejected-join repair fenced against stale actors.
- PR #1354: mechanical MUC-presence test relocation for reviewability.
- PRs #1346 and #1347: removed two obsolete XML/XEP-0184 helper surfaces.
- PRs #1341 and #1345: incremental `TODO-ISSUES.md` progress updates.

## Current implementation frontier: PR #1355

PR #1355 is the exact MUC actor-incarnation fence prerequisite for the durable
creator and storage work.

The full local review experiment is preserved at commit `eeda0e9d` and on
`backup/pr1355-full-destroy-review-20260715`. Its verification was green:

- `cargo fmt --all -- --check`
- checks for both affected crates across all features and targets
- strict Clippy with `-D warnings`
- 2,607 XMPP library tests
- 15 dedicated XEP-0045 tests
- 1,602 default server tests
- 1,928 clustering server tests

PostgreSQL-backed test bodies were not executed locally because
`WADDLE_TEST_POSTGRES_URL` was not configured. Their compilation and the
in-memory/exact-fence coverage passed, but CI remains authoritative for the
PostgreSQL runtime paths.

Green tests did not prove the complete destroy protocol. Three independent
adversarial passes found the following actionable design gaps:

1. Destroy authorization happens before the mailbox-serialized seal. A requester
   can be demoted before deletion and still destroy the room.
2. Dormant and poisoned persistent rooms cannot be destroyed through the owner
   IQ because preliminary authorization requires a live actor.
3. `Destroyed(None)` can acknowledge success while skipping catalog,
   permission-tuple, bookmark, archive-boundary, invite-ledger, and occupant
   notification work.
4. A replacement room incarnation can publish before the previous incarnation's
   destroy notifications are handed off, reordering client-visible room state.
5. A registry reply timeout can be followed by a committed deletion after the
   caller has discarded the only occupant snapshot.
6. Trusted administrative deletion unnecessarily depends on obtaining an
   occupant snapshot that its caller discards.
7. Several comments still describe an obsolete all-or-nothing destroy contract.

The implementation is therefore being split. The narrowed worktree is based on
`065699d3`; its remaining exact-fence guard/test port and destroy-scope removal
are in progress. The final #1355 will keep only:

- immutable typed `(entity, owner, epoch)` authority per actor incarnation;
- exact-fenced durable load, save, and delete calls;
- full room/entity/owner/epoch validation in the store;
- fail-closed joins and mutations when exact authority cannot be proven;
- exact fence installation before reclaimed-room durable reads; and
- regression coverage preventing a second restore from transplanting an actor
  onto a successor claim.

The notification, authorization, cleanup, and MAM-epoch coordinator belongs to
the #1283 stack instead of the exact-fence prerequisite.

The remote #1355 branch still contains its draft scaffold. The narrowed working
tree must be committed, then pass fresh verification and adversarial review
before its exact head is pushed.

## Durable creator and storage foundation

### PR #1352 — durable creator lifecycle

The remote PR is still a draft scaffold. A prior local implementation exists,
but it must be rebuilt on the final #1355 contract instead of pushing the old
combined branch.

Remaining behavior:

- atomic initialize-or-restore;
- typed created-versus-restored outcome;
- durable `AwaitingInitialConfiguration` versus active lifecycle;
- initial Owner persistence in the initialization transaction;
- status 201 only for the initialization winner;
- creator cancellation and failure recovery;
- production join/configuration integration; and
- a dedicated XEP-0045 lifecycle suite.

### PR #1350 — durable state integrity

This draft is stacked on #1352. Its current storage-only diff must be rebased
after #1352 and revalidated.

Remaining behavior:

- parent/child foreign-key and affiliation constraints;
- semantic verification of existing named constraints;
- repeatable-read parent and child loading;
- fail-closed rejection of orphaned, malformed, duplicated, or unknown durable
  state;
- missing-parent write rejection; and
- PostgreSQL schema/concurrency coverage.

## Remaining issue #1289 — mediated-invite atomicity

PR #1305 completed the actor-local protocol but deliberately did not wire it
into production. PR #1343 also explicitly excluded the invite journal and
coordinator. GitHub nevertheless closed #1289 when #1343 merged; that closure
does not match the implementation evidence.

Remaining reviewable slices:

1. Durable invite-operation journal and restart restoration.
2. Operation-identified affiliation/CAS transactions across every writer.
3. Ordinary MUC handler/coordinator integration and recovery.
4. Group-DM permission tuple, bookmark, archive-boundary, and delivery
   compensation using the same operation identity.

#1289 and its TODO entry must remain incomplete until every slice is merged and
the production invitation paths use the durable protocol.

## Remaining issue #1283 — room destruction and MAM epoch

Draft PR #1306 exists, but its local experiment spans approximately 85 files and
more than 10,000 added lines. It is retained as source material and must not be
reviewed or pushed as one monolith.

GitHub issue #1283 must be updated before implementation review: its current
acceptance criteria do not record the mailbox-linearized owner authorization,
dormant-room authorization, durable notification retention, caller-timeout
continuation, or application cleanup requirements found by the adversarial
review. The issue must also record that the fence-dependent slices are blocked
on the final #1355 contract, and the retained experiment must be rebased onto
that merged prerequisite before it is split.

Planned reviewable slices:

1. Typed owner authorization, actor seal, destroy permit, and notification
   barrier.
2. Retryable application cleanup that cannot acknowledge surviving catalog or
   authorization state.
3. MAM room-incarnation epoch storage, tombstones, rotation, and epoch-specific
   purge.
4. Groupchat archive/write/dispatch fencing and same-JID recreation coverage.

Completion invariants:

- authorization is evaluated after the actor mailbox cut-over;
- owners can destroy dormant persistent rooms;
- every acknowledged destroy has delivered or durably retained all required
  occupant effects;
- a new room incarnation cannot publish before the old destroy barrier ends;
- caller timeout cannot orphan a committed destroy's notification or cleanup;
- poison recovery cannot skip application cleanup;
- stale writes/purges cannot target a recreated room; and
- destroyed-incarnation MAM history cannot reappear.

## Remaining issue #1285 — terminal shutdown status 332

Draft PR #1312 exists, but its local experiment spans approximately 38 files and
nearly 5,000 added lines. It also needs splitting.

Planned slices:

1. Typed XEP-0045 status-332 presence construction and RoomActor transition.
2. Terminal versus resume-preserving shutdown coordinator.
3. Bounded cluster relay/replay delivery and end-to-end shutdown coverage.

Completion invariants:

- status 332 is emitted only for non-resumable service shutdown;
- deployment drains preserve resumable XEP-0198 state;
- every affected occupant session receives correctly addressed unavailable
  presence, including the required self-presence shape;
- repeated shutdown signals are idempotent; and
- dead or slow sessions cannot prevent process termination.

## Shared completion work

### #1136 and PR #1349

PR #1349 currently proves that the stanza-handler timeout metric can be recorded
through the production Rust helper. It still needs end-to-end deployment proof:

- trigger a synthetic timeout;
- query the actual Prometheus/Mimir translation;
- document the canonical series and labels;
- repair the existing OTLP/collector path if the signal is absent; and
- complete the remaining dead-metric wire-or-remove audit.

### #1163

The timeout alert remains blocked until #1136 proves the production series.
After that, add the alert as code, validate its rendered deployment, and finish
the requested Alloy/log-routing work. Contact-point routing may require owner
input.

### #1174

The obsolete substring parser and XEP-0184 helper are gone. The issue remains
open for verification/removal of the obsolete MUC registry and stale vCard/XML
paths. Those should remain small, behavior-preserving cleanup PRs.

## Delivery order

1. Narrow, verify, review, push, approve, and merge #1355.
2. Rebuild and merge creator-lifecycle-only #1352.
3. Rebase, verify, and merge storage-integrity #1350.
4. Reopen/correct #1289 and deliver its durable journal/coordinator slices.
5. Deliver the split #1283 destruction and MAM-epoch stack.
6. Deliver the split #1285 terminal-shutdown stack.
7. Finish #1136, then #1163, and finish the residual #1174 cleanup.
8. Merge an incremental `TODO-ISSUES.md` update after each completed issue.
9. Audit every issue, PR, test, review, and CI gate before closing epic #1279.

The slices listed above total approximately 17 additional mergeable PRs. The
estimate may shrink if small adjacent slices can be combined without weakening
review boundaries.

## Definition of merge-ready

For every code PR:

1. The implementation matches the issue and local XEP text.
2. Focused regressions and the relevant full suites pass.
3. Formatting, type checking, and strict Clippy/static analysis pass.
4. Multiple internal adversarial personas report no real actionable findings.
5. The exact reviewed head is pushed.
6. Qodo Code and Greptile approve that exact head with no actionable findings.
7. Every review thread is resolved.
8. All required CI jobs are green.
9. The PR title and description reflect the complete final diff.

No implementation PR may merge before all nine conditions are satisfied.

## Completion bookkeeping

- Reopen or correctly rescope #1289.
- Keep epic #1279 open until #1289, #1283, and #1285 are proven complete.
- Keep shared #1136/#1163/#1174 work visible until its remaining slices merge.
- Update and merge `TODO-ISSUES.md` immediately after each completed item.
- Close the epic only after an evidence-based audit, not from issue labels or a
  plausible implementation state.
