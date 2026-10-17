# Senderless SM promotion audit

This audit covers the producers in the repository, not historical production rows.
It does not establish which producer populated the queue reported in #1427.

| Producer | Outer message shape | Terminal recovery |
| --- | --- | --- |
| `archive_inbox_upload::handle_inbox_query_iq` | Senderless normal message: inbox `entry`, optional MAM `result` carrying the last archived message, Waddle inbox `metadata` | A query-scoped snapshot, reconstructed by querying the inbox again; do not create offline work after alternate delivery fails. |
| `groupchat_archive::push_inbox_update_to` (also reached by direct inbox updates) | Senderless headline message with Waddle inbox `push(entry, metadata)` | A current inbox projection, reconstructed by querying the inbox again; do not create offline work after alternate delivery fails. |
| MAM query responses (`build_result_messages`) | Normal message with one valid MAM result envelope; legacy message-form `fin` also recognized | Query-scoped recovery frames, excluded by `is_mam_query_response_message`. The archived inner peer message is not independently promoted from the envelope. |
| Other account/server messages without `from` | Any shape outside the precise classes above | Use ordinary offline classification. A body-bearing senderless message remains storable when capacity exists. At quota rejection, it has no peer return target and is retired as non-promotable without a bounce warning or quota-rejection increment. |

The inbox builders live in `waddle-xmpp/src/xep/xep0430.rs`. SM promotion
matches their outer shapes rather than treating every absent `from` as disposable.
Body, subject, thread, an explicit sender, or unrelated payloads prevent the inbox
projection match. Inbox query replies containing an embedded MAM result have
additional inbox payloads, so the single-envelope MAM guard alone cannot identify
them.

The #460 MAM filter was rechecked: `waddle-xmpp-core/src/mam.rs` requires a normal
message without bodies, subjects, or thread, and exactly one valid MAM envelope.
`sm_promotion::tests` exercises result and legacy fin rejection and preserves a
peer DM carrying a MAM extension. The new inbox tests call the actual query/push
builders, including query replies both with and without the last-message result.

## Quota return custody

A local offline sender now receives a typed `service-unavailable` error in its own
pending queue. Insertion uses the original stream and sequence through
`insert_promotion` with the immutable captured source fence: clustered stores
commit source retirement with
the error insertion under the source claim; portable stores commit the transfer
receipt with the error. A retry or restart therefore cannot enqueue another
error for the same source. Quota/storage/fence failure retains the original.
Remote senders still require S2S and retain original custody when no live sink
accepts the error.

XEP-0160 recommends that error messages SHOULD NOT be stored offline. This is a
narrow deliberate exception for an already accepted XEP-0198 message's terminal
rejection: keeping an error until its local sender reconnects provides durable
return custody and prevents repeated attempts at delivering the rejected original.
It does not enable ordinary offline storage for inbound error messages, alter the
stanza-error wire format, or bypass the sender's configured quota. If both queues
are full, the original remains available for the existing bounded recovery policy.

The quota counter records successful rejection handoff, rather than every failed
retry. Age-out remains governed by the ordinary pending-delivery retention policy.
Historical-row attribution and post-deployment metric observation require an
operational follow-up to #1427.
