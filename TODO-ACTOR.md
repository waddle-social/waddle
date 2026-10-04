# Distributed-actors implementation TODO

Status audited **2026-09-27** against `main@35ecd05c8`, GitHub issue/PR state, native dependency edges, and the source/test evidence noted below. A merged PR proves implementation landed; it does not prove a deployment or an operational check succeeded. This audit did not query live telemetry or rerun Rust suites.

The program index is [#1664](https://github.com/waddle-social/waddle/issues/1664); historical decisions are in closed #1628 and #1425. Their original “takeable now” lists are historical. Native blocked-by edges remain canonical for sequencing.

## What to tackle next

- **Immediate reliability:** #1386 — coordinate clustered shutdown with SM drain completion and classify terminal authority loss. The 2026-09-25 issue report remains relevant; current code already sleeps 250 ms between nonempty passes, so the old “no backoff” diagnosis is stale.
- **Distributed-actors bug:** #1732 — retire displaced-generation media authority without revoking the successor's tokens or removing its LiveKit participant. #1869 narrowed the replacement paths but explicitly deferred this work; re-establish the reproducer against the new bind fencing.
- **Small distributed-actors task:** #1688 — require `deployment.uuid` at render time for durable database configurations. Clustering already requires it; cover the remaining durable/noncluster configurations, preserve explicit dev/memory behavior, bump the chart, and verify the GitOps render.
- **Roadmap progress:** finish the #1658 acceptance audit, beginning with #1759 reconciliation and the remaining #1776 guarantees. Do not reimplement closed carve-outs listed below.
- **Larger recovery task:** design #1826 and #1825 together — durable remote-join handoff plus recovery of departures after socket-node restart. Persisting membership only after the join reply does not close the lost-acknowledgement gap.

**Remaining critical path:** #1658 → (#1659 ∥ #1660) → #1661 → #1662 → #1663. All prerequisites through #1657 are closed. The later umbrellas are sharpening/evaluation work, not ready-to-code feature tickets.

## Delivery and verification

1. Follow `AGENTS.md`: branch and draft PR with a plan before nontrivial implementation; preserve unrelated work.
2. Use XMPP-native semantics, relevant XEPs, typed payloads and XML builders. Rust protocol work needs the dedicated XEP suites; keep Clippy clean with `-D warnings`; use Bun for JavaScript/TypeScript.
3. Material design changes need independent architecture review. Resolve concrete in-scope review findings, and bind review/verification evidence to the reviewed commit.
4. Run the checks appropriate to the change, update the PR description, mark ready, and monitor CI. Keep the #1316 pacing and #1294/#1389 resume regressions green in later actor work.
5. Record deployment verification separately from merge status. Use the current cutover runbook for incompatible changes; production activation needs its own authorized window and evidence.

## Completed foundation and authority work (former waves 1–5)

Every issue in this table is closed. Historical constraints are retained where they matter to future changes.

| Issue | Landed work | Evidence / follow-up |
| --- | --- | --- |
| #1642 | Removed XEP-0397 ISR and the token store; ADR-011 | PR #1665 merged 2026-08-07. Superseded PR #1610 remains closed. Advisory disposition is still pending below. |
| #1643 | Durable principal fence for cross-node SM resume | PR #1666 merged 2026-08-07. Expired-claim promotion follow-up #1667 also closed via #1718; Grafana verification is tracked by #1722. Do not restore the rejected fail-closed migration-runner policy from the harvest branch. |
| #1316 | Bootstrap pacing and deferred-cap recovery | PR #1669 merged 2026-08-09. Eviction-path telemetry verification is tracked by #1722. |
| #1294, #1389 | Cross-node UserActor takeover and nonterminal successor recovery | PR #1668 merged 2026-08-09. Preserve their regression suites; the old roadmap's separate Loki follow-up has no completion evidence recorded in this audit. |
| #1644 | Room lifecycle/revision types and expand-only schema | PR #1673 merged 2026-08-11; unblocked #1645. |
| #1645 | Persist durable room mutations before memory changes | PR #1692 merged 2026-08-14. |
| #1646 | Durable room effect outbox and per-lifecycle FIFO | PR #1694 merged 2026-08-16. End-to-end remote write acceptance #1696 also closed via PR #1727 on 2026-09-03. |
| #1647 | Claim-fenced occupancy and pin projections | PR #1702 merged 2026-08-22. Pin-state rehydration remains deferred to the later durable-owner work. |
| #1648 | Closed server observations and bounded metrics | PR #1708 merged 2026-08-23; consumed and closed unmerged baseline PR #1238. |
| #1649 | Identity-free closed browser telemetry | PR #1719 merged 2026-08-27. Native telemetry-off checks landed; Grafana/Faro acceptance remains #1722. |
| #1650, #1137 | Typed ingress identities, semantic digest and wrapping SM comparisons | PR #1676 merged 2026-08-11. |
| #1651 | Append-only checksummed migration ledger | PR #1671 merged 2026-08-09. The original pre-ledger rollout freeze is historical, not a current blanket migration freeze; subsequent migrations have landed. |
| #1652 | Database lineage attestation | PR #1672 merged 2026-08-10. Enrollment completed through #1674/#1675; keep the deployment UUID stable. Remaining chart-wide validation is #1688. |
| #1653 | Foundation schema and inert epoch guards | PR #1686 merged 2026-08-12. Role separation #1689 and epoch activation remain separate. |
| #1654 | PostgreSQL ingress unit of work and substrate repositories | PR #1690 merged 2026-08-12. |
| #1655 | Transaction-taking MAM/inbox repositories | PR #1691 merged 2026-08-12. |
| #1656 | Shadow atomic ingress transaction | PR #1693 merged 2026-08-14; issue closed with the authority cutover on 2026-09-08. |
| #1695 | Shadow soak disposition | Window ended early by operator decision on 2026-09-05; issue closed 2026-09-08. Evaluable criteria passed; retention-horizon criteria were not evaluated, not declared passed. Finding #1735 was fixed by #1736. |
| #1657 | Ingress authority cutover and canonical identity | PR #1738 merged 2026-09-08; removed shadow scaffolding. Operations: [ingress authority](server/docs/operations/ingress-authority.md). Current rollout strategy is governed by the newer #1869 cutover below. |

## Effect executors (former waves 6–7)

### #1658 — open, prerequisites complete

The direct-message executor remains the prerequisite for both #1659 and #1660. Its September 20 comment is no longer an accurate list of open carve-outs.

**Landed/closed:** #1739–#1743 recovery foundations (PR #1752), #1753 extension ingress identity, #1755 recovery executor, #1756 keyed recipient append receipts, #1757 per-occupant fanout progress, #1760 append-proof/payload liveness, #1778 cross-node detached append identity, #1789 registered-socket detach identity, and #1805 UserActor delivery identity (PR #1820). Adjacent #1803 backlog work and #1804 relay compatibility are also closed.

**Recent completion:** #1770 archive/dispatch ordering landed in **PR #1834 on 2026-09-24**. It includes the production #1759 live full-JID recipient-preparation path, exact dispatch claims/offer state, and ordered recovery. It is no longer a draft awaiting merge.

Still to reconcile before closing #1658:

- [ ] **#1759:** verify the full live-full-JID plan/commit/execute acceptance on SQLite and PostgreSQL, especially one recipient archive/unread mutation on retry and carbon behavior. Production work is merged; historical synthetic tests and contradictory runbook paragraphs still need reconciliation (see closure audit below).
- [ ] **#1776:** remaining durable send/observer guarantees. #1834 advanced live ordering and claims, but explicitly retained offer-to-SM crash uncertainty and non-SM/keyless at-least-once cases. Do not equate dispatch ordering with every sink being idempotent.
- [x] **#1790:** relay receivers defer the canonical authorization read to the append decision that trusts it (local socket boundary, detached keyed append; the socket node for registered remote frames). Forwarding and owner-to-socket hops no longer read; no liveness probe, rejections stay definitive. Follow-up: the `RemoteUserSideEffect::Carbons` receiver keys appends with no canonical read.
- [ ] Compare the full #1658 scope with current code/tests: frozen targets before `h`, exact replay bytes and delay, distinct archive identities, ordinal round-trip, crash recovery, and non-SM uncertainty. Closed child issues alone do not establish epic completion.

### Remaining dependent work

| Issue | Current state | Boundary to preserve |
| --- | --- | --- |
| #1659 — fenced MUC manifest/reflection | Blocked by open #1658; #1646 prerequisite is complete | Frozen pre-`h` targets, reflection as an ordinary child, idempotent missing-child recovery; `GroupchatRetrySuppression` still exists in `server/routes/interpret/deps.rs` and the tombstone path; deletion remains pending. |
| #1660 — opaque extension delivery keys | Blocked by open #1658; parallel with #1659 once unblocked | One receipt authority, durable same-key acceptance, descendant-aware GC; calls/pins retain the `AwaitingDurableOwner` carve-out. |
| #1661 — P2 transport sharpening | Blocked by #1659 and #1660 | Mailbox + zero-payload hints; explicit **P2.2 mailbox DDL/core** slice is needed (issue comment identifies the omission). Cut relay consumers over lane by lane before deleting ordered relay. |
| #1662 — P3 connections/resume sharpening | Blocked by #1661 | Build on existing claim/resume machinery. Includes server + WASM/chat XEP-0388 SASL2 and XEP-0198 §11 inline resume; preserve #1294/#1389 behavior. |
| #1663 — P4 state/actor sharpening | Blocked by #1662 | Evaluate remaining state against the code then; calls/pins durability and obsolete actor cleanup are not assumed complete. |
| #1664 — program index | Open umbrella | Keep open until the program is dispositioned; its initial “takeable now” section needs reconciliation with this completed foundation. |

## Occupancy, relay and independent follow-ups

- [x] **#1733:** same-full-JID occupancy displacement and generation fencing, **PR #1869 merged 2026-09-27**. Includes typed legacy resume failure, bounded retirement, shorter SQL guard lifetimes, exact media rollback authority, and SM custody checks. Plan: [occupancy displacement](docs/planning/1733-occupancy-displacement.md).
- [x] **#1804:** live relay compatibility, PR #1841 merged 2026-09-24. Covers the documented route/frame baseline, not arbitrary registration/schema compatibility.
- [x] Remote mirror/MUC cleanup PR #1821, retirement operations PR #1823, and reconnect-contract PR #1824 merged 2026-09-22. Their remaining durability gaps are #1825/#1826, not unfinished work in those PRs.
- [ ] **#1732:** generation-specific token revocation and safe displaced-participant retirement. Ordinary local replacement changed under #1869; validate the residual path before implementation. Never revoke the shared identity's entire token bucket or remove a successor based only on the old FullJID.
- [ ] **#1825 + #1826:** durable membership/departure recovery and join acknowledgement handoff. Preserve genuine resume and replacement generations; transport failure is not departure authority.
- [ ] **#1386:** bounded coordination of room/SM shutdown, terminal-versus-transient confirmation outcomes, and a regression running both lifecycles together. Preserve immediate fatal-fence preemption and successor custody.
- [ ] **#1709:** the stack overflow was mitigated by PR #1710's 8 MiB Tokio worker stack. The remaining task is to identify/box the oversized Jingle future, not to rediscover the shipped mitigation.
- [ ] **#1699:** consolidate overlapping room-outbox flake tracking with #1705/#1745 only after preserving the deterministic mid-pass test requirement and the separate group-DM rename failure mentioned on #1699. The original drain defect was fixed in #1708.
- [ ] **#1641:** adjudicate unlanded #1357 transport-write responsibility and generation-fenced client callback work; retain custody branches until both packets are dispositioned.

Other useful open reliability tasks: #1787 reconnect/offline-message loss (not resolved by #1869), #1806 PostgreSQL fixture-name truncation, #1846 push failure classification, and #1847 terminalization alert semantics. These are adjacent bug work, not prerequisites inferred for the actor spine.

## Cutover, activation and operational verification

- [ ] **Current #1869 cutover:** committed GitOps strategy is `Recreate` for V0013 occupancy authority and register/force-detach protocol changes. Verify the new fleet and the runbook checks, then restore `RollingUpdate` in a **separate** change through the #1841 cutover guard. Neither the earlier #1657 instruction nor #1841 authorizes skipping this cutover. See [relay cutovers](docs/operations/relay-cutovers.md#occupancy-authority-cutover-1733-pr-1869). No live completion is asserted by this audit.
- [x] **Database lineage enrollment:** #1674 provisioned the UUID; #1675 recorded ready/attested replicas and removed one-shot enrollment. This is no longer owed rollout work.
- [ ] **#1688:** durable-database render-time UUID requirement, as scoped above.
- [ ] **#1689:** provision the migration-owner/runtime-role split; #1653 is complete, so implementation is unblocked. Runtime must not own/alter protected tables or assume the owner role.
- [ ] **Epoch 0→1 activation:** requires #1689, complete guard coverage, no old writers, and an explicit forward-only activation plan. Do not infer activation from merged guard code.
- [ ] **Cluster admission enablement:** retain as a separate activation item; this audit did not establish a completed activation.
- [ ] **`sessions` auth-context NOT NULL tightening:** separate migration/activation after proving the remaining data satisfies it.
- [ ] **SASL2 feature advertisement:** separate activation after P3.4 server/client implementation and conformance coverage.
- [ ] **#1722:** record all four Grafana-side checks: eviction `path` labels, shutdown budget, claimed-expiry promotion, and identity-free Faro events. Pod-side checks and merged code do not complete this issue.
- [ ] **Historical #1294/#1643 Loki follow-up:** reconcile the old roadmap's owed checks with recorded rollout evidence; no new operational check was performed here. #1722 covers the later #1667 promotion verification.
- [ ] **Advisory GHSA-5687-26jr-g8vv:** API still reports **draft**, with neither publication nor closure recorded. #1642 remediation is merged; decide publication/closure separately, without conflating remediation with advisory disposition.

## Open-issue closure audit

Recommendations below are based on source/test inspection and merged PR evidence. Issues have **not** been closed by this documentation update, and existing tests were inspected rather than rerun.

### Ready to close with the implementation evidence

| Open issue | Recommendation and evidence |
| --- | --- |
| [#1338 — migration race/atomicity](https://github.com/waddle-social/waddle/issues/1338) | Close as completed by [PR #1671](https://github.com/waddle-social/waddle/pull/1671). The migration runner takes a PostgreSQL transaction-scoped advisory lock and commits DDL with the ledger updates in the same transaction; concurrent-runner and rollback tests cover the two reported failures. |
| [#1296 — remote cleanup retries against a live owner](https://github.com/waddle-social/waddle/issues/1296) | Close as superseded by [PR #1785](https://github.com/waddle-social/waddle/pull/1785) and [PR #1869](https://github.com/waddle-social/waddle/pull/1869). Fresh foreign claims no longer drive noisy acquisition retries; exact-generation cleanup can route through the current UserActor owner. Claim-observation tests and the dedicated transferred-owner cleanup regression cover both deferral and convergence without waiting for owner death. No new live telemetry check was performed. |
| [#1737 — room authority follows relay metadata](https://github.com/waddle-social/waddle/issues/1737) | Close as completed by [PR #1738](https://github.com/waddle-social/waddle/pull/1738). Digest authorities now derive from offered room shape; the actual owner commits a claim-fenced `Relayed` ingress plan before reserved MUC execution, including local-owner fallback. Shape/plan/fenced-commit tests cover the replacement design. |

### Acceptance reconciliation or consolidation first

| Open issue | Why it should not be closed unconditionally |
| --- | --- |
| #1138 | Original detach age is preserved by the snapshot codec introduced in #1676 and restored for expiry checks. Implementation appears fixed, but this audit did not establish the exact repeated successful fanout → restart → original expiry-window regression. Verify that acceptance before closing. |
| #1759 | Core feature landed in #1834. `interpret/tests/plan.rs` now expects recipient archive preparation, but `tests/ingress_cases/recipient_drift.rs` still constructs historical unreceipted live plans. The ingress runbook's older #1759 limitation contradicts its newer completion paragraph. Reconcile those and establish the exact retry/unread acceptance evidence, then close rather than reimplement. |
| #1699 | Original outbox defect fixed in #1708; overlaps #1705/#1745. Preserve the deterministic scheduling refinement and separately disposition the group-DM rename failure before closing as consolidated. |
| #1658, #1776 | Partial completion is substantial, but the documented keyless/uncertain-send guarantees remain. Keep open for the remaining acceptance work. |
| #1709, #1790 | Mitigation/current-caller changes make their descriptions stale; neither establishes that the remaining optimization work is complete. |
| #1401 | #1869 addresses connection displacement, but malformed/invalid bind error handling remains a separate acceptance requirement. |
| #1295 | Ordinary cluster drain still skips UserActor claims; do not close merely because #1294 takeover or room draining landed. |
| #1298 | Member-list IQ still depends on a local RoomActor; relay support alone does not prove remote-owner query acceptance. |
| #1670 | Cancellation/custody improvements do not establish durable promotion inventory for every failure/restart path; some retry queues remain memory-only. |
| #1427 | Some retention/age-out work exists, but the missing-target quota-bounce path still needs disposition. |

## Branch and tracker housekeeping

Remote branch existence was checked with `git ls-remote` on 2026-09-27; no branches were deleted in this audit.

- [x] `codex/distributed-actors-p0-3` is absent; #1642/#1643 are complete.
- [x] #1869's `fix/1733-displace-occupancy-on-bind` local/remote branch cleanup completed after merge.
- [ ] Previously listed stale branches **still exist remotely**: `codex/reject-muc-client-delay`, `codex/seal-room-destroy-mam-epoch`, `codex/send-muc-status-332`, `codex/reap-dead-room-claims`, `codex/1311-monolith-backup`, `codex/adr0017-user-actor-claim-lifecycle`, `codex/fix-cluster-remote-resource-reconciliation`. Verify unique commits and associated issue disposition before deleting; existence alone is not proof they are disposable.
- [ ] `codex/fix-public-channel-members-only-backfill` still exists. Preserve until the five-channel `members_only` state is verified; the old V1007 slot was consumed, so any still-needed repair requires a fresh migration.
- [ ] Custody branches `codex/distributed-actors-p0-1-client-sm` and `backup/pr1357-p0-1-pre-successor-20260724` both still exist. Keep until #1641's packet decisions are recorded.
- [ ] Reconcile the GitHub #1664 index and stale issue descriptions using this audit. This update changes the repository roadmap only; it does not edit GitHub issue bodies or record unperformed operational results.
