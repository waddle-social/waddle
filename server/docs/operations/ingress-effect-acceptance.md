# Direct-message effect acceptance (#1658)

This matrix reconciles the original epic with the delivery policy merged in
#1899. That policy supersedes terminal non-SM uncertainty: an unknown started
send may retry after its 60-second deadline. A completed or custodied keyed
effect does not execute again merely because its aggregate receipt was lost.
Normal XEP-0198 retransmission and external providers without idempotency keys
do not acquire an exactly-once client-observation guarantee.

## Evidence boundaries

The source and test links below describe the implementation contract. A passing
test run proves only the scenarios it exercises. Final verification and review
must identify the exact commit; deployment verification is separate. The
baseline audit at `d17e4b61b980f0388f04abc80124b017607e75c9` is historical
evidence, not proof of later changes.

| Requirement | Implementation and regression seam | Acceptance boundary |
| --- | --- | --- |
| Freeze targets and child identities before handled acknowledgement | [direct planning](../../crates/waddle-server/src/server/routes/interpret/route_to_connection.rs), [phase A/B tests](../../crates/waddle-server/src/server/routes/interpret/tests/plan/phase_ab.rs), [recipient drift tests](../../crates/waddle-server/src/server/routes/websocket/tests/ingress_authority_recipient_drift.rs) | Commit precedes execution; retry cannot discover additional logical recipients. Current block policy can prevent a recorded delivery. |
| Distinguish fresh generated stamps from real plan divergence | [identity restoration](../../crates/waddle-server/src/ingress/restamp.rs), [restamping tests](../../crates/waddle-server/src/ingress/restamp/tests.rs), [wire checkpoint tests](../../crates/waddle-server/src/server/routes/websocket/tests/ingress_authority_recovery.rs) | Unique sender/recipient stamps restore exact recorded IDs even without MAM. Payload, target, foreign-stamp and audience changes remain visible. Ambiguous stamp ownership is not normalized; invalid restamping propagates an error before commit. |
| Distinct authoritative sender/recipient archive IDs; unread once | [transactional MAM](../../crates/waddle-xmpp/src/mam/storage/sqlx_store/tx_write.rs), [XEP-0313 ingress archive suite](../../crates/waddle-server/tests/xep0313_ingress_archive.rs), recipient drift tests above | Recipient preparation and receipts commit before processed direct delivery. A retry retains canonical archive identity and does not repeat recipient persistence. |
| Existing projection integrity | Transactional MAM and XEP-0313 suite above | Immutable columns and typed XML element content must match. Namespace-prefix spelling and attribute order are not message identity; literal transport replay bytes have their own row below. Timestamp equality follows SQLite's stored precision and PostgreSQL's encoded timestamp precision. Authoritative tombstones and missing-row repair retain their semantics. |
| Deferred system-message archives retain their original projection | [deferred archive execution](../../crates/waddle-server/src/ingress/execute_archive.rs), [pin retry tests](../../crates/waddle-server/src/ingress/room_pin_tests.rs) | Rebuild from the matching recorded system-broadcast payload and archive identity/time. A later nickname or regenerated description cannot replace the archived message. |
| Recover a committed unarchived storable full-JID route | [recovery reconstruction](../../crates/waddle-server/src/ingress/recovery_rebuild.rs), [prepared-route regressions](../../crates/waddle-server/src/ingress/recovery_prepared_direct_tests.rs), [legacy recovery tests](../../crates/waddle-server/src/ingress/recovery_executor_tests.rs) | Prepared payload evidence distinguishes storable routes from legacy delegation. Only recorded targets and payloads may execute. Protected `no-store` messages retain no prepared copy and have no ingress recovery obligation. Size limits before `h` and the existing bare-JID policy remain enforced. |
| Carbons and notification effects | [local carbon retries](../../crates/waddle-server/src/ingress/execute_local_carbons_tests.rs), [offline settlement](../../crates/waddle-server/src/ingress/offline_settlement_tests.rs), [relayed carbon authority](../../crates/waddle-server/src/ingress/append_authority_carbons.rs), [owner-receiver regressions](../../crates/waddle-server/src/ingress/execute_carbon_fanout_tests.rs) | Exact recorded audiences and transactional keyed effects. A relayed carbon claim must carry the relay-wide receipt and is verified against the canonical sender, intent and message before keyed custody; every target keeps its own resource authorization. Provider delivery without a durable provider key remains at-least-once. |
| One authorized keyed SM custody allocation | [keyed append regressions](../../crates/waddle-xmpp/src/stream_management/session_registry/tests/keyed_append.rs), [SQL custody regressions](../../crates/waddle-server/src/sm_persistence/ingress_append_tests.rs) | Durable append proof survives replay-cache eviction, resume and acknowledgement. An unacknowledged frame may legitimately be retransmitted. An already-accepted frame whose obligation fails authorization during detach retains unkeyed custody, without keyed deduplication. |
| Durable ordinal width and wrapping wire count | [typed ordinals](../../crates/waddle-xmpp/src/ingress/ordinal.rs), [substrate round-trip tests](../../crates/waddle-server/src/ingress_substrate/mod.rs), [authority wrap tests](../../crates/waddle-server/src/ingress_substrate/authority_tests.rs) | Durable `u64` and wire `u32` are distinct types; durable frontier allocation does not derive its epoch from wrapping wire `h`. |
| Replay bytes and original delay | [byte contract](../../crates/waddle-xmpp/src/stream_management/replay.rs), [durable byte suite](../../crates/waddle-server/tests/xep0198_replay_bytes.rs), [cross-node registry suite](../../crates/waddle-server/tests/xep0198_cross_node_resume.rs), [XEP-0198 delay suite](../../crates/waddle-xmpp/tests/xep0198_resume_replay_delay.rs) | The immutable bytes are production serializer output, not client-original lexical XML. Fresh registry hydration preserves them; first insertion of a missing server delay deliberately changes the frame. Repeated replay preserves the resulting bytes and original time. The cross-node proof uses fenced storage/registry transitions, not a remote relay or WebSocket handshake. |
| Unknown started live send | [live delivery tests](../../crates/waddle-server/src/ingress/live_delivery_tests.rs), [lease takeover tests](../../crates/waddle-server/src/ingress_uow/send_attempts_tests.rs) | Database-clock grace suppresses retries until expiry; replacement owners use fresh tokens. Durable proof suppresses repetition; absent proof permits another attempt, including non-SM sends. |
| Never-started reservation and offline handoff | [ambiguous/expired handoff tests](../../crates/waddle-server/src/ingress/execute_ambiguous_offline_tests.rs) | Expired reservations can recover. Pending custody requires permitted storage policy and clear sibling authority; it is not evidence of successful socket delivery. |
| Honor no-store without retaining a replayable ledger payload | [storage-hint policy](../../crates/waddle-server/src/ingress/storage_hint.rs), prepared-route regressions above | Full-JID, non-self `no-store` messages without `store` commit a terminal `storage_hint_forbids_handoff` receipt. The initial live/SM attempt is allowed; only the existing bounded SM buffer may replay it. Crash-before-send and absent recipients permit loss; retry, maintenance and later rebind never resurrect the message. No recipient-absence observation or bind fence is involved. |
| Preserve original time through remote delivery | [remote receiver regressions](../../crates/waddle-server/src/clustering/route_bridge/tests/transient_timestamp.rs), [wire compatibility regressions](../../crates/waddle-server/src/clustering/relay/remote_resource_compat/tests/transient_timestamp.rs) | Timestamp-only metadata reaches registered remote socket queues, ordered owners and detached SM custody. It is signed and fingerprinted with the ordered payload, creates no ingress append authority, and cannot override keyed custody time. Versioned live endpoints refuse metadata-dropping fallback; frozen legacy DTOs retain their existing bytes. |
| Preserve #1316 pacing | [send-window suite](../../crates/waddle-xmpp/tests/xep0198_send_window.rs), [WebSocket batch writer](../../crates/waddle-server/src/server/routes/websocket/batch_write.rs) and its tests | Bootstrap flood, deferred-cap behavior and mid-stream pacing must remain green; keyed append must not bypass dispatch pacing. |

