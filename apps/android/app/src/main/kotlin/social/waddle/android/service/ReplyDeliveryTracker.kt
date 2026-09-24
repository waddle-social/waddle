package social.waddle.android.service

/** Keep reply outcomes across the send continuation and a later stanza error. */
internal class ReplyDeliveryTracker {
    private enum class State { PENDING, ACKNOWLEDGED, FAILED, REJECTED }

    private data class Reply(val target: Pair<String, Boolean>?, val state: State)

    private val replies = linkedMapOf<String, Reply>()

    /** True when a failure already arrived before the reply was registered. */
    @Synchronized
    fun track(id: String, conversation: String, isGroupchat: Boolean): Boolean {
        val state = replies[id]?.state ?: State.PENDING
        remember(id, Reply(conversation to isGroupchat, state))
        return state == State.FAILED || state == State.REJECTED
    }

    @Synchronized
    fun acknowledge(id: String) {
        val reply = replies[id] ?: Reply(null, State.PENDING)
        if (reply.state != State.REJECTED) remember(id, reply.copy(state = State.ACKNOWLEDGED))
    }

    /** Events reach this tracker only after the manager validates rejection. */
    @Synchronized
    fun fail(id: String, rejected: Boolean): Pair<String, Boolean>? {
        val reply = replies[id] ?: Reply(null, State.PENDING)
        if (!rejected && reply.state == State.ACKNOWLEDGED) return null
        val state = if (rejected || reply.state == State.REJECTED) State.REJECTED else State.FAILED
        remember(id, reply.copy(state = state))
        return reply.target
    }

    @Synchronized
    fun clear() = replies.clear()

    private fun remember(id: String, reply: Reply) {
        replies[id] = reply
        while (replies.size > MAX_REPLIES) replies.remove(replies.keys.first())
    }

    private companion object {
        const val MAX_REPLIES = 256
    }
}
