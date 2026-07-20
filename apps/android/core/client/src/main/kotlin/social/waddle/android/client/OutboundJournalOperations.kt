package social.waddle.android.client

import social.waddle.android.client.prefs.DeliveryAttemptRef
import social.waddle.android.client.prefs.DeliveryJournal
import social.waddle.android.client.prefs.DeliveryJournalMutation
import social.waddle.android.client.prefs.DeliveryOwnerJournal
import social.waddle.android.client.prefs.NativeOutboundPhase
import social.waddle.android.client.prefs.OutboundOwnership
import social.waddle.android.client.prefs.QueuedOutboundMessage
import social.waddle.android.client.prefs.SmResumeSnapshot
import social.waddle.android.client.prefs.SmResumeStanzaKind
import social.waddle.android.client.prefs.SmResumeXmlToken
import social.waddle.android.client.prefs.TerminalReceiptState

internal fun DeliveryJournal.beginDeliveryAttempt(
    ownerBareJid: String,
    replacement: DeliveryAttemptRef,
    nowMillis: Long,
): DeliveryJournalMutation<DeliveryJournalStore.BeginAttemptResult> {
    check(activeOwnerBareJid == ownerBareJid) {
        "cannot begin a delivery attempt for an inactive owner"
    }
    val owner = owners[ownerBareJid] ?: DeliveryOwnerJournal()
    val terminalGate = owner.advancePastTerminalReceipt()
    if (terminalGate is TerminalReceiptAttemptGate.Blocked) {
        return DeliveryJournalMutation(this, terminalGate.result)
    }
    val gatedOwner = (terminalGate as TerminalReceiptAttemptGate.Ready).owner
    val snapshot = gatedOwner.sm.snapshot
        ?.takeIf { gatedOwner.sm.version > gatedOwner.sm.tombstoneVersion }
    val resumeIds = snapshot?.messageStanzaIds().orEmpty()
    val reconciledRows = gatedOwner.outboundRows.map { row ->
        when {
            row.ownership is OutboundOwnership.Terminal -> row
            row.clientStanzaId in resumeIds -> row.copy(
                ownership = OutboundOwnership.NativeOwned(
                    attempt = replacement,
                    phase = NativeOutboundPhase.RESUME,
                ),
            )
            row.ownership is OutboundOwnership.NativeOwned ->
                row.copy(ownership = OutboundOwnership.Ready)
            else -> row
        }
    }
    val consumedSm = gatedOwner.sm.copy(
        tombstoneVersion =
            if (snapshot == null) gatedOwner.sm.tombstoneVersion else gatedOwner.sm.version,
        writerAttempt = replacement.takeIf { gatedOwner.sm.version > 0 },
        snapshot = null,
    )
    val nextOwner = gatedOwner.copy(
        activeAttempt = replacement,
        sm = consumedSm,
        outboundRows = reconciledRows,
    ).gcTransitionReceipts(nowMillis)
    return DeliveryJournalMutation(
        journal = withOwner(ownerBareJid, nextOwner),
        result = DeliveryJournalStore.BeginAttemptResult.Started(
            DeliveryJournalStore.AttemptBootstrap(
                attempt = replacement,
                resumeSnapshot = snapshot,
                smVersion = consumedSm.version,
            ),
        ),
    )
}

private sealed interface TerminalReceiptAttemptGate {
    data class Ready(val owner: DeliveryOwnerJournal) : TerminalReceiptAttemptGate
    data class Blocked(val result: DeliveryJournalStore.BeginAttemptResult) : TerminalReceiptAttemptGate
}

/**
 * A terminal tombstone belongs to the old attempt. A fresh attempt may clear
 * it only after the fence has removed every mutable terminal projection.
 */
private fun DeliveryOwnerJournal.advancePastTerminalReceipt(): TerminalReceiptAttemptGate {
    val receipt = terminalReceipt ?: return TerminalReceiptAttemptGate.Ready(this)
    val ref = TerminalReceiptRef(receipt.owner, receipt.attempt, receipt.id)
    return when (val state = receipt.state) {
        is TerminalReceiptState.Pending -> TerminalReceiptAttemptGate.Blocked(
            DeliveryJournalStore.BeginAttemptResult.PendingReceipt(ref, state.claim),
        )
        is TerminalReceiptState.Acknowledged,
        TerminalReceiptState.PreAcknowledged,
        -> if (
            activeAttempt == null &&
            terminalIntents.isEmpty() &&
            outboundRows.none {
                it.ownership is OutboundOwnership.Terminal ||
                    it.ownership is OutboundOwnership.NativeOwned
            }
        ) {
            TerminalReceiptAttemptGate.Ready(copy(terminalReceipt = null))
        } else {
            TerminalReceiptAttemptGate.Blocked(
                DeliveryJournalStore.BeginAttemptResult.TombstoneNotPostFence(ref, state),
            )
        }
    }
}

internal fun DeliveryOwnerJournal.absoluteHeadOrThrow(
    ownerBareJid: String,
): QueuedOutboundMessage? {
    check(outboundRows.all { it.ownerBareJid == ownerBareJid }) {
        "delivery journal row owner does not match its bucket"
    }
    check(outboundRows.map { it.sequence }.toSet().size == outboundRows.size) {
        "delivery journal contains duplicate delivery sequences"
    }
    val maximum = outboundRows.maxOfOrNull { it.sequence }
    check(maximum == null || nextSequence > maximum) {
        "next delivery sequence must exceed every persisted row"
    }
    return outboundRows.minByOrNull { it.sequence }
}

internal fun DeliveryOwnerJournal.allocateSequenceOrThrow(
    ownerBareJid: String,
): Long {
    absoluteHeadOrThrow(ownerBareJid)
    check(nextSequence < Long.MAX_VALUE) {
        "delivery sequence exhausted"
    }
    return nextSequence
}

internal fun DeliveryJournal.withOwner(
    ownerBareJid: String,
    owner: DeliveryOwnerJournal,
): DeliveryJournal = copy(owners = owners + (ownerBareJid to owner))

private fun SmResumeSnapshot.messageStanzaIds(): Set<String> =
    queuedEntries.mapNotNull { entry ->
        if (entry.stanza.stanzaKind != SmResumeStanzaKind.MESSAGE) {
            return@mapNotNull null
        }
        val root = entry.stanza.tokens.firstOrNull() as? SmResumeXmlToken.Start
            ?: return@mapNotNull null
        if (root.name.namespace != "jabber:client" || root.name.localName != "message") {
            return@mapNotNull null
        }
        root.attributes.firstOrNull { attribute ->
            attribute.name.namespace.isEmpty() &&
                attribute.name.localName == "id"
        }?.value
    }.toSet()
