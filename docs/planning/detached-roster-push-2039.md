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

## Implementation

1. Return a distinct typed missing-fence error before any fenced append write.
2. Preserve the exact captured-fence check and fail-closed behavior.
3. Deduplicate the detached/claimed discovery snapshot by resource; concurrent
   resume can otherwise expose the same target in both maps. Report subscription
   append rejections immediately, then report the local fallback outcome,
   distinguishing local queue acceptance from unresolved local delivery. Neither
   means client receipt or proven roster catch-up.
4. Test missing and stale fences through the persistence append interface,
   including unchanged replay rows/counters. Verify fallback diagnostics and
   preservation of rejection evidence when a later resource blocks and the
   caller cancels fanout.

No ownership reacquisition, target suppression, protocol changes, or remote
retry mechanism is justified by the available production evidence. The analogous
IQ roster-set fanout is outside this focused subscription-path investigation.

## Historical telemetry check

A bounded read-only recheck of the original 2026-10-27 16:38–21:56 UTC window
returned the same three warnings. All lack captured-fence presence, push ID,
roster version, and delivery disposition. All three retrieved traces contain
12 spans with UNSET statuses. Their later `send_to` span has no result or payload
correlation; a trace-correlated log search returned only the same warnings.
No production state was changed. These observations cannot establish the
historical pushes' disposition or the reason their capture was unavailable.

[Original observation window](https://waddlesocial.grafana.net/d/waddle-overview/waddle-overview?from=1793119080000&to=1793138400000)

## Reading future warnings

- `missing captured ownership fence` means the append never reached the
  transactional owner check. It may reflect failed capture recovery or unavailable
  authority, and does not prove a successor exists.
- `fencing check failed` with a supplied capture means exact owner/epoch or local
  process authority validation rejected the write. It does not by itself identify
  a successor or its delivery outcome.
- `disposition=pending_local_fallback` on `Failed to record detached roster push`
  records the append rejection immediately, before awaiting another resource.
  A caller timeout can cancel fanout without erasing this evidence.
- `disposition=local_queue_accepted` means the final local fallback queue accepted
  the push. It does not mean a socket write or client acknowledgement succeeded.
- `disposition=unresolved_locally` means no local fallback acceptance was observed.
  No remote retry or roster catch-up is implied.

The immediate rejection warning retains its original text for existing count
queries: one such warning per rejected append. The separate
`Detached roster push local fallback outcome` event carries the final disposition.
`push_id` and `roster_version` correlate both events with the fallback frame.
A missing final event leaves the outcome unestablished; it does not prove either
acceptance or failure. Neither fence rejection writes a session, replay entry or
ingress custody proof.

Roster-version catch-up requires a subsequent client roster query; it is not an
automatic consequence of a new bind. Existing happy-path replay and stale-version
query tests establish those mechanisms, not recovery of the three observed pushes.
