# Delivery status

Last verified: 2026-07-16 against `origin/main` at `1b59bfc2`.

This document records the completion state of the Lane J adversarial follow-up
(epic #1279) and the shared observability/hygiene work discussed with it. It is
not a replacement for `TODO-ISSUES.md`; that file remains the ordered backlog
and must be updated and merged after each completed issue.

## Executive summary

- The original Lane J (#1243–#1268, plus review-discovered #1275) is
  complete.
- Nine of the twelve Lane J follow-up issues are genuinely complete.
- The three remaining product issues are #1289, #1283, and #1285.
- #1289 has been reopened because its durable journal and production
  coordinator are not implemented.
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
- PR #1355: bound every actor-owned MUC durable operation and publication
  boundary to the actor incarnation's exact room/entity/owner/epoch fence.
- PRs #1341 and #1345: incremental `TODO-ISSUES.md` progress updates.

## Exact MUC actor-incarnation fences — merged PR #1355

PR #1355 merged as `1b59bfc2` after binding clustered MUC durable load, save,
delete, mutation, restore, and publication paths to the immutable
`RoomClaimFenceContext` retained by each `RoomActor` incarnation. It also made
terminal ownership loss actor-specific, preserved same-JID successors during
demotion, and retained one full-tuple conditional release when local identity
supersession does not prove the old database claim disappeared.

Merge evidence for exact head `4d454695`:

- 21 of 21 GitHub checks passed, including PostgreSQL-backed `nixTest`;
- full `waddle-xmpp` and clustered `waddle-server` suites passed locally;
- strict Clippy, rustfmt, and focused identity-rotation, concurrent-dispatch,
  successor-safety, and XEP-0045 regressions passed;
- three internal adversarial review lanes reported no actionable findings;
- Qodo reported zero bugs, rule violations, requirement gaps, or skill findings;
  and
- Greptile rated the exact head 5/5 and safe to merge.

The notification, mailbox-linearized authorization, retryable application
cleanup, and MAM-incarnation coordinator remain in #1283. PR #1355 deliberately
did not claim that broader destruction scope.

## Current implementation frontier: durable creator and storage foundation

### PR #1352 — durable creator lifecycle

The remote PR remains a draft boundary commit with no implementation diff. A
prior local implementation exists, but it must be semantically ported onto
merged #1355 instead of rebasing or pushing the old combined branch: its
preparation/publication ordering and status-201 replay assumptions predate the
exact actor-fence contract.

Remaining behavior:

- atomic initialize-or-restore;
- typed created-versus-restored outcome;
- durable `AwaitingInitialConfiguration` versus active lifecycle;
- initial Owner persistence in the initialization transaction;
- status 201 only for the initialization winner;
- locked-room rejection for non-creators until accepted configuration;
- creator cancellation, unavailable-presence destruction, and failure recovery;
- restart, reclaimed-owner, competing-waiter, and exact-fence-loss behavior;
- production join/configuration integration; and
- a dedicated XEP-0045 lifecycle suite.

### PR #1350 — durable state integrity

This draft is stacked on #1352. Its current storage-only diff has twelve merge
conflict regions against merged #1355 and still uses the obsolete room-keyed
fence API. It must be rebuilt semantically after #1352 rather than resolved by
mechanically accepting either side.

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
coordinator. Issue #1289 was reopened on 2026-07-15 so its GitHub state now
matches the remaining implementation evidence.

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

GitHub issue #1283 was updated on 2026-07-15 with the mailbox-linearized owner
authorization, dormant-room authorization, durable notification retention,
caller-timeout continuation, application cleanup, and #1355 dependency found
by adversarial review. That exact-fence dependency is now merged. The retained
experiment must still be rebuilt on current `main` and split before review.

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

1. Rebuild and merge creator-lifecycle-only #1352.
2. Rebuild, verify, and merge storage-integrity #1350.
3. Deliver #1289's durable journal/coordinator slices.
4. Deliver the split #1283 destruction and MAM-epoch stack.
5. Deliver the split #1285 terminal-shutdown stack.
6. Finish #1136, then #1163, and finish the residual #1174 cleanup.
7. Merge an incremental `TODO-ISSUES.md` update after each completed issue.
8. Audit every issue, PR, test, review, and CI gate before closing epic #1279.

The slices listed above total approximately 16 additional mergeable PRs. The
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

- Keep reopened #1289 visible until every durable journal/coordinator slice is
  complete.
- Keep epic #1279 open until #1289, #1283, and #1285 are proven complete.
- Keep shared #1136/#1163/#1174 work visible until its remaining slices merge.
- Update and merge `TODO-ISSUES.md` immediately after each completed item.
- Close the epic only after an evidence-based audit, not from issue labels or a
  plausible implementation state.