## Scope and remaining work

- #1759 is complete in #1896; #1776 is complete in #1899. Their original issue
  descriptions are historical, not instructions to restore superseded behavior.
- #1909–#1912 are complete in #1913: #1909 reconciled this matrix, the RFC,
  runbook, roadmap and epic wording; #1910–#1912 supplied the
  recovery/integrity/replay acceptance work described above.
- #1906 is complete in #1923: the relayed-carbons owner receiver defers
  canonical authorization to the keyed append decision. No carve-out of the
  epic remains open; only the final closure record below is outstanding.
- #1908 investigates room-observer terminalization. It is adjacent; any impact
  on the DM contract must be demonstrated rather than inferred from shared
  maintenance code. Keyless guest/provider execution can repeat under #1899.
- Legacy records lacking recoverable provenance and maintenance delivery to
  remote-hosted resources retain the limitations documented in the
  [operations runbook](ingress-authority.md). Do not claim universal recovery
  for those families or fabricate completion receipts.
- MUC archive-only sender identity and nickname generation are frozen separately
  from the live room message. Replanning uses that authority rather than the
  recipient's current join state or the row being verified. Legacy rows without
  the sidecar cannot use it to reconstruct changed private metadata: a mismatch
  fails closed rather than silently treating the stored row as its own proof.
  Likewise, if a room message arrived without a wire ID and its canonical room
  copy is lost, the server-generated original ID cannot be recreated from the
  raw client envelope. Unverifiable retries produce a typed contradiction
  without new receipts or reflections. This is not a blanket rejection of
  legacy messages whose remaining authority still proves an identical row.

## Final closure record

Before closing #1658, attach the final reviewed commit, the corresponding CI
runs (workspace, dedicated XEP suites, Clippy `-D warnings` and doctests), and
the disposition of every remaining row above to the epic. Keep any explicitly
excluded legacy or remote-only behavior in the epic's acceptance boundary.
The implementation PR's verification record supplies evidence for its own
changes; it does not establish production activation.
