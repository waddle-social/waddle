# Detached roster push investigation (#2039)

## Evidence and boundary

Issue #2039 records three fenced append warnings on 2026-10-27. Those logs do
not distinguish absent captured authority from rejected exact ownership, and do
not establish client receipt, roster catch-up, or data loss. The published
revision cited by the issue is not a verified runtime revision.

Current main already recovers an initial missing fence without adopting a
successor epoch (`session_registry/append.rs`, introduced by #1981). Existing
captures remain immutable. A missing capture can also reflect failed reads or
unavailable authority; it is not proof of takeover.

## Implementation plan

1. Return a distinct typed missing-fence error before any fenced append write.
2. Preserve the exact captured-fence check and fail-closed behavior.
3. Report subscription roster push failures after local fallback, distinguishing
   local queue acceptance from unresolved local delivery. Neither means client
   receipt or proven roster catch-up.
4. Test missing and stale fences through the persistence append interface,
   including unchanged replay rows/counters, and verify fallback diagnostics.

No ownership reacquisition, target suppression, protocol changes, or remote
retry mechanism is justified by the available production evidence. The analogous
IQ roster-set fanout is outside this focused subscription-path investigation.
