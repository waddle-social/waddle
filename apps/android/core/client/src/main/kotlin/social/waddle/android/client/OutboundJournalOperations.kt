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

internal fun DeliveryJournal.beginDeliveryAttempt(
    ownerBareJid: String,
    replacement: DeliveryAttemptRef,
    nowMillis: Long,
): DeliveryJournalMutation<OutboundQueue.AttemptBootstrap> {
    check(activeOwnerBareJid == ownerBareJid) {
        "cannot begin a delivery attempt for an inactive owner"
    }
    val owner = owners[ownerBareJid] ?: DeliveryOwnerJournal()
    val snapshot = owner.sm.snapshot
        ?.takeIf { owner.sm.version > owner.sm.tombstoneVersion }
    val resumeIds = snapshot?.messageStanzaIds().orEmpty()
    val reconciledRows = owner.outboundRows.map { row ->
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
    val consumedSm = owner.sm.copy(
        tombstoneVersion =
            if (snapshot == null) owner.sm.tombstoneVersion else owner.sm.version,
        writerAttempt = replacement.takeIf { owner.sm.version > 0 },
        snapshot = null,
    )
    val nextOwner = owner.copy(
        activeAttempt = replacement,
        sm = consumedSm,
        outboundRows = reconciledRows,
    ).gcTransitionReceipts(nowMillis)
    return DeliveryJournalMutation(
        journal = withOwner(ownerBareJid, nextOwner),
        result = OutboundQueue.AttemptBootstrap(
            attempt = replacement,
            resumeSnapshot = snapshot,
            smVersion = consumedSm.version,
        ),
    )
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
