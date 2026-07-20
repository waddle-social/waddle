package social.waddle.android.client

import social.waddle.android.client.prefs.DeliveryJournalMutation
import social.waddle.android.client.prefs.DeliveryOwnerBareJid
import social.waddle.android.client.prefs.SessionPrefs

/** DataStore transaction boundaries for the pure terminal receipt transitions. */
internal suspend fun SessionPrefs.discoverTerminalReceipt(
    owner: DeliveryOwnerBareJid,
): TerminalReceiptDiscovery = updateDeliveryJournal { journal ->
    DeliveryJournalMutation(journal, journal.discoverTerminalReceipt(owner))
}

internal suspend fun SessionPrefs.claimTerminalReceipt(
    request: TerminalReceiptClaimRequest,
): TerminalReceiptClaimResult = updateDeliveryJournal { journal ->
    val result = journal.claimTerminalReceipt(request)
    DeliveryJournalMutation(result.journal, result)
}

internal suspend fun SessionPrefs.acknowledgeTerminalReceipt(
    lease: TerminalReceiptLease,
): TerminalReceiptAcknowledgeResult = updateDeliveryJournal { journal ->
    val result = journal.acknowledgeTerminalReceipt(lease)
    DeliveryJournalMutation(result.journal, result)
}

internal suspend fun SessionPrefs.releaseTerminalReceipt(
    lease: TerminalReceiptLease,
): TerminalReceiptReleaseResult = updateDeliveryJournal { journal ->
    val result = journal.releaseTerminalReceipt(lease)
    DeliveryJournalMutation(result.journal, result)
}
