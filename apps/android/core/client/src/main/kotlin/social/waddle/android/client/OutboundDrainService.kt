package social.waddle.android.client

import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.android.client.prefs.DeliveryOwnerBareJid
import social.waddle.android.client.prefs.OutboundOwnership
import social.waddle.android.client.prefs.QueuedOutboundMessage
import social.waddle.client.ffi.WaddleClientInterface
import social.waddle.client.ffi.WaddleSendMessageOutcome

internal data class OutboundDrainOperation(
    val owner: DeliveryOwnerBareJid,
    val attempt: DeliveryAttemptRef,
    val client: WaddleClientInterface,
)

internal sealed interface OutboundDrainDisposition {
    data object NoReady : OutboundDrainDisposition
    data object AwaitingNativeAck : OutboundDrainDisposition
    data object RetryableReleased : OutboundDrainDisposition
    data class TerminalRequired(
        val row: QueuedOutboundMessage,
        val ownership: OutboundOwnership.NativeOwned,
    ) : OutboundDrainDisposition
}

/** Stateless one-step absolute-head drain transaction. */
internal class OutboundDrainService(
    private val journal: DeliveryJournalStore,
    private val sendService: OutboundSendService,
) {
    suspend fun drainOne(operation: OutboundDrainOperation): OutboundDrainDisposition {
        val row = journal.claimAbsoluteReadyHead(operation.owner.value, operation.attempt)
            ?: return OutboundDrainDisposition.NoReady
        val ownership = row.ownership as OutboundOwnership.NativeOwned
        return when (val outcome = sendService.sendClaimed(row, operation.client)) {
            is WaddleSendMessageOutcome.Sent -> {
                if (outcome.stanzaId == row.clientStanzaId) {
                    OutboundDrainDisposition.AwaitingNativeAck
                } else {
                    OutboundDrainDisposition.TerminalRequired(row, ownership)
                }
            }
            WaddleSendMessageOutcome.NotConnected,
            WaddleSendMessageOutcome.TransportError,
            -> {
                journal.release(row.identity, ownership)
                OutboundDrainDisposition.RetryableReleased
            }
            else -> OutboundDrainDisposition.TerminalRequired(row, ownership)
        }
    }
}
