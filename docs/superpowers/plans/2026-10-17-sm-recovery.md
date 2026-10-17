# Durable stream-management recovery

Issues: #1670, #1198, #1807, #1427.

## Plan

1. Make bulk SM session scans skip and count corrupt records without weakening exact-JID fail-closed probes (#1198).
2. Accept typed stanzas at detached-stream append APIs, preserving replay serialization and rejecting non-stanza frames (#1807).
3. Persist a typed, non-resumable promotion inventory before source invalidation or consumption. Recover it after restart, fence settlement, and retain failed work until durable promotion completes (#1670).
4. Classify non-bounceable server-generated messages, preserve resolvable-sender error delivery, and verify pending queue expiration releases quota (#1427).

## Verification

The issue acceptance criteria establish the test seams: persistence storage APIs for corrupt-row isolation; public detached-stream recording and XEP-0198 replay for typed append; source consumption, promotion, restart and janitor APIs for durable recovery; promotion delivery and pending-storage expiration for quota handling. Add a failing regression at each seam before its implementation.

Review the durability design independently before implementation, then review the integrated diff. Run focused Rust tests and cargo check during development; finish with formatting, Clippy with warnings denied, the workspace nextest suite, doctests, and XEP coverage checks available in the repository. Record environment or live-deployment limits explicitly.

## Integration

Keep #1198 and #1807 bounded. Inventory lifecycle and quota handling must agree on successful durable settlement; retries must not duplicate pending rows or restore resume eligibility. No new dependencies.

## Implemented design

- Reuse `sm_sessions` and `sm_unacked` as canonical promotion inventory. Set both resume limits to zero, retain the exact claim fence, and let startup hydration and the existing expiry janitor recover unfinished work. Registration reconciles against the durable watermark; captured newer suffixes survive failed writes without recreating retired prefix entries.
- Capture the terminal backlog synchronously before the first backend await. Cancellation guards retain the complete prefix and accepted suffix; append commits the stanza and outbound watermark together. The normal promotion/confirmation chain owns retirement.
- Portable pending storage can use a database separate from SM storage. Record an origin/sequence handoff receipt in the same transaction as pending insertion; quota rejection rolls back both. Keep receipts after delivery until the canonical SM source is confirmed deleted.
- Bulk metadata scans skip and count corrupt rows. Exact-JID probes remain fail closed, and retraction sweeps use strict ID enumeration so corrupt metadata cannot hide readable message payloads.
- Public detached append APIs accept typed stanzas; existing immutable replay bytes retain their established contract. Senderless server frames can use alternate live delivery but do not enter offline promotion or bounce retry loops. Existing MAM filtering and pending-retention expiry remain in place with added coverage.
- Shutdown carries immutable evidence of an exact lost claim with each capture. It abandons only matching local custody without deleting durable state or repeating an unavailable authority lookup. Uncertain ownership retains custody without sink effects; expiry skips proven-lost sources and continues healthy work. A successor consuming a stale source row does not prevent unrelated recovery.

## Final lifecycle verification

The final lifecycle repair passed 234 focused registry, shutdown, expiry, self-fence, and PostgreSQL promotion regressions on the default stack. Formatting and diff checks passed, and independent adversarial review found no remaining actionable findings. Full all-feature workspace checks run in PR CI because the local disk cannot hold the complete test archive.

## Remaining limits

A crash or transient receipt-delete failure retains content-free origin/sequence receipts until the bounded janitor proves both canonical source absence and absence of local custody. Source-read errors preserve the receipts. Historical production row contents and post-deploy quota telemetry remain tracked in #1971 and cannot be established by local tests; #1427 stays open.

Review also identified a pre-existing, separate expired-during-resume completion path that deletes its source without promotion. This change covers promotion custody transitions; expired-resume completion is tracked in #1972.

## PR review repair plan

1. Bind every captured queue and clone to its original claim; reject stale captures after demotion, successor completion, and identity rotation. Keep explicit fresh-source admission cancellation-safe and single-use.
2. Complete the public typed append boundary and preserve canonical thread/replay serialization. Make atomic append outcomes explicit and reject conflicting sequence reuse.
3. Add timestamped portable receipts, serialized/additive bootstrap, source-proven orphan cleanup, and completion-path coverage. Never expire receipts solely by age.
4. Narrow senderless exclusions to audited server projection classes, durably hand quota errors to offline local senders, and exercise the production age-out sweep and exported metric.
5. Simplify repeated registration and claim-loss handling under the existing regression coverage. Reuse canonical session and receipt APIs; add no dependencies.
6. Keep #1427 open for historical production-row classification and post-deploy telemetry. Track expired-resume completion separately. Reply to each addressed review item with its implementation and evidence.

Capture authority travels with every queue and clone. Ordinary captures retain an immutable fence; only captures installed on the registry-owned source before a specific resume acquisition share that attempt's result. Unknown results are reconciled read-only, without replaying acquisition. Cancellation can retain a matching handoff without granting it permission to perform effects.

Every fenced promotion mutation receives the captured owner and epoch directly. Terminal recovery carries one registered inventory guard through prefix and suffix handling; its final existing-source refresh must persist accepted suffixes newer than the durable watermark before replacing the guard's payload. It never admits a missing source.

The frozen review repair passes 459 focused tests, including native PostgreSQL and SQLite, the actual stale-batch demotion/successor/recovery path, cancellation before an ambiguous resume result is published, all three receipt completion paths, and suffix-write failure followed by reopening pending storage. Formatting and independent adversarial review pass. Final workspace Clippy and CI run against the resulting commit.
