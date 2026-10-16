package social.waddle.android.service

import social.waddle.client.ffi.WaddleMessage

/**
 * Which messages already raised a notification, by XEP-0359 stanza-id,
 * so the live path and the push path never announce the same message
 * twice, whichever arrives first.
 *
 * A push names the archived message's stanza-id ([PushSummary.item]); a
 * live message carries one per archiving entity. The XEP-0160 offline
 * flush replays a message that push announced while the device was
 * offline, so it must be recognised by id. Its XEP-0203 `<delay/>` alone
 * proves nothing: the server also stamps one on XEP-0198 redeliveries to
 * a sibling resource, which push never announced.
 *
 * Both claims are atomic check-and-record operations, so a push and the
 * live copy of the same message racing each other produce one banner.
 */
class AnnouncedItems(private val capacity: Int = DEFAULT_CAPACITY) {
    private val lock = Any()
    private val live = LinkedHashSet<String>()
    private val pushed = LinkedHashSet<String>()

    /**
     * The live path takes [message]: `false` when push already announced
     * it, otherwise its ids are recorded as handled live.
     */
    fun claimLive(message: WaddleMessage): Boolean {
        val ids = idsOf(message)
        return synchronized(lock) {
            if (ids.any { it in pushed }) return@synchronized false
            remember(live, ids)
            true
        }
    }

    /**
     * The push path takes [item]: `false` when the live path already
     * handled it, otherwise it is recorded as announced by push.
     */
    fun claimPush(item: String): Boolean = synchronized(lock) {
        if (item in live || item in pushed) return@synchronized false
        remember(pushed, listOf(item))
        true
    }

    /** Whether the live path already handled the message with stanza-id [item]. */
    fun handledLive(item: String): Boolean = synchronized(lock) { item in live }

    /** Sign-out: forget everything. */
    fun clear() = synchronized(lock) {
        live.clear()
        pushed.clear()
    }

    private fun remember(into: LinkedHashSet<String>, ids: Collection<String>) {
        into += ids
        while (into.size > capacity) into.remove(into.first())
    }

    private fun idsOf(message: WaddleMessage): Set<String> =
        (message.stanzaIds.map { it.id } + listOfNotNull(message.stanzaId)).toSet()

    private companion object {
        const val DEFAULT_CAPACITY = 256
    }
}
