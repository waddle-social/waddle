package social.waddle.android.client

/** Which timeline a message belongs to, and whether the account sent it. */
internal data class ConversationKey(val jid: String, val isMine: Boolean)

/**
 * Conversation routing shared by the timeline, DM, and unread stores:
 * groupchat messages key on the room bare JID; chat/normal messages key
 * on the peer (the non-own side). For 1:1 chats `isMine` compares the
 * sender bare JID against the account bare JID; MUC echoes of own
 * messages come from `room/nick`, so they compare the occupant resource
 * against [ownNick] (the localpart used at join).
 */
internal fun conversationKeyOf(
    ownBareJid: String?,
    ownNick: String?,
    from: String?,
    to: String?,
    isGroupchat: Boolean,
): ConversationKey? {
    val fromBare = from?.let(::bareJid)
    val toBare = to?.let(::bareJid)
    val isMine = if (isGroupchat) {
        ownNick != null && from != null && resourcepart(from) == ownNick
    } else {
        ownBareJid != null && fromBare == ownBareJid
    }
    val conversation = when {
        isGroupchat -> fromBare ?: toBare
        isMine -> toBare
        else -> fromBare ?: toBare
    } ?: return null
    return ConversationKey(conversation, isMine)
}
