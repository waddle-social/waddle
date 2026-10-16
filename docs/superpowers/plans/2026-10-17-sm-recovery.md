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

## Remaining limits

A crash after canonical source deletion but before portable receipt cleanup can retain content-free origin/sequence receipts. This affects storage housekeeping, not payload durability. Historical production row contents and post-deploy quota telemetry cannot be established by local tests.

Review also identified a pre-existing, separate expired-during-resume completion path that deletes its source without promotion. This change covers promotion custody transitions; expired-resume completion remains a follow-up rather than expanding the current four issues.
