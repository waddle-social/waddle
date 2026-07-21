package social.waddle.android.client

import social.waddle.android.client.prefs.DeliveryOwnerBareJid
import social.waddle.android.client.prefs.DeliveryOwnerJournal
import social.waddle.android.client.prefs.OutboundOwnership
import social.waddle.android.client.prefs.TerminalReceipt
import social.waddle.android.client.prefs.TerminalReceiptState

/** Validates the durable receipt projection before any claim or release transition. */
internal fun DeliveryOwnerJournal.validateReceiptApplication(
    requestedOwner: DeliveryOwnerBareJid,
    receipt: TerminalReceipt,
    ref: TerminalReceiptRef,
): TerminalReceiptCorruption? {
    if (receipt.owner != requestedOwner || !receipt.matches(ref) || receipt.attempt.ownerBareJid != requestedOwner.value) {
        return TerminalReceiptCorruption.RECEIPT_BINDING_MISMATCH
    }
    if (activeAttempt != null) return TerminalReceiptCorruption.ACTIVE_ATTEMPT_REMAINS
    if (terminalIntents.isNotEmpty()) return TerminalReceiptCorruption.TERMINAL_INTENTS_REMAIN
    if (outboundRows.map { it.identity }.toSet().size != outboundRows.size) return TerminalReceiptCorruption.DUPLICATE_ROW
    if (
        outboundRows.any {
            it.identity.ownerBareJid != requestedOwner.value
        }
    ) {
        return TerminalReceiptCorruption.ROW_OWNER_MISMATCH
    }
    if (
        outboundRows.any {
            it.ownership is OutboundOwnership.Terminal
        }
    ) {
        return TerminalReceiptCorruption.TERMINAL_ROW_REMAINS
    }
    if (
        outboundRows.any {
            it.ownership is OutboundOwnership.NativeOwned
        }
    ) {
        return TerminalReceiptCorruption.NATIVE_OWNED_ROW_REMAINS
    }
    val pending = receipt.state as? TerminalReceiptState.Pending ?: return null
    if (pending.effects.isEmpty()) return TerminalReceiptCorruption.EMPTY_EFFECTS
    if (pending.effects.map { it.callback }.toSet().size != pending.effects.size ||
        pending.effects.map { it.row.identity }.toSet().size != pending.effects.size
    ) {
        return TerminalReceiptCorruption.DUPLICATE_EFFECT
    }
    if (pending.effects.any { effect ->
            effect.row.identity.ownerBareJid != requestedOwner.value ||
                effect.callback.row != effect.row.identity ||
                effect.callback.attempt != receipt.attempt
        }
    ) {
        return TerminalReceiptCorruption.EFFECT_BINDING_MISMATCH
    }
    if (!pending.effects.zipWithNext().all { (first, second) -> first.row.sequence < second.row.sequence }) {
        return TerminalReceiptCorruption.REVERSED_EFFECT_ORDER
    }
    return null
}
