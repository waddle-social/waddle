# Extension effect acceptance (#1660)

This matrix extends the completed direct-message executor contract. Canonical
effect receipts remain the sole completion authority; scheduling ancestry and
PubSub projection watermarks do not attest provider delivery.

| Requirement | Implementation and regression seam | Boundary |
| --- | --- | --- |
| Opaque, target-scoped keys | [Foundation bindings](../../crates/waddle-server/src/ingress_uow/effect_descendants.rs), [key derivation](../../crates/waddle-xmpp/src/ingress/keys.rs), substrate tests | Binding validates the recorded effect and target. Calls/pins return `AwaitingDurableOwner`; operational mutation receipts do not imply P1.4 durable ownership. |
| Atomic activity/preview acceptance | [projection executor](../../crates/waddle-server/src/ingress/execute_projection.rs), [projection tests](../../crates/waddle-server/src/ingress/execute_projection_tests.rs) | SQLite/PostgreSQL mutation, key binding and receipt roll back together. Completed replay preserves newer activity, including a newer gone state. |
| Approved preview restitution | [preview restoration](../../crates/waddle-server/src/ingress/recorded/preview_restore.rs), recorded preview/recovery tests | A retry cannot introduce freshly enriched slots; original pending slots and payloads recover from stored authority. Archive revisions fence older pointer updates. |
| Host-owned extension capability | [WIT ABI 3](../../wit/waddle-extension.wit), [runtime tests](../../crates/waddle-extensions/src/runtime/tests.rs), [server capability](../../crates/waddle-server/src/room_observation/capability.rs) | Guests receive borrowed resources, not exported keys. Runtime and server revalidate source, generation and unexpired lease. Forged/retained handles fail. |
| Durable observer output | [observer work](../../crates/waddle-server/src/ingress_uow/room_observation/work.rs), observer invocation/publication/retention suites | Approved results and publication descendants commit together. Persistence retries reuse the approved result without invoking the provider again. Unknown started work remains retryable past the former attempt cap. |
| Scoped notification custody | [candidate lineage](../../crates/waddle-server/src/notification_outbox/enqueue.rs), [ancestry tests](../../crates/waddle-server/src/notification_outbox/ancestry_tests.rs) | Fanout/coalescing preserves every parent. Explicit cancellation releases custody. Wire item IDs, even identical copies, confer no canonical authority. |
| Same-key durable provider queue | [queue acceptance](../../crates/waddle-server/src/push_service/publish_jobs.rs), queue/device/provider suites | Canonical acceptance freezes payload/options and retries the same job. Wire publications create separate jobs and retain their original item IDs. Per-device evidence is job-scoped. Keyless providers remain at-least-once. |
| Ordered PubSub backing | [versioned backing](../../crates/waddle-server/src/pubsub/versioned.rs), [SQL tests](../../crates/waddle-server/src/pubsub/versioned_tests.rs), memory storage test | Atomic revision/token/fingerprint and item writes fence delayed old retries across distinct databases. Superseded backing is not provider acceptance. Watermarks survive deletion without retaining notification text. Existing PubSub v8 data is preserved. |
| XEP-0357 item identity | [notification publication tests](../../crates/waddle-server/tests/notification_outbox/publish.rs), dedicated XEP-0357/0060 suites | Stable item ID proves neither canonical authority nor provider delivery. Ordinary wire republication remains a fresh publication. |
| Descendant/reference retention | [retention frontier](../../crates/waddle-server/src/ingress_substrate/retention.rs), substrate/dispatch/observer tests | Aliases, keys and receipts survive until all descendants and SM/pending custody settle, then the full eight-day tail. Relay ACK does not settle ancestry. New references invalidate eligibility under canonical locks. |
| Upgrade and concurrency | V1025, [legacy adoption](../../crates/waddle-server/src/notification_outbox/legacy_ancestry.rs), upgrade/NOWAIT suites | Additional parent locks are nonblocking and whole-transaction retries retain sanitized classes. Legacy adoption preserves frozen data and refuses to invent provider completion. Unreconstructable history remains conservatively retained. |
| Identity-free transitions | Projection, observer, notification and provider transition code | No new key, JID, plugin identity or content fields are emitted by these transitions. Projection fingerprints remain storage metadata. |

Verification and independent review are recorded on [PR #1939](https://github.com/waddle-social/waddle/pull/1939)
against its final commit. A source link or passing focused test does not assert
deployment activation. The rollout uses the [#1660 cutover](../../../docs/operations/relay-cutovers.md#extension-effect-authority-cutover-1660):
stop old collectors, apply V1025 and rebuild ABI 3 guests before restoring rolling
updates through the existing guard.
