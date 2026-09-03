# Distributed-actors implementation TODO

Execution order for the [distributed-actors roadmap #1664](https://github.com/waddle-social/waddle/issues/1664), derived from the native blocked-by graph, sequenced for maximum parallelism and minimum rebase churn. Assembled from wayfinder map #1628 (closed 2026-08-07); program history in #1425 (closed).

**Critical path (9 deep):** #1651 → #1652/#1653 → #1654 → #1655 → #1656 → #1657 → #1658 → #1659. Everything else feeds this spine from the side — start the side chains immediately so the spine never waits on them.

## Standard flow (every slice)

1. Branch + **draft PR** whose description carries the plan (repo rule — first action).
2. Implement per the issue's Scope/Acceptance. Hard rules: XMPP-native, XEP conformance, typed payloads, XML via builders, dedicated XEP suites, clippy `-D warnings`, bun-only.
3. Adversarial personas review to CLEAN; reviews bind to the **exact head SHA**, remediation restarts review. `codex review` as an independent pass on spine slices.
4. All checks green (nix gates need the branch to contain current main — merge up before judging red CI). Update PR title/desc, undraft, merge; monitor CI to green.
5. Behavioral slices get post-deploy Loki/metric verification (merges roll to prod via Flux).

Model assignment: **gpt-5.6-terra via codex** (worktree-isolated) for clear-spec slices; **fable-5 / opus-5 / gpt-5.6-sol** for semantically dangerous ones (marked ⚠ below). Reviews always fable-5/opus-5 personas.

Known traps: cuenv lock drift is nondeterministic (rerun, never edit the lock) · chat tests need `bun run build` first · jingle/telemetry tests flake when filtered · span-export close race is CI-only.

---

## Wave 1 — now, all parallel (worktrees)

Merge order within the wave: **#1642 lands first** (advisory remediation + −6k-line deletion everyone else would rebase over). The rest merge as they go green.

- [OK] **#1642** `fix(server): remove XEP-0397 ISR and drop the token store` — re-derive from harvest branch `codex/distributed-actors-p0-3`; **add the `clustering_isr_tokens` DROP migration** #1610 lacked; carries ADR-011; stale-doc cleanup. Closes advisory GHSA-5687-26jr-g8vv on merge. Behavioral: clustered reconnects take the slow path (sanctioned).
- [OK] **#1316** session-bootstrap flood overruns unacked queue — MERGED 2e726779 (PR #1669, 2026-08-09) after 13 codex review rounds. Owed: post-deploy Mimir check (SmUnackedEvictions now `> 0` with `path` label).
- [OK] **#1294** cross-node resume takes over the UserActor claim — MERGED cd9ba752 (PR #1668, 2026-08-09; squash-rebased onto post-#1669 main). Owed: post-deploy Loki checks.
- [OK] **#1389** conflict-close surfaces as terminal "replaced by newer sign-in" — MERGED with #1294 in PR #1668 (superseded-session recovery: witnesses, banner on all surfaces, successor convergence).
- [OK] **#1644** `feat(server): room lifecycle and revision types with expand-only schema` — MERGED e436da56 (PR #1673, 2026-08-11). Store-owned ensure_schema DDL, no migration version (#1651 freeze intact); codex-bot review loop clean at 7c8dd1d2. #1645-binding notes in the PR desc. Unblocks #1645.
- [OK] ⚠ **#1650** `feat(server): typed ingress identity domain and versioned semantic digest` — ten identity types + SemanticDigest v1 canonicalizer + property suites; **fixes and closes #1137** (wrap-aware comparator at the three plain-`>` sites).
- [OK] **#1651** `fix(server): append-only migration ledger with checksum enforcement` — MERGED 43860571 (PR #1671, 2026-08-10). Codex review clean at bf8f7949. **Rolling-deploy constraint: do not add a new migration version until ledger-aware binaries are fully rolled out** (a pre-ledger pod seeing an unknown version would reset the ledger). Unblocks #1652 and (with #1650) #1653.
- [OK] **#1648** `feat(server): closed observation variants and bounded cluster metrics` — MERGED b072120c (PR #1708, 2026-08-23); the four metric families mapped onto closed variants, PR #1238 consumed and closed.
- [OK] **#1649** `feat(chat): closed telemetry observations with source-level identity removal` — MERGED 33018c2c (PR #1719, 2026-08-27). Faro identity removed at source (resource, `call_id`, message IDs, error text/stacks); closed observation unions + measurement schemas; native surfaces proven telemetry-off by `scripts/check-native-remote-telemetry.sh` (TOML-semantic Cargo closure) wired into Apple/Android/chat CI.

## Wave 2 — after their wave-1 blockers

- [OK] ⚠ **#1643** `feat(server): durable principal fence for cross-node SM resume` (after #1642) — MERGED 375ba545 (PR #1666, 2026-08-07). Both #1610 defects root-caused; custody branch deleted. Owed: post-deploy Loki checks, follow-up #1667 (expired-claim promotion). Note for #1651: do NOT resurrect harvest's fail-closed runner policy as-is — it breaks the combined global+waddle test-runner history (see PR #1666 root-cause).
- [OK] ⚠ **#1645** `fix(server): commit durable room state before memory mutation` (after #1644) — MERGED 90240584 (PR #1692, 2026-08-14). The inversion across every durable mutation kind; boundary lock ordering; retires `OwnershipLostAfterApply`/`PersistFailed` windows. Heavy adversarial review.
- [OK] **#1652** `feat(server): database lineage attestation at readiness` — MERGED 0b59cfca (PR #1672, 2026-08-10); enrollment-gated rollout owed.
- [OK] ⚠ **#1653** `feat(server): expand-only foundation schema with inert epoch guards` — PR #1686: table pack, inert transaction-bound guards, manifest, and epoch-0 compatibility proof.

## Wave 3

- [OK] **#1646** `feat(server): durable room effect outbox with per-lifecycle FIFO` (after #1645) — follow the `call_teardown_outbox` pattern; destroy leases + tombstone. MERGED 8ea8cfe5 (PR #1694, 2026-08-16). Follow-up **#1696** (end-to-end remote write acceptance for relayed effects) MERGED 9923bd02 (PR #1727, 2026-09-03) — shipped as a dedicated `remote_resource_write_accepted.v1` ask; `deliver_ordered.v2` unchanged.
- [OK] **#1647** `feat(server): one-use occupancy projection authorization` (after #1645, ∥ #1646) — PR #1702; claim-fenced one-use occupancy and pin projections, with local departure convergence retries. Pin-state rehydration remains with the P4 owner (#1660), per the #1647 scope amendment.
- [OK] **#1654** `feat(server): Postgres ingress unit-of-work seam and substrate repositories` (after #1652 + #1653) — dark; one-transaction-spans-everything proof.

## Wave 4

- [OK] **#1655** `feat(server): transaction-taking MAM and inbox repositories` (after #1654) — MERGED 61726ad1 (PR #1691, 2026-08-12). MAM leaves its private pool; call sites untouched until cutover.
- [OK] ⚠ **#1656** `feat(server): shadow atomic ingress transaction` (after #1655 + #1643 + #1644) — MERGED 0818f7fd (PR #1693, 2026-08-14); issue stays open as the soak-gate record. Full boundary transaction on live traffic, new-tables-only; shadow health via P0.5 closed-variant vocabulary.

**⏸ Soak gate:** deploy #1656 and let the shadow run in prod until decision-class / alias-outcome / retry counts look clean. Do not collapse #1656 and #1657 into one step — the gap is the de-risking.

- [ ] **#1695** `ops(server): enable WADDLE_INGRESS_SHADOW_ENABLED in prod and run the #1656 soak` — the shadow shipped **off** (prod ConfigMap verified 2026-08-20: no `WADDLE_INGRESS_SHADOW_*` key); the soak had not started. Enables the flag via `config.extraEnv` in the prod HelmRelease, adds the `waddle-ingress-shadow` Mimir alert group + dashboard row, and the runbook `server/docs/operations/ingress-shadow-soak.md` with the pass criteria and observation window. #1657 stays blocked until the window is recorded as passed on #1695.

## Wave 5

- [ ] ⚠ **#1657** `feat(server): ingress authority cutover with canonical identity` (after #1656 + #1645) — `h` advances post-commit by decision class; **MUC origin-id dedupe retires here** with its scenarios migrated; "no reachable pool-per-operation correctness write" asserted; shadow scaffolding removed in the same PR. Riskiest flip on the roadmap; small PR by design.

## Wave 6–7

- [ ] ⚠ **#1658** `feat(server): idempotent direct-message effect execution` (after #1657) — keeps #1316's regression seam green.
- [ ] ⚠ **#1659** `feat(server): fenced MUC effect manifest and reflection` (after #1658 + #1646) — deletes `GroupchatRetrySuppression`.
- [ ] ⚠ **#1660** `feat(server): opaque delivery keys for extension effects` (after #1658, ∥ #1659) — single receipt authority; calls/pins stay `AwaitingDurableOwner`.

## Wave 8 — sharpening evaluations (grilling sessions, not implementation)

- [ ] **#1661** P2 umbrella → fine slices (after #1659 + #1660 merge) — mailbox + zero-payload hints; **staged lane-by-lane relay cutover**, ordered-relay code deleted last.
- [ ] **#1662** P3 umbrella → fine slices (after P2 sharpened) — includes committed **P3.4: XEP-0388 SASL2 + XEP-0198 §11 inline resume** (cross-surface: server + WASM client + chat). Keep-green: #1294/#1389 seams.
- [ ] **#1663** P4 sharpening (after P3 umbrella) — evaluate remaining state/actor cleanup against the then-current codebase.

## Activation backlog (each its own issue when prerequisites land; prod activity needs explicit approval + named window + runbook)

- [ ] Epoch 0→1 flip (guard coverage + no-old-writers verification; forward-only)
- [ ] **#1689** Provision ingress-guard migration-owner/runtime-role split before the epoch 0→1 flip.
- [ ] Cluster admission enablement
- [ ] NOT NULL tightening of `sessions` auth-context columns
- [ ] SASL2 stream-feature advertisement (P3.4)

## Housekeeping (nothing blocks on these)

- [ ] Adjudicate **#1641** (unlanded #1357 remnants: transport write-responsibility seam + 0198 suite; generation-fenced callback test) — ordinary-bug work; custody branches retained until resolved.
- [ ] Delete stale branches: `codex/reject-muc-client-delay`, `codex/seal-room-destroy-mam-epoch`, `codex/send-muc-status-332`, `codex/reap-dead-room-claims`, `codex/1311-monolith-backup`, `codex/adr0017-user-actor-claim-lifecycle`, `codex/fix-cluster-remote-resource-reconciliation`; `codex/fix-public-channel-members-only-backfill` only after verifying the five-channel `members_only` prod state (repair unlanded, its V1007 slot consumed — re-file as fresh migration if still broken).
- [ ] Custody branches stay until their consumers land: `codex/distributed-actors-p0-1-client-sm` + `backup/pr1357-p0-1-pre-successor-20260724` (until #1641). `codex/distributed-actors-p0-3` DELETED 2026-08-07 (#1642 + #1643 both merged).
- [ ] Close/publish decision on advisory GHSA-5687-26jr-g8vv after #1642 merges.
