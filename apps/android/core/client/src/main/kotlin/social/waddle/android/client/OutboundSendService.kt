package social.waddle.android.client

import kotlinx.coroutines.CancellationException
import social.waddle.android.client.DeliveryJournalStore.EnqueueResult
import social.waddle.android.client.DeliveryJournalStore.LiveAdmissionResult
import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.android.client.prefs.DeliveryOwnerBareJid
import social.waddle.android.client.prefs.DeliverySource
import social.waddle.android.client.prefs.OutboundOwnership
import social.waddle.android.client.prefs.QueuedOutboundContent
import social.waddle.android.client.prefs.QueuedOutboundDraft
import social.waddle.android.client.prefs.QueuedOutboundMessage
import social.waddle.android.client.prefs.QueuedOutboundPayload
import social.waddle.android.client.prefs.QueuedOutboundTarget
import social.waddle.android.client.store.TimelineStore
import social.waddle.client.ffi.WaddleClientInterface
import social.waddle.client.ffi.WaddleSendMessageOutcome

internal data class OutboundSendRequest(
    val target: QueuedOutboundTarget,
    val content: QueuedOutboundContent,
    val source: DeliverySource,
)

/** The lifecycle façade issues one ephemeral admission for one send operation. */
internal sealed interface OutboundSendAdmission {
    val owner: DeliveryOwnerBareJid

    data class Offline(override val owner: DeliveryOwnerBareJid) : OutboundSendAdmission

    data class Live(
        override val owner: DeliveryOwnerBareJid,
        val attempt: DeliveryAttemptRef,
        val client: WaddleClientInterface,
    ) : OutboundSendAdmission
}

internal sealed interface OutboundSendDisposition {
    data class Completed(val result: SendResult) : OutboundSendDisposition
    data class Queued(val result: SendResult) : OutboundSendDisposition
    data class TerminalRequired(
        val row: QueuedOutboundMessage,
        val ownership: OutboundOwnership.NativeOwned,
        val wireOutcome: WaddleSendMessageOutcome,
    ) : OutboundSendDisposition
}

/** Stateless durable-send transaction boundary. */
internal class OutboundSendService(
    private val journal: DeliveryJournalStore,
    private val timelineStore: TimelineStore,
) {
    suspend fun send(
        request: OutboundSendRequest,
        admission: OutboundSendAdmission,
    ): OutboundSendDisposition {
        val draft = QueuedOutboundDraft.create(
            ownerBareJid = admission.owner.value,
            clientStanzaId = newClientStanzaId(),
            enqueuedAtMillis = System.currentTimeMillis(),
            payload = QueuedOutboundPayload(request.target, request.content),
            source = request.source,
        )
        return when (admission) {
            is OutboundSendAdmission.Offline -> OutboundSendDisposition.Completed(enqueueOffline(draft))
            is OutboundSendAdmission.Live -> sendLive(draft, admission)
        }
    }

    private suspend fun enqueueOffline(draft: QueuedOutboundDraft): SendResult {
        val enqueue = persistQueueMutation { journal.enqueueReady(draft) }
            ?: return SendResult(WaddleSendMessageOutcome.Error)
        val stored = when (enqueue) {
            is EnqueueResult.Stored -> enqueue.row
            is EnqueueResult.Conflict,
            EnqueueResult.CapacityExhausted,
            EnqueueResult.StaleAttempt,
            -> return SendResult(WaddleSendMessageOutcome.Error)
        }
        return SendResult(
            outcome = WaddleSendMessageOutcome.NotConnected,
            delivery = DeliveryOutcomeRef(stored.identity, stored.source),
        )
    }

    private suspend fun sendLive(
        draft: QueuedOutboundDraft,
        admission: OutboundSendAdmission.Live,
    ): OutboundSendDisposition {
        val result = persistQueueMutation {
            journal.enqueueAndClaimAbsoluteHead(draft, admission.attempt)
        } ?: return OutboundSendDisposition.Completed(SendResult(WaddleSendMessageOutcome.Error))
        return when (result) {
            is LiveAdmissionResult.Claimed -> {
                val ownership = result.row.ownership as? OutboundOwnership.NativeOwned
                    ?: return OutboundSendDisposition.Completed(SendResult(WaddleSendMessageOutcome.Error))
                reconcileInitialOutcome(result.row, ownership, sendClaimed(result.row, admission.client))
            }
            is LiveAdmissionResult.Queued -> {
                OutboundSendDisposition.Queued(
                    SendResult(WaddleSendMessageOutcome.NotConnected, DeliveryOutcomeRef(result.row.identity, result.row.source)),
                )
            }
            is LiveAdmissionResult.Conflict,
            LiveAdmissionResult.CapacityExhausted,
            LiveAdmissionResult.StaleAttempt,
            -> OutboundSendDisposition.Completed(SendResult(WaddleSendMessageOutcome.Error))
        }
    }

    private suspend fun reconcileInitialOutcome(
        row: QueuedOutboundMessage,
        ownership: OutboundOwnership.NativeOwned,
        outcome: WaddleSendMessageOutcome,
    ): OutboundSendDisposition {
        val delivery = DeliveryOutcomeRef(row.identity, row.source)
        if (outcome is WaddleSendMessageOutcome.Sent && outcome.stanzaId == row.clientStanzaId) {
            return OutboundSendDisposition.Completed(SendResult(outcome, delivery))
        }
        if (outcome == WaddleSendMessageOutcome.NotConnected || outcome == WaddleSendMessageOutcome.TransportError) {
            return if (persistQueueMutation { journal.release(row.identity, ownership) } == true) {
                OutboundSendDisposition.Completed(SendResult(outcome, delivery))
            } else {
                OutboundSendDisposition.Completed(SendResult(WaddleSendMessageOutcome.Error))
            }
        }
        return OutboundSendDisposition.TerminalRequired(row, ownership, outcome)
    }

    internal suspend fun sendClaimed(
        queued: QueuedOutboundMessage,
        client: WaddleClientInterface,
    ): WaddleSendMessageOutcome {
        val (finalBody, options) = preparedSend(queued.clientStanzaId, queued.body, queued.sendExtras())
        val outcome = try {
            if (queued.isGroupchat) client.sendGroupchatMessage(queued.conversationJid, finalBody, options)
            else client.sendChatMessage(queued.conversationJid, finalBody, options)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            WaddleSendMessageOutcome.TransportError
        }
        if (!queued.isGroupchat && outcome is WaddleSendMessageOutcome.Sent) {
            timelineStore.onLiveMessage(
                ownDmEcho(
                    ownJid = queued.ownerBareJid,
                    peerJid = queued.conversationJid,
                    stanzaId = queued.clientStanzaId,
                    body = finalBody,
                    options = options,
                ),
            )
        }
        return outcome
    }

    private suspend fun <T> persistQueueMutation(block: suspend () -> T): T? = try {
        block()
    } catch (cancellation: CancellationException) {
        throw cancellation
    } catch (_: Throwable) {
        null
    }
}
