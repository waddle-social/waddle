package social.waddle.android.feature.search

/**
 * Which archive a search runs against: one conversation, room MAM or
 * the account's personal (DM) archive.
 */
data class MessageSearchTarget(
    val conversationJid: String,
    val isGroupchat: Boolean,
)

/**
 * One rendered search match. Ephemeral by design: hits never enter the
 * timeline store, so this is a plain display projection of the archived
 * message, not a [social.waddle.android.client.store.TimelineItem].
 */
data class MessageSearchHit(
    /** MAM result id — unique within the queried archive. */
    val key: String,
    /** Display name (MUC nick / DM localpart); null when unattributed. */
    val author: String?,
    val body: String,
    /** RFC 3339 archive timestamp; formatted at render time. */
    val timestamp: String?,
)

/** The search sheet's state machine (web message-search parity). */
sealed interface MessageSearchState {
    /** No query yet (or the field was cleared). */
    data object Idle : MessageSearchState

    /** The debounced query is in flight. */
    data object Searching : MessageSearchState

    /** The archive matched at least one message. */
    data class Results(val hits: List<MessageSearchHit>) : MessageSearchState

    /** The archive answered with no displayable matches. */
    data object Empty : MessageSearchState

    /** No live session, or the archive query failed. */
    data object Failed : MessageSearchState
}
