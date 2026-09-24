package social.waddle.android.client

import social.waddle.android.client.prefs.QueuedOutboundMessage
import java.util.Locale

/** The core validated the JIDs. Compare account parts without folding resources. */
internal fun XmppEvent.MessageRejected.matches(message: QueuedOutboundMessage): Boolean {
    if (stanzaId != message.clientStanzaId) return false
    val owner = bareJid(message.ownerBareJid).lowercase(Locale.ROOT)
    if (to != null && bareJid(to).lowercase(Locale.ROOT) != owner) return false
    val sender = bareJid(from).lowercase(Locale.ROOT)
    val recipient = bareJid(message.conversationJid).lowercase(Locale.ROOT)
    val senderResource = resourcepart(from)
    val serviceDomain = sender == owner.substringAfter('@') || sender == recipient.substringAfter('@')
    if (senderResource == null && '@' !in sender && serviceDomain) return true
    if (sender != recipient) return false
    if (message.isGroupchat) return senderResource == null
    val recipientResource = resourcepart(message.conversationJid)
    // Full destinations can be MUC occupants: another occupant never owns
    // this send. A bare room/service can still reject its routing attempt.
    return recipientResource == null || senderResource == null || senderResource == recipientResource
}
