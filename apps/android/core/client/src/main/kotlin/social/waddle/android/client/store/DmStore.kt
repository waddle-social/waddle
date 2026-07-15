package social.waddle.android.client.store

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import social.waddle.android.client.bareJid
import social.waddle.client.ffi.WaddleMessage

/**
 * Recent DM peers, most-recent-first: seeded from persisted last-seen
 * conversations at login, then reordered by live chat traffic.
 */
class DmStore {
    private val lock = Any()
    private val recency = LinkedHashSet<String>()

    private val _peers = MutableStateFlow<List<String>>(emptyList())

    /** Peer bare JIDs, most recently active first. */
    val peers: StateFlow<List<String>> = _peers.asStateFlow()

    /** Seed known peers (oldest-to-newest); live traffic reorders them. */
    fun seed(peerJids: Collection<String>) {
        synchronized(lock) {
            peerJids.forEach { recency.add(bareJid(it)) }
            publish()
        }
    }

    fun onChatMessage(ownBareJid: String?, message: WaddleMessage) {
        if (message.isMuc || message.messageType != "chat") return
        val fromBare = message.from?.let(::bareJid)
        val toBare = message.to?.let(::bareJid)
        val peer = (if (fromBare == ownBareJid) toBare else fromBare) ?: return
        if (peer == ownBareJid) return
        synchronized(lock) {
            recency.remove(peer)
            recency.add(peer)
            publish()
        }
    }

    fun clear() {
        synchronized(lock) {
            recency.clear()
            publish()
        }
    }

    private fun publish() {
        _peers.value = recency.toList().asReversed()
    }
}
