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
