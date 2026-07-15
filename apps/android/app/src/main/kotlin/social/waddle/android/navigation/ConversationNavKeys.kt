package social.waddle.android.navigation

import social.waddle.android.jid.bareJidOf
import social.waddle.android.jid.localpartOf
import social.waddle.client.ffi.WaddleTopology

/**
 * Classify a bare conversation JID (from a notification tap) into a nav
 * key: a known topology channel or joined room opens as [WaddleNavKey.Channel],
 * everything else as [WaddleNavKey.Dm].
 */
fun conversationNavKeyFor(
    topology: WaddleTopology,
    joinedRooms: Set<String>,
    conversationJid: String,
): WaddleNavKey {
    val bare = bareJidOf(conversationJid)
    val channel = topology.channels.firstOrNull { it.roomJid == bare }
    return when {
        channel != null -> WaddleNavKey.Channel(roomJid = bare, name = channel.name)
        bare in joinedRooms -> WaddleNavKey.Channel(roomJid = bare, name = localpartOf(bare))
        else -> WaddleNavKey.Dm(peerJid = bare, name = localpartOf(bare))
    }
}
