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

/**
 * Live room ownership, decided at ingest: the sending occupant's
 * disclosed real JID wins when there is one ([authorJid]); otherwise the
 * nick comparison in [key] (made against our ACTUAL occupant nick — see
 * [liveOwnNickOf]) stands.
 */
internal fun ConversationKey.withLiveAuthor(
    isGroupchat: Boolean,
    authorJid: String?,
    ownBareJid: String?,
): ConversationKey =
    if (isGroupchat && authorJid != null) copy(isMine = authorJid == ownBareJid?.let(::normalizedBareJid)) else this

/**
 * The nick our live room reflections come from: our actual occupant nick
 * in the sender's room (self-presence, incl. a XEP-0045 210 rename), else
 * the configured join nick when the room has not told us yet.
 */
internal fun liveOwnNickOf(from: String?, configuredNick: String?, actualNickIn: (String) -> String?): String? =
    from?.let { actualNickIn(bareJid(it)) } ?: configuredNick
