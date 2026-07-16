package social.waddle.android.feature.conversation

import social.waddle.android.client.MessageSendExtras
import social.waddle.android.client.SendResult
import social.waddle.android.client.VerbResult
import social.waddle.client.ffi.WaddleChatState
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
     * [extras] carry XEP-0461 reply / XEP-0201 thread annotations.
     */
    suspend fun send(body: String, extras: MessageSendExtras? = null): SendResult

    /**
     * The user is looking at the conversation: persist recency so the
     * DM list survives restarts. No-op for channels (their list comes
     * from the topology, not recency).
     */
    fun recordConversationSeen() {}

    /**
     * XEP-0085: best-effort, live-session-only typing notification
     * (a stale state must never queue for replay).
     */
    suspend fun sendChatState(state: WaddleChatState) {}

    /**
     * The newest message is on screen and read: dispatch the XEP-0333
     * displayed marker + XEP-0490 MDS cursor (deduped downstream).
     */
    suspend fun markDisplayed() {}

    /** XEP-0444: toggle [emoji] on the account's set for [targetId]. */
    suspend fun toggleReaction(targetId: String, emoji: String): VerbResult = VerbResult.Rejected

    /** XEP-0308: replace an own message's body; [threadId] repeats
     *  the corrected message's XEP-0201 thread. */
    suspend fun sendCorrection(targetId: String, newBody: String, threadId: String?): VerbResult =
        VerbResult.Rejected

    /** XEP-0424: retract an own message. */
    suspend fun sendRetraction(targetId: String): VerbResult = VerbResult.Rejected

    /** Whether this conversation supports pinning (rooms only). */
    val canPin: Boolean get() = false

    /** `urn:waddle:pin:0` pin/unpin (rooms only). */
    suspend fun setPinned(targetId: String, pinned: Boolean): VerbResult = VerbResult.Rejected
}
