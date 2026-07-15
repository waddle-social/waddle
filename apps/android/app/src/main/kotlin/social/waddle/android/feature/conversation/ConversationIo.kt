package social.waddle.android.feature.conversation

import social.waddle.android.client.SendResult
import social.waddle.client.ffi.WaddleMamPage

/** Transport seam of a conversation screen (MUC channel or 1:1 DM). */
interface ConversationIo {
    /** Make the conversation live (MUC join); no-op for DMs. */
    suspend fun ensureJoined() {}

    /**
     * Fetch one MAM page ending before [beforeId] (the newest page when
     * `null`); `null` result means "not connected / query failed".
     */
    suspend fun fetchHistory(maxMessages: UInt, beforeId: String?): WaddleMamPage?

    /**
     * Send [body] to the conversation; [SendResult.queuedId] is set when
     * the session manager persisted the message for offline replay.
     */
    suspend fun send(body: String): SendResult

    /**
     * The user is looking at the conversation: persist recency so the
     * DM list survives restarts. No-op for channels (their list comes
     * from the topology, not recency).
     */
    fun recordConversationSeen() {}
}
